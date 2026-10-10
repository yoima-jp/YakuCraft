use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, State, ipc::Channel};
use tauri_plugin_opener::OpenerExt;
use tokio_util::sync::CancellationToken;
use yc_ai::{
    AiError, CHATGPT_MODELS, ChatGptLogin, ChatGptProvider, CredentialManager, DeepLProvider,
    MAX_BATCH_ENTRIES, MAX_BATCH_SOURCE_CHARACTERS, OpenAiCompatibleProvider, OpenAiProvider,
    ProviderId, TranslationContext, TranslationEntry as AiTranslationEntry, TranslationOptions,
    TranslationOutcome, TranslationProvider, translate_entries,
};
use yc_language::{EntryFlag, TranslationOrigin};
use yc_project::{
    AppSettings, EntryUpdate, OpenedProject, ProjectEntry, UpdateOrigin, load_settings,
};

use crate::{
    error::AppError,
    state::{AiJobControl, AppState},
};

/// ログイン制御をAI command moduleに閉じ、callback portの競合を防ぎつつ、
/// 資格情報の保存中に次の試行が現在世代を置き換えないようにする。
#[derive(Default)]
struct ChatGptLoginControl {
    active: Mutex<Option<(u64, CancellationToken)>>,
    generation: std::sync::atomic::AtomicU64,
    transition: tokio::sync::Mutex<()>,
    commit: tokio::sync::Mutex<()>,
}

impl ChatGptLoginControl {
    async fn begin(&self) -> (u64, CancellationToken) {
        let _commit = self.commit.lock().await;
        let id = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        let cancellation = CancellationToken::new();
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some((_, previous)) = active.as_ref() {
            previous.cancel();
        }
        *active = Some((id, cancellation.clone()));
        (id, cancellation)
    }

    fn is_current(&self, id: u64) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .is_some_and(|(active_id, cancellation)| {
                *active_id == id && !cancellation.is_cancelled()
            })
    }

    async fn cancel(&self) -> bool {
        let _commit = self.commit.lock().await;
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some((_, cancellation)) = active.as_ref() {
            cancellation.cancel();
            true
        } else {
            false
        }
    }

    fn finish(&self, id: u64) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if active
            .as_ref()
            .is_some_and(|(active_id, _)| *active_id == id)
        {
            *active = None;
        }
    }
}

fn chatgpt_login_control() -> &'static ChatGptLoginControl {
    static CONTROL: OnceLock<ChatGptLoginControl> = OnceLock::new();
    CONTROL.get_or_init(ChatGptLoginControl::default)
}

