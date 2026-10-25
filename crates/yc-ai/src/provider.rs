use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use yc_validation::{AiTranslation, ProfiledRequestedEntry, validate_ai_batch_with_profiles};

use crate::{
    AiError, Result, TranslationContext, TranslationEntry, TranslationOutcome, TranslationProgress,
    batch::{DEFAULT_CONCURRENCY, split_batches},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub batch_translation: bool,
    pub entry_context: bool,
    pub custom_prompt: bool,
    pub structured_output: bool,
    pub requires_api_key: bool,
}

#[async_trait]
pub trait TranslationProvider: Send + Sync {
    async fn test_connection(&self, cancellation: &CancellationToken) -> Result<()>;

    async fn translate_batch(
        &self,
        entries: &[TranslationEntry],
        context: &TranslationContext,
        cancellation: &CancellationToken,
    ) -> Result<Vec<AiTranslation>>;

    fn capabilities(&self) -> ProviderCapabilities;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranslationOptions {
    pub concurrency: usize,
}

impl Default for TranslationOptions {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

impl TranslationOptions {
    pub fn effective_concurrency(self) -> usize {
        self.concurrency.clamp(1, 4)
    }
}

struct ActiveJob {
    id: Uuid,
    cancellation: CancellationToken,
}

/// 1つの Provider job group だけを登録し、handle の寿命を job の寿命として扱う。
#[derive(Clone, Default)]
pub struct JobManager {
    active: Arc<Mutex<Option<ActiveJob>>>,
}

pub struct TranslationJob {
    id: Uuid,
    cancellation: CancellationToken,
    manager: Weak<Mutex<Option<ActiveJob>>>,
}

impl JobManager {
    pub fn start_job(&self) -> Result<TranslationJob> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if active.is_some() {
            return Err(AiError::JobAlreadyRunning);
        }

        let id = Uuid::new_v4();
        let cancellation = CancellationToken::new();
        *active = Some(ActiveJob {
            id,
            cancellation: cancellation.clone(),
        });
        Ok(TranslationJob {
            id,
            cancellation,
            manager: Arc::downgrade(&self.active),
        })
    }

    pub fn cancel_job(&self, id: Uuid) -> bool {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(job) = active.as_ref().filter(|job| job.id == id) else {
            return false;
        };
        job.cancellation.cancel();
        true
    }
}

impl TranslationJob {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

impl Drop for TranslationJob {
    fn drop(&mut self) {
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let mut active = manager.lock().unwrap_or_else(|poison| poison.into_inner());
        if active.as_ref().is_some_and(|job| job.id == self.id) {
            *active = None;
        }
    }
}

pub async fn translate_entries(
    provider: &dyn TranslationProvider,
    entries: &[TranslationEntry],
    context: &TranslationContext,
    cancellation: &CancellationToken,
    options: TranslationOptions,
    progress: Option<&(dyn Fn(TranslationProgress) + Send + Sync)>,
) -> Result<TranslationOutcome> {
    if cancellation.is_cancelled() {
        return Err(AiError::Cancelled);
    }

    let batches = split_batches(entries);
    let total = entries.len();
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let parallelism = options.effective_concurrency();
    let request_entries = entries;
    let mut batches = stream::iter(batches.into_iter().map(|batch| {
        let cancellation = cancellation.clone();
        let completed = Arc::clone(&completed);
        async move {
            let batch = &request_entries[batch];
            if cancellation.is_cancelled() {
                return Err(AiError::Cancelled);
            }
            let translations = provider
                .translate_batch(batch, context, &cancellation)
                .await?;
            if cancellation.is_cancelled() {
                return Err(AiError::Cancelled);
            }

            let done = completed.fetch_add(batch.len(), std::sync::atomic::Ordering::Relaxed)
                + batch.len();
            if let Some(progress) = progress {
                progress(TranslationProgress {
                    completed: done,
                    total,
                });
            }
            Ok(translations)
        }
    }))
    .buffer_unordered(parallelism);

    let mut responses = Vec::new();
    loop {
        let next = tokio::select! {
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            next = batches.next() => next,
        };
        match next {
            Some(Ok(batch_responses)) => {
                if cancellation.is_cancelled() {
                    return Err(AiError::Cancelled);
                }
                responses.extend(batch_responses);
            }
            Some(Err(error)) => return Err(error),
            None => break,
        }
    }

    if cancellation.is_cancelled() {
        return Err(AiError::Cancelled);
    }
    let requested: Vec<_> = entries
        .iter()
        .map(|entry| ProfiledRequestedEntry {
            id: entry.id,
            source: entry.source.clone(),
            format_profile: entry.format_profile,
        })
        .collect();
    let validation = validate_ai_batch_with_profiles(&requested, &responses);
    let request_order: HashMap<i64, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.id, index))
        .collect();
    let mut outcome = TranslationOutcome {
        accepted: validation.accepted,
        rejected: validation.rejected,
    };
    outcome.accepted.sort_by_key(|translation| {
        request_order
            .get(&translation.id)
            .copied()
            .unwrap_or(usize::MAX)
    });
    outcome.rejected.sort_by_key(|translation| {
        request_order
            .get(&translation.id)
            .copied()
            .unwrap_or(usize::MAX)
    });
    Ok(outcome)
}
