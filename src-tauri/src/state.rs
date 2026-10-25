use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio_util::sync::CancellationToken as AiCancellationToken;
use tracing_appender::non_blocking::WorkerGuard;
use yc_ai::{CredentialBackend, CredentialManager, CredentialStoreError, JobManager, ProviderId};
use yc_analysis::{AnalysisData, CancellationToken};
use yc_project::{OpenedProject, ProjectAssetIndex};

use crate::paths::AppPaths;

pub struct AppState {
    pub paths: AppPaths,
    pub active_project: Arc<Mutex<Option<OpenedProject>>>,
    /// Projectのファイルを読むpreview/thumbnailと削除を直列化し、rename後の再オープンを防ぐ。
    pub(crate) project_io_lock: Arc<Mutex<()>>,
    pub(crate) project_analysis_cache: Arc<Mutex<Option<CachedProjectAnalysis>>>,
    pub(crate) project_asset_index_cache: Arc<Mutex<Option<CachedProjectAssetIndex>>>,
    pub analysis_jobs: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub ai_jobs: JobManager,
    pub ai_job_tokens: Arc<Mutex<HashMap<String, AiJobControl>>>,
    pub(crate) deleting_projects: Arc<Mutex<HashSet<String>>>,
    pub credentials: Option<Arc<CredentialManager>>,
    pub exported_paths: Arc<Mutex<HashSet<PathBuf>>>,
    pub recent_lock: Arc<Mutex<()>>,
    pub(crate) preview_context: Arc<Mutex<Option<crate::commands::preview::PreviewContext>>>,
    _log_guard: Mutex<WorkerGuard>,
}

pub(crate) struct CachedProjectAnalysis {
    pub project_id: String,
    pub analyses: HashMap<i64, Arc<AnalysisData>>,
}

pub(crate) struct CachedProjectAssetIndex {
    pub project_id: String,
    pub indices: HashMap<i64, CachedUnitAssetIndex>,
}

pub(crate) struct CachedUnitAssetIndex {
    pub included_namespaces: BTreeSet<String>,
    pub index: ProjectAssetIndex,
}

/// キャンセルとDB反映を同じgateで順序付け、反映前のキャンセルでバッチが保存されないようにする。
pub struct AiJobControl {
    pub project_id: String,
    pub cancellation: AiCancellationToken,
    pub apply_gate: Arc<Mutex<()>>,
}

pub(crate) struct ProjectDeletionGuard {
    project_id: String,
    deleting_projects: Arc<Mutex<HashSet<String>>>,
}

impl Drop for ProjectDeletionGuard {
    fn drop(&mut self) {
        match self.deleting_projects.lock() {
            Ok(mut projects) => {
                projects.remove(&self.project_id);
            }
            Err(error) => {
                tracing::error!(%error, project_id = %self.project_id, "Project deletion state lock was poisoned")
            }
        }
    }
}

impl AppState {
    pub fn new(paths: AppPaths, log_guard: WorkerGuard) -> Self {
        let use_e2e_credential_store = cfg!(debug_assertions)
            && std::env::var_os("YAKUCRAFT_E2E").is_some()
            && std::env::var_os("YAKUCRAFT_E2E_USE_SYSTEM_CREDENTIAL_STORE").is_none();
        let credentials = if use_e2e_credential_store {
            // E2Eは既定でOS資格情報を避け、専用確認時だけシステムストアを使う。
            CredentialManager::new(E2eCredentialBackend::default())
                .map(Arc::new)
                .map_err(|error| {
                    tracing::error!(%error, "E2E AI credential store could not be initialized");
                })
                .ok()
        } else {
            #[cfg(windows)]
            {
                CredentialManager::windows()
                    .map_err(|error| {
                        tracing::error!(%error, "AI credential store could not be initialized");
                    })
                    .ok()
                    .map(Arc::new)
            }
            #[cfg(target_os = "linux")]
            {
                CredentialManager::linux()
                    .map_err(|error| {
                        tracing::error!(%error, "AI credential store could not be initialized");
                    })
                    .ok()
                    .map(Arc::new)
            }
            #[cfg(not(any(windows, target_os = "linux")))]
            {
                None
            }
        };

        Self {
            paths,
            active_project: Arc::new(Mutex::new(None)),
            project_io_lock: Arc::new(Mutex::new(())),
            project_analysis_cache: Arc::new(Mutex::new(None)),
            project_asset_index_cache: Arc::new(Mutex::new(None)),
            analysis_jobs: Arc::new(Mutex::new(HashMap::new())),
            ai_jobs: JobManager::default(),
            ai_job_tokens: Arc::new(Mutex::new(HashMap::new())),
            deleting_projects: Arc::new(Mutex::new(HashSet::new())),
            credentials,
            exported_paths: Arc::new(Mutex::new(HashSet::new())),
            recent_lock: Arc::new(Mutex::new(())),
            preview_context: Arc::new(Mutex::new(None)),
            _log_guard: Mutex::new(log_guard),
        }
    }