// UI 文字列の翻訳は短文の大量処理なので、最新世代の低価格モデルを既定にする（2026-09 時点）。
// 利用者は設定画面で変更できる。OpenAI のモデル廃止に合わせて見直すこと。
const DEFAULT_OPENAI_MODEL: &str = "gpt-6-luna";
const DEFAULT_CHATGPT_MODEL: &str = "gpt-6-luna";
const DEFAULT_COMPATIBLE_URL: &str = "https://api.openai.com/v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    pub provider: String,
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    pub model: String,
    pub base_url: String,
    pub batch_size: usize,
    pub concurrency: usize,
    pub custom_prompt: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAiProviderInput {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub batch_size: usize,
    pub concurrency: usize,
    pub custom_prompt: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiEntryBaseline {
    pub id: i64,
    /// API設定や認証情報の準備中に入力された訳を保存時に検出するため、IPC開始時の値を比較基準にする。
    pub target_text: String,
    pub origin: TranslationOrigin,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiRejectedEntry {
    pub id: i64,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AiJobEvent {
    Progress {
        job_id: String,
        completed: usize,
        total: usize,
    },
    BatchResult {
        job_id: String,
        entries: Vec<ProjectEntry>,
        rejected: Vec<AiRejectedEntry>,
    },
    Completed {
        job_id: String,
        completed: usize,
        total: usize,
    },
    Cancelled {
        job_id: String,
        completed: usize,
        total: usize,
    },
    Failed {
        job_id: String,
        error: AppError,
    },
}

#[derive(Clone)]
struct AiJobEntry {
    translation: AiTranslationEntry,
    glossary_terms: Vec<yc_ai::GlossaryTerm>,
    namespace: String,
    mod_name: String,
    before_target: String,
    before_origin: TranslationOrigin,
}

struct AiJobRunContext<'a> {
    job_id: &'a str,
    channel: &'a Channel<AiJobEvent>,
    active_project: &'a Arc<std::sync::Mutex<Option<OpenedProject>>>,
    project_id: &'a str,
    config: &'a AiSettings,
    provider: Arc<dyn TranslationProvider>,
    cancellation: CancellationToken,
    apply_gate: Arc<std::sync::Mutex<()>>,
}

#[tauri::command]
pub async fn get_ai_settings(
    provider: Option<String>,
    state: State<'_, AppState>,
) -> Result<AiSettings, AppError> {
    let settings = load_settings(&state.paths.settings_file).map_err(settings_error)?;
    let provider = match provider {
        Some(provider) => parse_provider(&provider)?,
        None => provider_from_settings(&settings)?,
    };
    let credential_status = match state.credentials.as_ref() {
        Some(credentials) => credentials
            .status(provider)
            .await
            .map_err(credential_error)?,
        None => yc_ai::CredentialStatus {
            configured: false,
            provider,
            account_label: None,
        },
    };
    let configured = provider_is_configured(&settings, provider, credential_status.configured);
    Ok(ai_settings_from(
        &settings,
        provider,
        configured,
        credential_status.account_label,
    ))
}

#[tauri::command]
pub async fn start_chatgpt_login(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<yc_ai::CredentialStatus, AppError> {
    let credentials = state
        .credentials
        .as_ref()
        .ok_or_else(credential_unavailable)?;
    let control = chatgpt_login_control();
    let (attempt_id, cancellation) = control.begin().await;
    let _attempt_guard = ChatGptLoginAttemptGuard {
        control,
        id: attempt_id,
    };
    let _transition = control.transition.lock().await;
    if !control.is_current(attempt_id) {
        return Err(chatgpt_login_cancelled_error());
    }
    let login = tokio::select! {
        _ = cancellation.cancelled() => return Err(chatgpt_login_cancelled_error()),
        login = ChatGptLogin::start(cancellation.clone()) => login.map_err(chatgpt_login_error)?,
    };
    if cancellation.is_cancelled() {
        return Err(chatgpt_login_cancelled_error());
    }
    // Frontendの自由入力URLは受け取らず、crateが固定originから生成したURLだけをOSの既定ブラウザで開く。
    app.opener()
        .open_url(login.authorization_url(), None::<&str>)
        .map_err(|_| {
            AppError::simple(
                "chatgpt_browser_open_failed",
                "既定のブラウザを開けませんでした。",
            )
        })?;
    let account = login.wait().await.map_err(chatgpt_login_error)?;
    if cancellation.is_cancelled() || !control.is_current(attempt_id) {
        return Err(chatgpt_login_cancelled_error());
    }
    let credential = account.into_credential();
    // begin/cancelも同じgateを取るため、試行の置換・キャンセルと保存を順序付ける。
    let _commit = control.commit.lock().await;
    if cancellation.is_cancelled() || !control.is_current(attempt_id) {
        return Err(chatgpt_login_cancelled_error());
    }
    let saved = credentials
        .save_chatgpt_credential(&credential)
        .await
        .map_err(credential_error);
    drop(_attempt_guard);
    saved
}

#[tauri::command]
pub async fn cancel_chatgpt_login() -> bool {
    chatgpt_login_control().cancel().await
}

#[tauri::command]
pub async fn logout_chatgpt(state: State<'_, AppState>) -> Result<(), AppError> {
    let credentials = state
        .credentials
        .as_ref()
        .ok_or_else(credential_unavailable)?;
    credentials
        .remove_api_key(ProviderId::ChatGpt)
        .await
        .map_err(credential_error)?;
    Ok(())
}

struct ChatGptLoginAttemptGuard<'a> {
    control: &'a ChatGptLoginControl,
    id: u64,
}

impl Drop for ChatGptLoginAttemptGuard<'_> {
    fn drop(&mut self) {
        self.control.finish(self.id);
    }
}

fn chatgpt_login_cancelled_error() -> AppError {
    AppError::simple(
        "chatgpt_login_cancelled",
        "ChatGPTログインをキャンセルしました。",
    )
}

fn chatgpt_login_error(error: AiError) -> AppError {
    match error {
        AiError::ChatGptLoginCancelled | AiError::Cancelled => chatgpt_login_cancelled_error(),
        AiError::ChatGptCallbackPortInUse => AppError::simple(
            "chatgpt_callback_port_in_use",
            "ChatGPTログインに使うポート1455が使用中です。使用中のアプリを閉じてください。",
        ),
        AiError::ChatGptLoginTimedOut => AppError::simple(
            "chatgpt_login_timed_out",
            "ChatGPTログインの有効期限が切れました。もう一度お試しください。",
        ),
        _ => AppError::simple(
            "chatgpt_login_failed",
            "ChatGPTにログインできませんでした。もう一度お試しください。",
        ),
    }
}

#[tauri::command]
pub async fn save_ai_provider(
    config: SaveAiProviderInput,
    api_key: Option<String>,
    state: State<'_, AppState>,
) -> Result<AiSettings, AppError> {
    let provider = parse_provider(&config.provider)?;
    validate_ai_config(&config)?;

    if provider == ProviderId::ChatGpt
        && api_key.as_deref().is_some_and(|key| !key.trim().is_empty())
    {
        return Err(AppError::simple(
            "invalid_ai_settings",
            "ChatGPTはログインしてください。",
        ));
    }

    if let Some(key) = api_key.as_deref().filter(|key| !key.trim().is_empty()) {
        let credentials = state
            .credentials
            .as_ref()
            .ok_or_else(credential_unavailable)?;
        credentials
            .save_api_key(provider, key)
            .await
            .map_err(credential_error)?;
    }

    let settings = super::settings::update_settings_file(
        &state.paths.settings_file,
        |settings| {
            settings.values.insert(
                "ai.provider".to_owned(),
                Value::String(provider.as_str().to_owned()),
            );
            settings.values.insert(
                setting_key(provider, "model"),
                Value::String(config.model.trim().to_owned()),
            );
            if provider == ProviderId::OpenAiCompatible {
                settings.values.insert(
                    setting_key(provider, "baseUrl"),
                    Value::String(config.base_url.trim().to_owned()),
                );
            }
            settings.values.insert(
                setting_key(provider, "batchSize"),
                Value::from(config.batch_size),
            );
            settings.values.insert(
                setting_key(provider, "concurrency"),
                Value::from(config.concurrency),
            );
            settings.values.insert(
                setting_key(provider, "customPrompt"),
                Value::String(config.custom_prompt),
            );
            Ok(())
        },
        settings_error,
        settings_error,
    )?;

    let credential_status = match state.credentials.as_ref() {
        Some(credentials) => credentials
            .status(provider)
            .await
            .map_err(credential_error)?,
        None => yc_ai::CredentialStatus {
            configured: false,
            provider,
            account_label: None,
        },
    };
    let configured = provider_is_configured(&settings, provider, credential_status.configured);
    Ok(ai_settings_from(
        &settings,
        provider,
        configured,
        credential_status.account_label,
    ))
}

#[tauri::command]
pub async fn delete_ai_provider(
    provider: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let provider = parse_provider(&provider)?;
    if let Some(credentials) = state.credentials.as_ref() {
        credentials
            .remove_api_key(provider)
            .await
            .map_err(credential_error)?;
    }

    super::settings::update_settings_file(
        &state.paths.settings_file,
        |settings| {
            for field in [
                "model",
                "baseUrl",
                "batchSize",
                "concurrency",
                "customPrompt",
            ] {
                settings.values.remove(&setting_key(provider, field));
            }
            if provider_from_settings(settings)? == provider {
                settings.values.insert(
                    "ai.provider".to_owned(),
                    Value::String(ProviderId::OpenAi.as_str().to_owned()),
                );
            }
            Ok(())
        },
        settings_error,
        settings_error,
    )?;
    Ok(())
}

#[tauri::command]
pub async fn test_ai_provider(
    provider: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let provider = parse_provider(&provider)?;
    let settings = load_settings(&state.paths.settings_file).map_err(settings_error)?;
    let credential = load_provider_credential(state.credentials.as_deref(), provider).await?;
    let configured = provider_is_configured(&settings, provider, credential.is_some());
    if !configured {
        return Err(not_configured_error());
    }
    let ai_settings = ai_settings_from(&settings, provider, configured, None);
    let client = build_provider(
        provider,
        &ai_settings,
        credential,
        state.credentials.clone(),
    )?;
    client
        .test_connection(&CancellationToken::new())
        .await
        .map_err(ai_error)
}

#[tauri::command]
pub async fn start_ai_job(
    entry_ids: Vec<i64>,
    provider: String,
    expected_entries: Vec<AiEntryBaseline>,
    mod_names_by_unit_and_namespace: HashMap<i64, HashMap<String, String>>,
    expected_project_id: String,
    channel: Channel<AiJobEvent>,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    let provider = parse_provider(&provider)?;
    let selected_ids = unique_ids(entry_ids);
    if selected_ids.is_empty() {
        return Err(AppError::simple(
            "empty_ai_selection",
            "翻訳する項目がありません。",
        ));
    }

    let deleting_projects = state
        .deleting_projects
        .lock()
        .map_err(|_| internal_state_error("Project deletion state"))?;
    if deleting_projects.contains(&expected_project_id) {
        return Err(AppError::simple(
            "project_delete_in_progress",
            "このプロジェクトは削除中です。",
        ));
    }

    let job = state.ai_jobs.start_job().map_err(ai_error)?;
    let job_id = job.id().to_string();
    let cancellation = job.cancellation_token();
    let expected_by_id: HashMap<_, _> = expected_entries
        .into_iter()
        .map(|entry| (entry.id, entry))
        .collect();
    let apply_gate = Arc::new(std::sync::Mutex::new(()));
    state
        .ai_job_tokens
        .lock()
        .map_err(|_| internal_state_error("AI job state"))?
        .insert(
            job_id.clone(),
            AiJobControl {
                project_id: expected_project_id.clone(),
                cancellation: cancellation.clone(),
                apply_gate: Arc::clone(&apply_gate),
            },
        );
    drop(deleting_projects);

    let active_project = Arc::clone(&state.active_project);
    let settings_file = state.paths.settings_file.clone();
    let credentials = state.credentials.clone();
    let job_tokens = Arc::clone(&state.ai_job_tokens);
    let event_channel = channel.clone();
    let total = selected_ids.len();
    let returned_job_id = job_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = async {
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let settings = load_settings(&settings_file).map_err(settings_error)?;
            let credential = load_provider_credential(credentials.as_deref(), provider).await?;
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let configured = provider_is_configured(&settings, provider, credential.is_some());
            if !configured {
                return Err(not_configured_error());
            }
            let ai_settings = ai_settings_from(&settings, provider, configured, None);
            let client = Arc::<dyn TranslationProvider>::from(build_provider(
                provider,
                &ai_settings,
                credential,
                credentials.clone(),
            )?);

            let prepare_active_project = Arc::clone(&active_project);
            let prepare_cancellation = cancellation.clone();
            let (project_id, entries) = tauri::async_runtime::spawn_blocking(move || {
                prepare_ai_job_entries(
                    &prepare_active_project,
                    &selected_ids,
                    &expected_by_id,
                    &mod_names_by_unit_and_namespace,
                    &expected_project_id,
                    &prepare_cancellation,
                )
            })
            .await
            .map_err(|error| task_error(error.to_string()))??;
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }

            let context = AiJobRunContext {
                job_id: &job_id,
                channel: &event_channel,
                active_project: &active_project,
                project_id: &project_id,
                config: &ai_settings,
                provider: client,
                cancellation: cancellation.clone(),
                apply_gate: Arc::clone(&apply_gate),
            };
            run_ai_job(context, entries).await
        }
        .await;
        let completed = result.as_ref().copied().unwrap_or(0);
        let terminal = if cancellation.is_cancelled() {
            AiJobEvent::Cancelled {
                job_id: job_id.clone(),
                completed,
                total,
            }
        } else if let Err(error) = result {
            AiJobEvent::Failed {
                job_id: job_id.clone(),
                error,
            }
        } else {
            AiJobEvent::Completed {
                job_id: job_id.clone(),
                completed,
                total,
            }
        };
        let _ = event_channel.send(terminal);
        if let Ok(mut jobs) = job_tokens.lock() {
            jobs.remove(&job_id);
        }
        drop(job);
    });

    Ok(returned_job_id)
}

fn prepare_ai_job_entries(
    active_project: &Arc<std::sync::Mutex<Option<OpenedProject>>>,
    selected_ids: &[i64],
    expected_by_id: &HashMap<i64, AiEntryBaseline>,
    mod_names_by_unit_and_namespace: &HashMap<i64, HashMap<String, String>>,
    expected_project_id: &str,
    cancellation: &CancellationToken,
) -> Result<(String, Vec<AiJobEntry>), AppError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    let active = active_project
        .lock()
        .map_err(|_| internal_state_error("active Project state"))?;
    let opened = active
        .as_ref()
        .ok_or_else(|| AppError::simple("project_required", "プロジェクトを開いてください。"))?;
    if opened.summary().id != expected_project_id {
        return Err(project_changed_error());
    }
    // 翻訳元とnamespaceはProject DBから読み、Mod名は解析済み情報をIPCで受け取ってJAR再解析を省く。
    let stored_entries = opened
        .repository()
        .list_entries(None)
        .map_err(|error| database_error(error.to_string()))?;
    let glossary_terms = opened
        .repository()
        .list_glossary_terms()
        .map_err(|error| database_error(error.to_string()))?;
    let glossary_matcher = yc_project::GlossaryMatcher::new(&glossary_terms)
        .map_err(|error| database_error(error.to_string()))?;
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    let by_id: HashMap<_, _> = stored_entries
        .into_iter()
        .map(|entry| (entry.id, entry))
        .collect();
    let mut entries = Vec::with_capacity(selected_ids.len());
    for id in selected_ids {
        let entry = by_id.get(id).ok_or_else(|| {
            AppError::simple("entry_not_found", "翻訳項目を読み込めませんでした。")
        })?;
        let expected = expected_by_id.get(id).ok_or_else(|| {
            AppError::simple("invalid_ai_selection", "翻訳項目を読み込めませんでした。")
        })?;
        let source = entry.source_text.clone().unwrap_or_default();
        let matched_glossary_terms = glossary_matcher
            .matching_terms(&source)
            .into_iter()
            .map(|term| yc_ai::GlossaryTerm {
                source_term: term.source_term,
                target_term: term.target_term,
            })
            .collect();
        let source_locale = entry.source_locale.as_deref().unwrap_or("en_us");
        entries.push(AiJobEntry {
            translation: AiTranslationEntry::new(
                entry.id,
                entry.key.clone(),
                source,
                source_locale,
                "ja_jp",
            ),
            glossary_terms: matched_glossary_terms,
            namespace: entry.namespace.clone(),
            mod_name: mod_names_by_unit_and_namespace
                .get(&entry.mod_unit_id)
                .and_then(|namespaces| namespaces.get(&entry.namespace))
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| entry.namespace.clone()),
            before_target: expected.target_text.clone(),
            before_origin: expected.origin.clone(),
        });
    }
    Ok((opened.summary().id.clone(), entries))
}