    /// 対象ProjectのAI jobをキャンセルし、DBへ反映する処理が終わるまで待つ。
    pub(crate) async fn cancel_and_wait_project_ai_jobs(
        &self,
        project_id: &str,
    ) -> Result<ProjectDeletionGuard, crate::error::AppError> {
        let deletion_guard = {
            let mut deleting = self
                .deleting_projects
                .lock()
                .map_err(|_| internal_state_error("Project deletion state"))?;
            if !deleting.insert(project_id.to_owned()) {
                return Err(crate::error::AppError::simple(
                    "project_delete_in_progress",
                    "このプロジェクトは削除中です。",
                ));
            }
            ProjectDeletionGuard {
                project_id: project_id.to_owned(),
                deleting_projects: Arc::clone(&self.deleting_projects),
            }
        };

        let jobs = {
            let jobs = self
                .ai_job_tokens
                .lock()
                .map_err(|_| internal_state_error("AI job state"))?;
            jobs.values()
                .filter(|job| job.project_id == project_id)
                .map(|job| (job.cancellation.clone(), Arc::clone(&job.apply_gate)))
                .collect::<Vec<_>>()
        };

        for (cancellation, gate) in jobs {
            let _guard = gate
                .lock()
                .map_err(|_| internal_state_error("AI job apply gate"))?;
            cancellation.cancel();
        }

        loop {
            let has_running_job = self
                .ai_job_tokens
                .lock()
                .map_err(|_| internal_state_error("AI job state"))?
                .values()
                .any(|job| job.project_id == project_id);
            if !has_running_job {
                return Ok(deletion_guard);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Writerのキューを空にしてからProject lockを外し、SQLiteを途中状態で残さない。
    pub fn flush_and_close_project(&self) {
        match self.analysis_jobs.lock() {
            Ok(jobs) => jobs.values().for_each(CancellationToken::cancel),
            Err(error) => {
                tracing::error!(%error, "Analysis job state lock poisoned during shutdown")
            }
        }
        let ai_jobs = match self.ai_job_tokens.lock() {
            Ok(jobs) => jobs
                .values()
                .map(|job| (job.cancellation.clone(), Arc::clone(&job.apply_gate)))
                .collect::<Vec<_>>(),
            Err(error) => {
                tracing::error!(%error, "AI job state lock poisoned during shutdown");
                Vec::new()
            }
        };
        for (cancellation, gate) in ai_jobs {
            match gate.lock() {
                Ok(_guard) => cancellation.cancel(),
                Err(error) => tracing::error!(%error, "AI job apply gate poisoned during shutdown"),
            }
        }
        match self.active_project.lock() {
            Ok(mut active) => drop(active.take()),
            Err(error) => tracing::error!(%error, "Project state lock poisoned during shutdown"),
        }
        match self.project_analysis_cache.lock() {
            Ok(mut analysis) => *analysis = None,
            Err(error) => {
                tracing::error!(%error, "Project analysis cache lock poisoned during shutdown")
            }
        }
        match self.project_asset_index_cache.lock() {
            Ok(mut index) => *index = None,
            Err(error) => {
                tracing::error!(%error, "Project asset index cache could not be cleared during shutdown")
            }
        }
        match self.preview_context.lock() {
            Ok(mut preview) => *preview = None,
            Err(error) => {
                tracing::error!(%error, "Preview cache could not be cleared during shutdown")
            }
        }
    }
}

#[derive(Default)]
struct E2eCredentialBackend(HashMap<ProviderId, String>);

impl CredentialBackend for E2eCredentialBackend {
    fn get(
        &mut self,
        provider: ProviderId,
    ) -> std::result::Result<Option<String>, CredentialStoreError> {
        Ok(self.0.get(&provider).cloned())
    }

    fn set(
        &mut self,
        provider: ProviderId,
        secret: &str,
    ) -> std::result::Result<(), CredentialStoreError> {
        self.0.insert(provider, secret.to_owned());
        Ok(())
    }

    fn delete(&mut self, provider: ProviderId) -> std::result::Result<(), CredentialStoreError> {
        self.0.remove(&provider);
        Ok(())
    }
}

fn internal_state_error(label: &str) -> crate::error::AppError {
    crate::error::AppError::new(
        "internal_state",
        "アプリの状態を読み取れませんでした。再起動してもう一度お試しください。",
        Some(label.to_owned()),
    )
}