#[tauri::command]
pub fn cancel_ai_job(job_id: String, state: State<'_, AppState>) -> bool {
    let Ok(job_id) = uuid::Uuid::parse_str(&job_id) else {
        return false;
    };
    let control = state.ai_job_tokens.lock().ok().and_then(|jobs| {
        jobs.get(&job_id.to_string()).map(|control| {
            (
                control.cancellation.clone(),
                Arc::clone(&control.apply_gate),
            )
        })
    });
    let Some((cancellation, apply_gate)) = control else {
        return false;
    };
    let Ok(_guard) = apply_gate.lock() else {
        return false;
    };
    // このgateをDB反映側も保持するため、cancelが先なら未反映、反映側が先ならcommit済みの結果として順序が確定する。
    cancellation.cancel();
    state.ai_jobs.cancel_job(job_id)
}

async fn run_ai_job(
    context: AiJobRunContext<'_>,
    entries: Vec<AiJobEntry>,
) -> Result<usize, AppError> {
    let AiJobRunContext {
        job_id,
        channel,
        active_project,
        project_id,
        config,
        provider,
        cancellation,
        apply_gate,
    } = context;
    let total = entries.len();
    let batches = split_job_batches(entries, config.batch_size);
    let completed = Arc::new(AtomicUsize::new(0));
    let concurrency = config.concurrency.clamp(1, 4);
    let jobs = stream::iter(batches.into_iter().map(|batch| {
        let provider = Arc::clone(&provider);
        let cancellation = cancellation.clone();
        let custom_prompt = config.custom_prompt.clone();
        async move {
            let namespace = batch
                .first()
                .map(|entry| entry.namespace.as_str())
                .unwrap_or_default();
            let mod_name = batch
                .first()
                .map(|entry| entry.mod_name.clone())
                .unwrap_or_default();
            let mut seen_glossary_terms = HashSet::new();
            let glossary_terms = batch
                .iter()
                .flat_map(|entry| entry.glossary_terms.iter().cloned())
                .filter(|term| {
                    seen_glossary_terms.insert((term.source_term.clone(), term.target_term.clone()))
                })
                .collect();
            let context = TranslationContext {
                mod_name,
                namespace: namespace.to_owned(),
                custom_prompt: Some(custom_prompt),
                glossary_terms,
            };
            let translation_entries: Vec<_> = batch
                .iter()
                .map(|entry| entry.translation.clone())
                .collect();
            let outcome = translate_entries(
                provider.as_ref(),
                &translation_entries,
                &context,
                &cancellation,
                TranslationOptions { concurrency: 1 },
                None,
            )
            .await;
            (batch, outcome)
        }
    }))
    .buffer_unordered(concurrency);
    futures_util::pin_mut!(jobs);

    while let Some((batch, outcome)) = jobs.next().await {
        if cancellation.is_cancelled() {
            return Err(cancelled_error());
        }
        let outcome = outcome.map_err(ai_error)?;
        let (applied, rejected) = {
            let active = active_project
                .lock()
                .map_err(|_| internal_state_error("active Project state"))?;
            let opened = active.as_ref().ok_or_else(project_changed_error)?;
            if opened.summary().id != project_id {
                return Err(project_changed_error());
            }
            let _apply_guard = apply_gate
                .lock()
                .map_err(|_| internal_state_error("AI batch apply gate"))?;
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            apply_outcome_to_project(opened, &batch, &outcome, &cancellation)?
        };
        let batch_event = AiJobEvent::BatchResult {
            job_id: job_id.to_owned(),
            entries: applied,
            rejected,
        };
        if channel.send(batch_event).is_err() {
            cancellation.cancel();
            return Err(channel_error());
        }
        let done = completed.fetch_add(batch.len(), Ordering::Relaxed) + batch.len();
        let progress = AiJobEvent::Progress {
            job_id: job_id.to_owned(),
            completed: done,
            total,
        };
        if channel.send(progress).is_err() {
            cancellation.cancel();
            return Err(channel_error());
        }
    }

    Ok(completed.load(Ordering::Relaxed))
}

fn apply_outcome_to_project(
    opened: &OpenedProject,
    batch: &[AiJobEntry],
    outcome: &TranslationOutcome,
    cancellation: &CancellationToken,
) -> Result<(Vec<ProjectEntry>, Vec<AiRejectedEntry>), AppError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    let current = opened
        .repository()
        .list_entries(None)
        .map_err(|error| database_error(error.to_string()))?;
    let mut current_by_id: HashMap<_, _> =
        current.into_iter().map(|entry| (entry.id, entry)).collect();
    let requested_by_id: HashMap<_, _> = batch
        .iter()
        .map(|entry| (entry.translation.id, entry))
        .collect();
    let unchanged_ids: HashSet<_> = requested_by_id
        .iter()
        .filter_map(|(id, requested)| {
            current_by_id
                .get(id)
                .is_some_and(|entry| {
                    entry.target_text.as_deref().unwrap_or_default() == requested.before_target
                        && entry.origin == requested.before_origin
                })
                .then_some(*id)
        })
        .collect();

    let updates: Vec<_> = outcome
        .accepted
        .iter()
        .filter(|translation| unchanged_ids.contains(&translation.id))
        .map(|translation| EntryUpdate {
            id: translation.id,
            target_text: translation.translation.clone(),
            origin: UpdateOrigin::Ai,
        })
        .collect();
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    if !updates.is_empty() {
        opened
            .writer()
            .update_entries(updates.clone())
            .map_err(|error| database_error(error.to_string()))?;
    }

    for update in &updates {
        opened
            .writer()
            .remove_flag(update.id, EntryFlag::PlaceholderError)
            .map_err(|error| database_error(error.to_string()))?;
        opened
            .writer()
            .remove_flag(update.id, EntryFlag::NeedsReview)
            .map_err(|error| database_error(error.to_string()))?;
    }

    let requested_ids: HashSet<_> = requested_by_id.keys().copied().collect();
    let mut rejected_by_id: HashMap<i64, Vec<String>> = HashMap::new();
    for rejection in &outcome.rejected {
        if requested_ids.contains(&rejection.id) && unchanged_ids.contains(&rejection.id) {
            let reasons = rejected_by_id.entry(rejection.id).or_default();
            for reason in rejection.reasons.iter().map(validation_reason) {
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
            }
        }
    }
    let mut rejected = Vec::with_capacity(rejected_by_id.len());
    for entry in batch {
        let id = entry.translation.id;
        let Some(reasons) = rejected_by_id.remove(&id) else {
            continue;
        };
        opened
            .writer()
            .add_flag(id, EntryFlag::NeedsReview)
            .map_err(|error| database_error(error.to_string()))?;
        if let Some(stored) = current_by_id.get_mut(&id)
            && !stored.flags.contains(&EntryFlag::NeedsReview)
        {
            stored.flags.push(EntryFlag::NeedsReview);
        }
        rejected.push(AiRejectedEntry { id, reasons });
    }

    let applied = updates
        .iter()
        .filter_map(|update| {
            let stored = current_by_id.get_mut(&update.id)?;
            stored.target_text = Some(update.target_text.clone());
            stored.translated_source_text = stored.source_text.clone();
            stored.origin = TranslationOrigin::Ai;
            stored.flags.retain(|flag| {
                !matches!(flag, EntryFlag::PlaceholderError | EntryFlag::NeedsReview)
            });
            Some(stored.clone())
        })
        .collect();
    Ok((applied, rejected))
}

fn split_job_batches(entries: Vec<AiJobEntry>, batch_size: usize) -> Vec<Vec<AiJobEntry>> {
    let limit = batch_size.clamp(1, MAX_BATCH_ENTRIES);
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut namespace: Option<String> = None;
    let mut source_characters = 0usize;
    for entry in entries {
        let count_limit = batch.len() >= limit;
        let character_count = entry.translation.source.chars().count();
        let character_limit = !batch.is_empty()
            && source_characters.saturating_add(character_count) > MAX_BATCH_SOURCE_CHARACTERS;
        let namespace_changed = namespace
            .as_deref()
            .is_some_and(|value| value != entry.namespace);
        if count_limit || character_limit || namespace_changed {
            batches.push(std::mem::take(&mut batch));
            source_characters = 0;
        }
        namespace = Some(entry.namespace.clone());
        source_characters = source_characters.saturating_add(character_count);
        batch.push(entry);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

fn ai_settings_from(
    settings: &AppSettings,
    provider: ProviderId,
    configured: bool,
    account_label: Option<String>,
) -> AiSettings {
    let model_default = match provider {
        ProviderId::OpenAi => DEFAULT_OPENAI_MODEL,
        ProviderId::ChatGpt => DEFAULT_CHATGPT_MODEL,
        _ => "",
    };
    AiSettings {
        provider: provider.as_str().to_owned(),
        configured,
        account_label,
        model: setting_string(settings, provider, "model", model_default),
        base_url: setting_string(settings, provider, "baseUrl", DEFAULT_COMPATIBLE_URL),
        batch_size: setting_usize(settings, provider, "batchSize", MAX_BATCH_ENTRIES),
        concurrency: setting_usize(settings, provider, "concurrency", 2).clamp(1, 4),
        custom_prompt: setting_string(settings, provider, "customPrompt", ""),
    }
}

fn provider_is_configured(
    settings: &AppSettings,
    provider: ProviderId,
    has_credential: bool,
) -> bool {
    if provider == ProviderId::ChatGpt {
        let model = setting_string(settings, provider, "model", DEFAULT_CHATGPT_MODEL);
        return has_credential && CHATGPT_MODELS.iter().any(|candidate| candidate.id == model);
    }
    if provider != ProviderId::OpenAiCompatible {
        return has_credential;
    }

    let config = ai_settings_from(settings, provider, false, None);
    OpenAiCompatibleProvider::new(&config.base_url, None, config.model).is_ok()
}

fn setting_string(
    settings: &AppSettings,
    provider: ProviderId,
    field: &str,
    default: &str,
) -> String {
    settings
        .values
        .get(&setting_key(provider, field))
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn setting_usize(
    settings: &AppSettings,
    provider: ProviderId,
    field: &str,
    default: usize,
) -> usize {
    settings
        .values
        .get(&setting_key(provider, field))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

fn setting_key(provider: ProviderId, field: &str) -> String {
    format!("ai.{}.{}", provider.as_str(), field)
}

fn provider_from_settings(settings: &AppSettings) -> Result<ProviderId, AppError> {
    let provider = settings
        .values
        .get("ai.provider")
        .and_then(Value::as_str)
        // 既定は ChatGPT（サブスクリプション）。API Key を用意せずにログインだけで使い始められるため。
        .unwrap_or("chatgpt");
    parse_provider(provider)
}

fn parse_provider(provider: &str) -> Result<ProviderId, AppError> {
    match provider {
        "openai" => Ok(ProviderId::OpenAi),
        "openai-compatible" => Ok(ProviderId::OpenAiCompatible),
        "deepl" => Ok(ProviderId::DeepL),
        "chatgpt" => Ok(ProviderId::ChatGpt),
        _ => Err(AppError::simple(
            "invalid_ai_provider",
            "Providerを選び直してください。",
        )),
    }
}

fn validate_ai_config(config: &SaveAiProviderInput) -> Result<(), AppError> {
    let provider = parse_provider(&config.provider)?;
    if config.batch_size == 0
        || config.batch_size > MAX_BATCH_ENTRIES
        || !(1..=4).contains(&config.concurrency)
    {
        return Err(AppError::simple(
            "invalid_ai_settings",
            "AI翻訳の設定を確認してください。",
        ));
    }
    if provider != ProviderId::DeepL && config.model.trim().is_empty() {
        return Err(AppError::simple(
            "invalid_ai_settings",
            "Modelを入力してください。",
        ));
    }
    if provider == ProviderId::ChatGpt
        && !CHATGPT_MODELS
            .iter()
            .any(|candidate| candidate.id == config.model)
    {
        return Err(AppError::simple(
            "invalid_ai_settings",
            "ChatGPTのモデルを選び直してください。",
        ));
    }
    if provider == ProviderId::OpenAiCompatible {
        OpenAiCompatibleProvider::new(&config.base_url, None, &config.model).map_err(ai_error)?;
    }
    Ok(())
}

fn build_provider(
    provider: ProviderId,
    config: &AiSettings,
    credential: Option<String>,
    credentials: Option<Arc<CredentialManager>>,
) -> Result<Box<dyn TranslationProvider>, AppError> {
    let client: Box<dyn TranslationProvider> = match provider {
        ProviderId::OpenAi => Box::new(
            OpenAiProvider::new(
                credential.ok_or_else(not_configured_error)?,
                config.model.clone(),
            )
            .map_err(ai_error)?,
        ),
        ProviderId::OpenAiCompatible => Box::new(
            OpenAiCompatibleProvider::new(&config.base_url, credential, config.model.clone())
                .map_err(ai_error)?,
        ),
        ProviderId::DeepL => Box::new(
            DeepLProvider::new(credential.ok_or_else(not_configured_error)?).map_err(ai_error)?,
        ),
        ProviderId::ChatGpt => Box::new(
            ChatGptProvider::new(
                credential.as_deref().ok_or_else(not_configured_error)?,
                config.model.clone(),
                credentials
                    .ok_or_else(credential_unavailable)?
                    .as_ref()
                    .clone(),
            )
            .map_err(ai_error)?,
        ),
    };
    Ok(client)
}

async fn load_provider_credential(
    credentials: Option<&CredentialManager>,
    provider: ProviderId,
) -> Result<Option<String>, AppError> {
    let Some(credentials) = credentials else {
        return Ok(None);
    };
    credentials
        .load_api_key(provider)
        .await
        .map(|secret| secret.map(|secret| secret.expose_secret().to_owned()))
        .map_err(credential_error)
}

fn unique_ids(ids: Vec<i64>) -> Vec<i64> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.into_iter().filter(|id| seen.insert(*id)).collect()
}

fn validation_reason(issue: &yc_validation::ValidationIssue) -> String {
    use yc_validation::ValidationIssue;
    match issue {
        ValidationIssue::MissingResponse => "翻訳が返されませんでした".to_owned(),
        ValidationIssue::DuplicateId => "翻訳結果が重複しています".to_owned(),
        ValidationIssue::UnknownId => "不明な項目の結果が含まれていました".to_owned(),
        ValidationIssue::EmptyTranslation => "翻訳が空です".to_owned(),
        ValidationIssue::PlaceholderMismatch { .. } => "Placeholderが原文と一致しません".to_owned(),
        ValidationIssue::InvalidControlCharacter { .. } => {
            "使用できない制御文字が含まれています".to_owned()
        }
    }
}

fn settings_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "settings_unavailable",
        "設定を読み書きできませんでした。",
        Some(error.to_string()),
    )
}

fn credential_error(error: AiError) -> AppError {
    AppError::new(
        "ai_credential_error",
        "API Keyを読み書きできませんでした。",
        Some(error.to_string()),
    )
}

fn credential_unavailable() -> AppError {
    AppError::simple(
        "ai_credential_unavailable",
        "この環境ではAPI Keyを保存できません。",
    )
}

fn not_configured_error() -> AppError {
    AppError::simple(
        "ai_not_configured",
        "AI Translationの設定を完了してください。",
    )
}

fn ai_error(error: AiError) -> AppError {
    let (code, user_message) = match &error {
        AiError::JobAlreadyRunning => ("ai_job_running", "AI翻訳はすでに実行中です。"),
        AiError::Cancelled => ("ai_job_cancelled", "AI翻訳をキャンセルしました。"),
        AiError::InvalidUrl => ("ai_invalid_url", "接続先URLを確認してください。"),
        AiError::ChatGptReauthenticationRequired => (
            "chatgpt_reauthentication_required",
            "ChatGPTに再ログインしてください。",
        ),
        _ => ("ai_provider_error", "AI翻訳を実行できませんでした。"),
    };
    AppError::new(code, user_message, Some(error.to_string()))
}

fn cancelled_error() -> AppError {
    AppError::simple("ai_job_cancelled", "AI翻訳をキャンセルしました。")
}

fn channel_error() -> AppError {
    AppError::simple("ai_channel_closed", "AI翻訳の進捗を受け取れませんでした。")
}

fn project_changed_error() -> AppError {
    AppError::simple(
        "project_changed",
        "プロジェクトが切り替わったため、AI翻訳を停止しました。",
    )
}

fn database_error(detail: impl Into<String>) -> AppError {
    AppError::new(
        "database",
        "翻訳データを保存できませんでした。",
        Some(detail.into()),
    )
}

fn internal_state_error(detail: &str) -> AppError {
    AppError::new(
        "internal_state",
        "アプリの状態を読み取れませんでした。再起動してもう一度お試しください。",
        Some(detail.to_owned()),
    )
}

fn task_error(detail: String) -> AppError {
    AppError::new(
        "task_failed",
        "処理に失敗しました。もう一度お試しください。",
        Some(detail),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        AiEntryBaseline, AiJobEntry, AiSettings, ai_settings_from, apply_outcome_to_project,
        parse_provider, prepare_ai_job_entries,
    };
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;
    use yc_ai::{
        CredentialBackend, CredentialManager, CredentialStoreError, ProviderId, TranslationOutcome,
    };
    use yc_language::{EntryFlag, LanguageFileFormat, TranslationOrigin};
    use yc_project::{
        AnalysisData, EntryUpdate, GlossaryTermInput, LanguageRecord, ModRecord, NamespaceRecord,
        NestedArchiveRecord, ProjectEntry, UpdateOrigin, create_project, open_project,
    };
    use yc_validation::{AiTranslation, RequestedEntry, ValidatedTranslation, validate_ai_batch};

    #[derive(Default)]
    struct MemoryCredentialStore(HashMap<ProviderId, String>);

    impl CredentialBackend for MemoryCredentialStore {
        fn get(&mut self, provider: ProviderId) -> Result<Option<String>, CredentialStoreError> {
            Ok(self.0.get(&provider).cloned())
        }

        fn set(&mut self, provider: ProviderId, secret: &str) -> Result<(), CredentialStoreError> {
            self.0.insert(provider, secret.to_owned());
            Ok(())
        }

        fn delete(&mut self, provider: ProviderId) -> Result<(), CredentialStoreError> {
            self.0.remove(&provider);
            Ok(())
        }
    }

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn create() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("yakucraft-ai-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn project_with_entry() -> (TemporaryDirectory, yc_project::OpenedProject, ProjectEntry) {
        let temporary = TemporaryDirectory::create();
        let source_path = temporary.0.join("source.jar");
        fs::write(&source_path, b"test JAR").unwrap();
        let summary = create_project(
            temporary.0.join("projects"),
            std::slice::from_ref(&source_path),
            None,
        )
        .unwrap();
        let opened = open_project(
            temporary.0.join("projects"),
            Uuid::parse_str(&summary.id).unwrap(),
        )
        .unwrap();
        opened
            .writer()
            .save_analysis(
                1,
                AnalysisData {
                    mods: vec![ModRecord {
                        mod_unit_id: 0,
                        mod_id: Some("test".into()),
                        name: Some("Test".into()),
                        version: None,
                        loader: None,
                        minecraft_version: None,
                        metadata_source: None,
                    }],
                    namespaces: vec![NamespaceRecord {
                        mod_unit_id: 0,
                        namespace: "test".into(),
                        container_path: String::new(),
                        included: true,
                        entry_count: 1,
                    }],
                    nested_archives: Vec::<NestedArchiveRecord>::new(),
                    languages: vec![LanguageRecord {
                        mod_unit_id: 0,
                        namespace: "test".into(),
                        locale: "en_us".into(),
                        format: LanguageFileFormat::Json,
                        container_path: String::new(),
                        archive_path: "assets/test/lang/en_us.json".into(),
                        entry_count: 1,
                        is_target: false,
                    }],
                    entries: vec![yc_language::TranslationEntry {
                        namespace: "test".into(),
                        key: "item.apple".into(),
                        source_text: Some("Apple %s".into()),
                        source_locale: Some("en_us".into()),
                        target_text: String::new(),
                        target_original_text: None,
                        translated_source_text: None,
                        origin: TranslationOrigin::Missing,
                        flags: vec![],
                        sort_index: 0,
                    }],
                },
            )
            .unwrap();
        let entry = opened.repository().list_entries(None).unwrap().remove(0);
        (temporary, opened, entry)
    }

    #[test]
    fn ai_job_preparation_keeps_only_terms_present_in_the_source() {
        let (_temporary, opened, entry) = project_with_entry();
        let project_id = opened.summary().id.clone();
        opened
            .writer()
            .upsert_glossary_terms(vec![
                GlossaryTermInput {
                    source_term: "Apple".into(),
                    target_term: "りんご".into(),
                    note: None,
                    case_sensitive: false,
                },
                GlossaryTermInput {
                    source_term: "Mana".into(),
                    target_term: "マナ".into(),
                    note: None,
                    case_sensitive: false,
                },
            ])
            .unwrap();
        let active_project = Arc::new(Mutex::new(Some(opened)));
        let expected = HashMap::from([(
            entry.id,
            AiEntryBaseline {
                id: entry.id,
                target_text: String::new(),
                origin: entry.origin,
            },
        )]);

        let (_, prepared) = prepare_ai_job_entries(
            &active_project,
            &[entry.id],
            &expected,
            &HashMap::new(),
            &project_id,
            &CancellationToken::new(),
        )
        .unwrap();

        let sources = prepared[0]
            .glossary_terms
            .iter()
            .map(|term| term.source_term.as_str())
            .collect::<Vec<_>>();
        assert_eq!(sources, ["Apple"]);
    }

    #[tokio::test]
    async fn ai_settings_report_credential_status_without_exposing_the_secret() {
        let manager = CredentialManager::new(MemoryCredentialStore::default()).unwrap();
        manager
            .save_api_key(ProviderId::OpenAi, "never-return-this")
            .await
            .unwrap();
        let status = manager.status(ProviderId::OpenAi).await.unwrap();
        let mut settings = yc_project::AppSettings::new();
        settings.values.insert(
            "ai.openai.model".into(),
            serde_json::Value::String("example-model".into()),
        );
        let response: AiSettings =
            ai_settings_from(&settings, status.provider, status.configured, None);
        let encoded = serde_json::to_string(&response).unwrap();
        assert!(response.configured);
        assert_eq!(
            parse_provider(&response.provider).unwrap(),
            ProviderId::OpenAi
        );
        assert!(!encoded.contains("never-return-this"));
        assert!(!encoded.contains("apiKey"));
    }

    #[test]
    fn accepted_ai_result_updates_database_origin_and_translated_source() {
        let (_temporary, opened, entry) = project_with_entry();
        let source = entry.source_text.clone().unwrap();
        let batch = [AiJobEntry {
            translation: yc_ai::TranslationEntry::new(
                entry.id,
                entry.key.clone(),
                source.clone(),
                "en_us",
                "ja_jp",
            ),
            glossary_terms: Vec::new(),
            namespace: entry.namespace.clone(),
            mod_name: "Test".into(),
            before_target: entry.target_text.clone().unwrap_or_default(),
            before_origin: entry.origin.clone(),
        }];
        let outcome = TranslationOutcome {
            accepted: vec![ValidatedTranslation {
                id: entry.id,
                translation: "りんご %s".into(),
            }],
            rejected: Vec::new(),
        };
        let (updated, rejected) =
            apply_outcome_to_project(&opened, &batch, &outcome, &CancellationToken::new()).unwrap();
        assert!(rejected.is_empty());
        assert_eq!(updated[0].origin, TranslationOrigin::Ai);
        assert_eq!(
            updated[0].translated_source_text.as_deref(),
            Some(source.as_str())
        );
    }

    #[test]
    fn cancelled_batch_is_not_written_to_database() {
        let (_temporary, opened, entry) = project_with_entry();
        let batch = [AiJobEntry {
            translation: yc_ai::TranslationEntry::new(
                entry.id,
                entry.key.clone(),
                "Apple %s",
                "en_us",
                "ja_jp",
            ),
            glossary_terms: Vec::new(),
            namespace: entry.namespace.clone(),
            mod_name: "Test".into(),
            before_target: entry.target_text.clone().unwrap_or_default(),
            before_origin: entry.origin.clone(),
        }];
        let outcome = TranslationOutcome {
            accepted: vec![ValidatedTranslation {
                id: entry.id,
                translation: "りんご %s".into(),
            }],
            rejected: Vec::new(),
        };
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(apply_outcome_to_project(&opened, &batch, &outcome, &cancellation).is_err());
        let unchanged = opened.repository().list_entries(None).unwrap().remove(0);
        assert!(unchanged.target_text.is_none());
        assert_eq!(unchanged.origin, TranslationOrigin::Missing);
    }

    #[test]
    fn ai_validation_rejections_do_not_enter_the_database_as_translations() {
        let (_temporary, opened, entry) = project_with_entry();
        let requested = [RequestedEntry {
            id: entry.id,
            source: "Apple %s".into(),
        }];
        let invalid = [AiTranslation {
            id: entry.id,
            translation: "りんご".into(),
        }];
        let validation = validate_ai_batch(&requested, &invalid);
        let batch = [AiJobEntry {
            translation: yc_ai::TranslationEntry::new(
                entry.id, entry.key, "Apple %s", "en_us", "ja_jp",
            ),
            glossary_terms: Vec::new(),
            namespace: entry.namespace,
            mod_name: "Test".into(),
            before_target: entry.target_text.unwrap_or_default(),
            before_origin: entry.origin,
        }];
        let outcome = TranslationOutcome {
            accepted: validation.accepted,
            rejected: validation.rejected,
        };
        let (updated, rejected) =
            apply_outcome_to_project(&opened, &batch, &outcome, &CancellationToken::new()).unwrap();
        assert!(updated.is_empty());
        assert_eq!(rejected.len(), 1);
        let stored = opened.repository().list_entries(None).unwrap().remove(0);
        assert!(stored.target_text.is_none());
        assert!(stored.flags.contains(&EntryFlag::NeedsReview));
    }

    #[test]
    fn ai_job_preparation_honors_cancellation_before_reading_entries() {
        let (_temporary, opened, entry) = project_with_entry();
        let project_id = opened.summary().id.clone();
        let active_project = Arc::new(Mutex::new(Some(opened)));
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = prepare_ai_job_entries(
            &active_project,
            &[entry.id],
            &HashMap::from([(
                entry.id,
                AiEntryBaseline {
                    id: entry.id,
                    target_text: String::new(),
                    origin: TranslationOrigin::Missing,
                },
            )]),
            &HashMap::new(),
            &project_id,
            &cancellation,
        );

        assert!(result.is_err());
    }

    #[test]
    fn ai_result_does_not_replace_an_entry_edited_after_job_start() {
        let (_temporary, opened, entry) = project_with_entry();
        let batch = [AiJobEntry {
            translation: yc_ai::TranslationEntry::new(
                entry.id,
                entry.key.clone(),
                "Apple %s",
                "en_us",
                "ja_jp",
            ),
            glossary_terms: Vec::new(),
            namespace: entry.namespace.clone(),
            mod_name: "Test".into(),
            before_target: String::new(),
            before_origin: TranslationOrigin::Missing,
        }];
        opened
            .writer()
            .update_entries(vec![EntryUpdate {
                id: entry.id,
                target_text: "手動の訳".into(),
                origin: UpdateOrigin::Manual,
            }])
            .unwrap();
        let outcome = TranslationOutcome {
            accepted: vec![ValidatedTranslation {
                id: entry.id,
                translation: "AIの訳 %s".into(),
            }],
            rejected: Vec::new(),
        };

        let (updated, rejected) =
            apply_outcome_to_project(&opened, &batch, &outcome, &CancellationToken::new()).unwrap();

        assert!(updated.is_empty());
        assert!(rejected.is_empty());
        let stored = opened.repository().list_entries(None).unwrap().remove(0);
        assert_eq!(stored.target_text.as_deref(), Some("手動の訳"));
        assert_eq!(stored.origin, TranslationOrigin::Edited);
    }
}
