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
    AiError, AnthropicProvider, CHATGPT_MODELS, ChatGptLogin, ChatGptProvider, CredentialManager,
    DeepLProvider, MAX_BATCH_ENTRIES, MAX_BATCH_SOURCE_CHARACTERS, OpenAiCompatibleProvider,
    OpenAiProvider, ProviderId, TranslationContext, TranslationEntry as AiTranslationEntry,
    TranslationOptions, TranslationOutcome, TranslationProvider, translate_entries,
};
use yc_language::{EntryFlag, TranslationOrigin};
use yc_project::{
    AppSettings, DocumentProjectEntry, EntryUpdate, OpenedProject, ProjectEntry,
    TranslationEntryKind, UpdateOrigin, load_settings,
};
use yc_validation::PlaceholderProfile;

use crate::{
    commands::editor::EditorEntry,
    error::AppError,
    plamo,
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
const GEMINI_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai/";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";
const DEEPSEEK_URL: &str = "https://api.deepseek.com";
const GROQ_URL: &str = "https://api.groq.com/openai/v1";
const OLLAMA_URL: &str = "http://localhost:11434/v1";
const LM_STUDIO_URL: &str = "http://localhost:1234/v1";
const ANTHROPIC_URL: &str = "https://api.anthropic.com";

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
    /// PLaMo だけが使うローカル実行デバイス。他のプロバイダーには出さない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum AiTargetSupportLevel {
    Supported,
    Limited,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTargetSupport {
    level: AiTargetSupportLevel,
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
    /// PLaMo だけが使う。他のプロバイダーでは無視する。
    #[serde(default)]
    pub device: Option<String>,
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
        entries: Vec<EditorEntry>,
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
    shader_format: bool,
}

struct AiSourceEntry {
    id: i64,
    mod_unit_id: i64,
    namespace: String,
    key: String,
    source_text: Option<String>,
    source_locale: Option<String>,
    format_profile: PlaceholderProfile,
    shader_format: bool,
}

struct AiJobRunContext<'a> {
    job_id: &'a str,
    channel: &'a Channel<AiJobEvent>,
    active_project: &'a Arc<std::sync::Mutex<Option<OpenedProject>>>,
    project_id: &'a str,
    config: &'a AiSettings,
    provider_id: ProviderId,
    provider: Arc<dyn TranslationProvider>,
    cancellation: CancellationToken,
    apply_gate: Arc<std::sync::Mutex<()>>,
    /// PLaMo のときだけ入る。適用時に利用記録（`ai_jobs`）へ残すモデル ID。
    plamo_model_id: Option<String>,
}

#[tauri::command]
pub async fn get_ai_settings(
    provider: Option<String>,
    state: State<'_, AppState>,
) -> Result<AiSettings, AppError> {
    // 既定を決める前に移行の完了を待つ。待たずに読むと、移行が保存する前に
    // 「未保存 = PLaMo」を返し、画面がその結果を保持してしまう。
    if provider.is_none() {
        migrate_default_ai_provider(&state).await;
    }
    let settings = load_settings(&state.paths.settings_file).map_err(settings_error)?;
    let provider = match provider {
        Some(provider) => parse_provider(&provider)?,
        None => provider_from_settings(&settings)?,
    };
    // PLaMo は資格情報を使わないため、ストアの状態を一切参照しない。壊れた Secret Service が
    // ローカル翻訳の設定表示を壊さないようにするための分岐。
    if !provider.uses_credential_store() {
        let configured = plamo::configured(&state.plamo, &settings);
        return Ok(ai_settings_from(&settings, provider, configured, None));
    }
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
pub async fn get_ai_target_support(
    target_locale: String,
    state: State<'_, AppState>,
) -> Result<AiTargetSupport, AppError> {
    // 対応可否は既定プロバイダーで変わるため、移行の完了を待ってから判定する。
    migrate_default_ai_provider(&state).await;
    let settings = load_settings(&state.paths.settings_file).map_err(settings_error)?;
    let provider = provider_from_settings(&settings)?;
    // PLaMo の対応言語はカタログ由来の固定表で決まるため、資格情報もネットワークも要らない。
    if !provider.uses_credential_store() {
        return Ok(AiTargetSupport {
            level: target_support_level(provider, &target_locale, None),
        });
    }
    let deepl_supported = if provider != ProviderId::DeepL
        || yc_ai::deepl::deepl_target_languages(&target_locale).is_none()
    {
        None
    } else if let Ok(Some(api_key)) =
        load_provider_credential(state.credentials.as_deref(), provider).await
    {
        match DeepLProvider::new(api_key) {
            Ok(client) => client.target_locale_support(&target_locale).await,
            Err(_) => None,
        }
    } else {
        None
    };
    Ok(AiTargetSupport {
        level: target_support_level(provider, &target_locale, deepl_supported),
    })
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
                &yc_locale::t!("ai.browserOpenFailed"),
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
        &yc_locale::t!("ai.chatgptLoginCancelled"),
    )
}

fn chatgpt_login_error(error: AiError) -> AppError {
    match error {
        AiError::ChatGptLoginCancelled | AiError::Cancelled => chatgpt_login_cancelled_error(),
        AiError::ChatGptCallbackPortInUse => AppError::simple(
            "chatgpt_callback_port_in_use",
            &yc_locale::t!("ai.chatgptCallbackPortInUse"),
        ),
        AiError::ChatGptLoginTimedOut => AppError::simple(
            "chatgpt_login_timed_out",
            &yc_locale::t!("ai.chatgptLoginTimedOut"),
        ),
        _ => AppError::simple(
            "chatgpt_login_failed",
            &yc_locale::t!("ai.chatgptLoginFailed"),
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
            &yc_locale::t!("ai.chatgptMustLogin"),
        ));
    }

    // PLaMo は API Key を持たない。資格情報ストアへは触らず、渡されても無視する。
    if provider.uses_credential_store()
        && let Some(key) = api_key.as_deref().filter(|key| !key.trim().is_empty())
    {
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
            if provider == ProviderId::Plamo {
                // PLaMo は接続先も追加指示も持たない。保存済みの古い値が残っていると
                // 画面の表示と実際の動作がずれるため、ここで消す。
                settings.values.remove(&setting_key(provider, "baseUrl"));
                settings
                    .values
                    .remove(&setting_key(provider, "customPrompt"));
                // 旧値 `gpu` が来ても Vulkan として保存し直す。読み替えは parse に閉じ込め、
                // 保存値としては常に現在の 3 値のいずれかにしておく。
                let device = config
                    .device
                    .as_deref()
                    .and_then(plamo::PlamoDevice::parse)
                    .unwrap_or_else(plamo::default_device);
                settings.values.insert(
                    setting_key(provider, "device"),
                    Value::String(device.as_str().to_owned()),
                );
            } else {
                settings.values.insert(
                    setting_key(provider, "customPrompt"),
                    Value::String(config.custom_prompt),
                );
            }
            // PLaMo は 1 リクエスト 1 項目なので送信件数を持たない。保存しない。
            if provider != ProviderId::Plamo {
                settings.values.insert(
                    setting_key(provider, "batchSize"),
                    Value::from(config.batch_size),
                );
            }
            settings.values.insert(
                setting_key(provider, "concurrency"),
                Value::from(config.concurrency),
            );
            Ok(())
        },
        settings_error,
        settings_error,
    )?;

    if !provider.uses_credential_store() {
        let configured = plamo::configured(&state.plamo, &settings);
        return Ok(ai_settings_from(&settings, provider, configured, None));
    }

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
    // PLaMo は資格情報を持たず、削除もモデル単位で行う。ここで設定を消すと画面の
    // 選択が既定へ戻るだけなので、何もせず成功として扱う。
    if !provider.uses_credential_store() {
        return Ok(());
    }
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
    if !provider.uses_credential_store() {
        return test_plamo(&state, &settings).await;
    }
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

/// 保存済み設定でサーバを起動し、`/health` と 1 回の生成を確かめる。
async fn test_plamo(state: &AppState, settings: &AppSettings) -> Result<(), AppError> {
    if !plamo::configured(&state.plamo, settings) {
        return Err(plamo::not_ready_error());
    }
    let config = plamo::run_config(&state.plamo, settings)?;
    let slots = setting_usize(settings, ProviderId::Plamo, "concurrency", 2).clamp(1, 4);
    let server_config = plamo::server_config(&config, slots)?;
    // 起動と使用中モデルの記録を削除と排他して行う。分けると、読み込み中のモデルを
    // 削除が「未使用」と誤認して消し得る。
    let lease = state
        .plamo
        .start_server(&server_config, config.model_path.clone())
        .await
        .map_err(|error| plamo::runtime_start_error(config.device, error))?;
    let provider = build_plamo_provider(&lease, None)?;
    provider
        .test_connection(&CancellationToken::new())
        .await
        .map_err(ai_error)
}

#[tauri::command]
pub async fn list_ai_models(
    provider: String,
    base_url: String,
    api_key: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<String>, AppError> {
    let provider = parse_provider(&provider)?;
    // PLaMo のモデル一覧はカタログ由来。ネットワークも資格情報も使わない。
    if provider == ProviderId::Plamo {
        return Ok(yc_plamo::models()
            .iter()
            .map(|model| model.id.to_owned())
            .collect());
    }
    if provider == ProviderId::ChatGpt {
        let fallback = || {
            CHATGPT_MODELS
                .iter()
                .map(|model| model.id.to_owned())
                .collect()
        };
        let Some(credentials) = state.credentials.as_deref() else {
            return Ok(fallback());
        };
        let Ok(Some(credential)) = load_provider_credential(Some(credentials), provider).await
        else {
            return Ok(fallback());
        };
        let Ok(client) =
            ChatGptProvider::new(&credential, DEFAULT_CHATGPT_MODEL, credentials.clone())
        else {
            return Ok(fallback());
        };
        return Ok(client
            .list_models(&CancellationToken::new())
            .await
            .unwrap_or_else(|_| fallback()));
    }
    let credential = match api_key.filter(|key| !key.trim().is_empty()) {
        Some(key) => Some(key),
        None => load_provider_credential(state.credentials.as_deref(), provider).await?,
    };
    let cancellation = CancellationToken::new();
    let models = match provider {
        ProviderId::OpenAi => {
            yc_ai::openai_compatible::list_models(
                DEFAULT_COMPATIBLE_URL,
                credential.as_deref(),
                &cancellation,
            )
            .await
        }
        ProviderId::OpenAiCompatible
        | ProviderId::Gemini
        | ProviderId::OpenRouter
        | ProviderId::DeepSeek
        | ProviderId::Groq
        | ProviderId::Ollama
        | ProviderId::LmStudio => {
            yc_ai::openai_compatible::list_models(
                provider_base_url(provider, &base_url),
                credential.as_deref(),
                &cancellation,
            )
            .await
        }
        ProviderId::Anthropic => match credential.as_deref() {
            Some(api_key) => AnthropicProvider::list_models(api_key, &cancellation).await,
            None => Err(AiError::InvalidConfiguration),
        },
        ProviderId::DeepL | ProviderId::ChatGpt | ProviderId::Plamo => Ok(Vec::new()),
    };
    models.map_err(ai_error)
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
            &yc_locale::t!("ai.noEntries"),
        ));
    }

    let deleting_projects = state
        .deleting_projects
        .lock()
        .map_err(|_| internal_state_error("Project deletion state"))?;
    if deleting_projects.contains(&expected_project_id) {
        return Err(AppError::simple(
            "project_delete_in_progress",
            &yc_locale::t!("ai.projectDeleting"),
        ));
    }

    // PLaMo はローカル実行なので、ジョブを作る前に翻訳先と実行環境を確かめる。ここで落とせば
    // サーバを起動せずに済み、UI は開始直後に理由を受け取れる。
    if provider == ProviderId::Plamo {
        let settings = load_settings(&state.paths.settings_file).map_err(settings_error)?;
        validate_plamo_job_start(&state, &settings, &expected_project_id)?;
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
    let plamo_state = Arc::clone(&state.plamo);
    let job_tokens = Arc::clone(&state.ai_job_tokens);
    let event_channel = channel.clone();
    let total = selected_ids.len();
    let returned_job_id = job_id.clone();
    tauri::async_runtime::spawn(async move {
        // PLaMo のサーバリースはジョブ全体で保持する。`ServerLease` を落とすとアイドル停止の
        // 計測が始まり、長いジョブの途中でサーバが落ちてしまう。
        let mut plamo_lease: Option<yc_llama::ServerLease> = None;
        let result = async {
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let settings = load_settings(&settings_file).map_err(settings_error)?;
            let ai_settings = ai_settings_from(&settings, provider, true, None);
            // PLaMo はサーバの用意と項目の準備を分けて行う。Provider へ渡すシェーダー設定の
            // 判定には準備後の項目一覧が要るため、サーバを先に立ててから組み立てる。
            let mut plamo_model_id = String::new();
            if provider == ProviderId::Plamo {
                let config = plamo::run_config(&plamo_state, &settings)?;
                let server_config = plamo::server_config(&config, ai_settings.concurrency)?;
                // 起動と使用中モデルの記録を削除と排他して行う。分けると、読み込み中の
                // モデルを削除が「未使用」と誤認して消し得る。
                let lease = plamo_state
                    .start_server(&server_config, config.model_path.clone())
                    .await
                    .map_err(|error| plamo::runtime_start_error(config.device, error))?;
                tracing::info!(
                    model_id = config.model_id,
                    device = config.device.as_str(),
                    slots = server_config.slots,
                    "plamo server started"
                );
                plamo_model_id = config.model_id;
                plamo_lease = Some(lease);
            }

            let prepare_active_project = Arc::clone(&active_project);
            let prepare_cancellation = cancellation.clone();
            let selected = selected_ids.clone();
            let expected = expected_by_id.clone();
            let mod_names = mod_names_by_unit_and_namespace.clone();
            let project_id_expected = expected_project_id.clone();
            let (project_id, entries) = tauri::async_runtime::spawn_blocking(move || {
                prepare_ai_job_entries(
                    &prepare_active_project,
                    &selected,
                    &expected,
                    &mod_names,
                    &project_id_expected,
                    &prepare_cancellation,
                )
            })
            .await
            .map_err(|error| task_error(error.to_string()))??;
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }

            let client: Arc<dyn TranslationProvider> = if provider == ProviderId::Plamo {
                let lease = plamo_lease.as_ref().ok_or_else(not_configured_error)?;
                // シェーダーパックの設定項目は UI ラベルと語調が違う。どの項目が該当するかを
                // ここで確定させ、Provider には判定だけを渡す。
                let shader_entries = entries
                    .iter()
                    .filter(|entry| entry.shader_format)
                    .map(|entry| entry.translation.id)
                    .collect::<HashSet<_>>();
                Arc::from(build_plamo_provider(
                    lease,
                    (!shader_entries.is_empty()).then(|| Arc::new(shader_entries)),
                )?)
            } else {
                let credential = load_provider_credential(credentials.as_deref(), provider).await?;
                if cancellation.is_cancelled() {
                    return Err(cancelled_error());
                }
                let configured = provider_is_configured(&settings, provider, credential.is_some());
                if !configured {
                    return Err(not_configured_error());
                }
                Arc::from(build_provider(
                    provider,
                    &ai_settings,
                    credential,
                    credentials.clone(),
                )?)
            };

            let context = AiJobRunContext {
                job_id: &job_id,
                channel: &event_channel,
                active_project: &active_project,
                project_id: &project_id,
                config: &ai_settings,
                provider_id: provider,
                provider: client,
                cancellation: cancellation.clone(),
                apply_gate: Arc::clone(&apply_gate),
                plamo_model_id: (provider == ProviderId::Plamo).then_some(plamo_model_id),
            };
            run_ai_job(context, entries).await
        }
        .await;
        // リースを落としてよい。停止はマネージャのアイドル判定に任せ、直後の再実行で
        // モデルを読み直さずに済むようにする。
        drop(plamo_lease);
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

/// PLaMo のジョブを始められるか、サーバを起動する前に確かめる。
///
/// 翻訳先の対応表と実行環境のどちらも、サーバ起動（数秒〜数十秒）より前に判定したい。
/// 失敗の理由は利用者がそのまま次の操作へ移れる文言にする。
fn validate_plamo_job_start(
    state: &AppState,
    settings: &AppSettings,
    expected_project_id: &str,
) -> Result<(), AppError> {
    let active = state
        .active_project
        .lock()
        .map_err(|_| internal_state_error("active Project state"))?;
    let opened = active.as_ref().ok_or_else(|| {
        AppError::simple("project_required", &yc_locale::t!("ai.projectRequired"))
    })?;
    if opened.summary().id != expected_project_id {
        return Err(project_changed_error());
    }
    let target_locale = opened
        .repository()
        .target_locale()
        .map_err(|error| database_error(error.to_string()))?;
    if yc_plamo::plamo_code_for_locale(&target_locale).is_none() {
        return Err(AppError::simple(
            "ai_target_unsupported",
            &yc_locale::t!(
                "ai.targetUnsupportedByProvider",
                provider = provider_display_name(ProviderId::Plamo)
            ),
        ));
    }
    drop(active);
    // ライセンス未同意・モデル未取得・ランタイム未取得は、それぞれ別の案内になる。
    plamo::run_config(&state.plamo, settings).map(|_| ())
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
    let opened = active.as_ref().ok_or_else(|| {
        AppError::simple("project_required", &yc_locale::t!("ai.projectRequired"))
    })?;
    if opened.summary().id != expected_project_id {
        return Err(project_changed_error());
    }
    // 翻訳元とnamespaceはProject DBから読み、Mod名は解析済み情報をIPCで受け取ってJAR再解析を省く。
    let repository = opened.repository();
    let target_locale = repository
        .target_locale()
        .map_err(|error| database_error(error.to_string()))?;
    let shaderpack_unit_ids: HashSet<_> = repository
        .list_mod_units()
        .map_err(|error| database_error(error.to_string()))?
        .into_iter()
        .filter(|unit| unit.source_kind == yc_project::ModUnitSourceKind::Shaderpack)
        .map(|unit| unit.id)
        .collect();
    let mut stored_entries: Vec<_> = repository
        .list_entries(None)
        .map_err(|error| database_error(error.to_string()))?
        .into_iter()
        .map(|entry| {
            let format_profile = if entry.flags.contains(&EntryFlag::PatchouliText) {
                PlaceholderProfile::Patchouli
            } else {
                PlaceholderProfile::Lang
            };
            AiSourceEntry {
                id: entry.id,
                mod_unit_id: entry.mod_unit_id,
                namespace: entry.namespace,
                key: entry.key,
                source_text: entry.source_text,
                source_locale: entry.source_locale,
                format_profile,
                shader_format: shaderpack_unit_ids.contains(&entry.mod_unit_id),
            }
        })
        .collect();
    stored_entries.extend(
        repository
            .list_document_entries(None)
            .map_err(|error| database_error(error.to_string()))?
            .into_iter()
            .map(|entry| {
                let format_profile = match entry.entry_kind {
                    TranslationEntryKind::Patchouli => PlaceholderProfile::Patchouli,
                    TranslationEntryKind::Ftbquests => PlaceholderProfile::FtbQuests,
                    TranslationEntryKind::Lang => PlaceholderProfile::Lang,
                };
                AiSourceEntry {
                    id: entry.id,
                    mod_unit_id: entry.mod_unit_id,
                    namespace: entry.namespace,
                    key: entry.key,
                    source_text: entry.source_text,
                    source_locale: entry.source_locale,
                    format_profile,
                    shader_format: false,
                }
            }),
    );
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
            AppError::simple("entry_not_found", &yc_locale::t!("ai.entryReadFailed"))
        })?;
        let expected = expected_by_id.get(id).ok_or_else(|| {
            AppError::simple("invalid_ai_selection", &yc_locale::t!("ai.entryReadFailed"))
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
            translation: AiTranslationEntry::new_with_profile(
                entry.id,
                entry.key.clone(),
                source,
                source_locale,
                &target_locale,
                entry.format_profile,
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
            shader_format: entry.shader_format,
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
        provider_id,
        provider,
        cancellation,
        apply_gate,
        plamo_model_id,
    } = context;
    let total = entries.len();
    // PLaMo は 1 リクエスト 1 項目しか扱えず、外側のバッチに複数入れると Mod 名・用語集・
    // 進捗が先頭項目のものに固定される。項目ごとに 1 バッチへ分け、並列度はスロット数で保つ。
    let batch_size = if plamo_model_id.is_some() {
        1
    } else {
        config.batch_size
    };
    let batches = split_job_batches(entries, batch_size);
    let completed = Arc::new(AtomicUsize::new(0));
    let concurrency = config.concurrency.clamp(1, 4);
    // 追加指示を渡すかどうかはバッチごとの分岐で参照するだけなので、事前に真偽値へ落とす。
    let is_plamo = plamo_model_id.is_some();
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
                // PLaMo は指示追従モデルではないため、追加指示は渡さない。
                custom_prompt: (!is_plamo).then_some(custom_prompt),
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
        let outcome = outcome.map_err(|error| ai_error_for_provider(error, provider_id))?;
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
            let (applied, rejected) = apply_outcome_to_project(
                opened,
                &batch,
                &outcome,
                &cancellation,
                plamo_model_id.as_ref().map(|model| AiJobRecord {
                    job_id,
                    provider: ProviderId::Plamo.as_str(),
                    model,
                }),
            )?;
            let applied_ids: HashSet<_> = applied.iter().map(|entry| entry.id).collect();
            let repository = opened.repository();
            let mut editor_entries: Vec<_> = repository
                .list_entries(None)
                .map_err(|error| database_error(error.to_string()))?
                .into_iter()
                .filter(|entry| applied_ids.contains(&entry.id))
                .map(EditorEntry::from)
                .collect();
            editor_entries.extend(
                repository
                    .list_document_entries(None)
                    .map_err(|error| database_error(error.to_string()))?
                    .into_iter()
                    .filter(|entry| applied_ids.contains(&entry.id))
                    .map(EditorEntry::from),
            );
            (editor_entries, rejected)
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

/// 適用と同時に `ai_jobs` へ残す利用記録。PLaMo のときだけ渡す。
struct AiJobRecord<'a> {
    job_id: &'a str,
    provider: &'a str,
    model: &'a str,
}

fn apply_outcome_to_project(
    opened: &OpenedProject,
    batch: &[AiJobEntry],
    outcome: &TranslationOutcome,
    cancellation: &CancellationToken,
    ai_job: Option<AiJobRecord<'_>>,
) -> Result<(Vec<ProjectEntry>, Vec<AiRejectedEntry>), AppError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    let repository = opened.repository();
    let mut current: Vec<_> = repository
        .list_entries(None)
        .map_err(|error| database_error(error.to_string()))?;
    current.extend(
        repository
            .list_document_entries(None)
            .map_err(|error| database_error(error.to_string()))?
            .into_iter()
            .map(document_entry_as_project_entry),
    );
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
    let shader_format_error_ids: HashSet<_> = updates
        .iter()
        .filter_map(|update| {
            let entry = requested_by_id.get(&update.id)?;
            (entry.shader_format
                && yc_validation::shader_format_warning(
                    &entry.translation.source,
                    &update.target_text,
                ))
            .then_some(update.id)
        })
        .collect();
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    if let Some(record) = ai_job {
        // 訳文の反映と利用記録を同じトランザクションで書く。分けると、記録だけ残って
        // 訳文が入らない（またはその逆）状態になり、書き出しのライセンス表記がずれる。
        opened
            .writer()
            .update_entries_recording_ai_job(
                updates.clone(),
                yc_project::AiJobApplication {
                    job_id: record.job_id.to_owned(),
                    provider: record.provider.to_owned(),
                    model: record.model.to_owned(),
                    applied_entries: updates.len(),
                },
            )
            .map_err(|error| database_error(error.to_string()))?;
    } else if !updates.is_empty() {
        opened
            .writer()
            .update_entries(updates.clone())
            .map_err(|error| database_error(error.to_string()))?;
    }

    for update in &updates {
        if shader_format_error_ids.contains(&update.id) {
            opened
                .writer()
                .add_flag(update.id, EntryFlag::PlaceholderError)
                .map_err(|error| database_error(error.to_string()))?;
        } else {
            opened
                .writer()
                .remove_flag(update.id, EntryFlag::PlaceholderError)
                .map_err(|error| database_error(error.to_string()))?;
        }
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
            if shader_format_error_ids.contains(&update.id) {
                stored.flags.push(EntryFlag::PlaceholderError);
            }
            Some(stored.clone())
        })
        .collect();
    Ok((applied, rejected))
}

fn document_entry_as_project_entry(entry: DocumentProjectEntry) -> ProjectEntry {
    ProjectEntry {
        id: entry.id,
        mod_unit_id: entry.mod_unit_id,
        namespace: entry.namespace,
        key: entry.key,
        source_text: entry.source_text,
        source_locale: entry.source_locale,
        target_text: entry.target_text,
        target_original_text: entry.target_original_text,
        translated_source_text: entry.translated_source_text,
        origin: entry.origin,
        sort_index: entry.sort_index,
        flags: entry.flags,
    }
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
    ai_settings_with_device_default(
        settings,
        provider,
        configured,
        account_label,
        plamo::default_device_str,
    )
}

/// デバイスの既定値を注入して設定表示を組み立てる。
///
/// 既定デバイスは実機の GPU 検出（`nvidia-smi` と Vulkan ローダー）に依存する。既定値を
/// 遅延評価にし、注入できるようにしてあるのは、保存済みのデバイスがあるときや単体テストが
/// 実機を見ずに済むようにするため。既定値は PLaMo かつデバイス未保存のときだけ引く。
fn ai_settings_with_device_default(
    settings: &AppSettings,
    provider: ProviderId,
    configured: bool,
    account_label: Option<String>,
    device_default: impl FnOnce() -> &'static str,
) -> AiSettings {
    let model_default = match provider {
        ProviderId::OpenAi => DEFAULT_OPENAI_MODEL,
        ProviderId::ChatGpt => DEFAULT_CHATGPT_MODEL,
        ProviderId::Plamo => yc_plamo::DEFAULT_MODEL_ID,
        _ => "",
    };
    AiSettings {
        provider: provider.as_str().to_owned(),
        configured,
        account_label,
        model: setting_string(settings, provider, "model", model_default),
        base_url: if provider == ProviderId::OpenAiCompatible {
            setting_string(settings, provider, "baseUrl", DEFAULT_COMPATIBLE_URL)
        } else {
            provider_base_url(provider, DEFAULT_COMPATIBLE_URL).to_owned()
        },
        batch_size: setting_usize(settings, provider, "batchSize", MAX_BATCH_ENTRIES),
        concurrency: setting_usize(settings, provider, "concurrency", 2).clamp(1, 4),
        custom_prompt: setting_string(settings, provider, "customPrompt", ""),
        // PLaMo だけがデバイスを持つ。他のプロバイダーへ出すと画面側の契約が増えるため省く。
        device: (provider == ProviderId::Plamo)
            .then(|| setting_string_or_else(settings, provider, "device", device_default)),
    }
}

fn provider_is_configured(
    settings: &AppSettings,
    provider: ProviderId,
    has_credential: bool,
) -> bool {
    if provider == ProviderId::ChatGpt {
        let model = setting_string(settings, provider, "model", DEFAULT_CHATGPT_MODEL);
        return has_credential && yc_ai::chatgpt::valid_model_id(&model);
    }
    if provider == ProviderId::DeepL {
        return has_credential;
    }
    // PLaMo は資格情報ではなく、モデル・ランタイム・ライセンス同意で決まる。
    // この関数は資格情報しか見られないため、呼び出し側が別経路で判定する。
    if provider == ProviderId::Plamo {
        return false;
    }
    let config = ai_settings_from(settings, provider, false, None);
    if provider_requires_api_key(provider) && !has_credential {
        return false;
    }
    match provider {
        ProviderId::OpenAi => {
            has_credential && OpenAiProvider::new("configured", config.model).is_ok()
        }
        ProviderId::Anthropic => {
            has_credential && AnthropicProvider::new("configured", config.model).is_ok()
        }
        ProviderId::OpenAiCompatible
        | ProviderId::Gemini
        | ProviderId::OpenRouter
        | ProviderId::DeepSeek
        | ProviderId::Groq
        | ProviderId::Ollama
        | ProviderId::LmStudio => OpenAiCompatibleProvider::new(
            provider_base_url(provider, &config.base_url),
            None,
            config.model,
        )
        .is_ok(),
        ProviderId::DeepL | ProviderId::ChatGpt | ProviderId::Plamo => false,
    }
}

fn target_support_level(
    provider: ProviderId,
    target_locale: &str,
    deepl_supported: Option<bool>,
) -> AiTargetSupportLevel {
    if provider == ProviderId::Plamo {
        // PLaMo の対応言語はカタログの固定表で決まる。未対応なら翻訳そのものを開始しない。
        if yc_plamo::plamo_code_for_locale(target_locale).is_none() {
            AiTargetSupportLevel::Unsupported
        } else {
            AiTargetSupportLevel::Supported
        }
    } else if provider == ProviderId::DeepL {
        if yc_ai::deepl::deepl_target_languages(target_locale).is_none()
            || deepl_supported == Some(false)
        {
            AiTargetSupportLevel::Unsupported
        } else {
            AiTargetSupportLevel::Supported
        }
    } else if yc_locale::is_constructed_language(target_locale) {
        AiTargetSupportLevel::Limited
    } else {
        AiTargetSupportLevel::Supported
    }
}

fn provider_requires_api_key(provider: ProviderId) -> bool {
    matches!(
        provider,
        ProviderId::OpenAi
            | ProviderId::Gemini
            | ProviderId::OpenRouter
            | ProviderId::DeepSeek
            | ProviderId::Groq
            | ProviderId::Anthropic
            | ProviderId::DeepL
    )
}

fn provider_base_url(provider: ProviderId, custom_url: &str) -> &str {
    match provider {
        ProviderId::OpenAi => DEFAULT_COMPATIBLE_URL,
        ProviderId::OpenAiCompatible => custom_url,
        ProviderId::Gemini => GEMINI_URL,
        ProviderId::OpenRouter => OPENROUTER_URL,
        ProviderId::DeepSeek => DEEPSEEK_URL,
        ProviderId::Groq => GROQ_URL,
        ProviderId::Ollama => OLLAMA_URL,
        ProviderId::LmStudio => LM_STUDIO_URL,
        ProviderId::Anthropic => ANTHROPIC_URL,
        // PLaMo はローカルのサーバへ繋ぐため、利用者が指定する接続先を持たない。
        ProviderId::Plamo => "",
        ProviderId::DeepL | ProviderId::ChatGpt => custom_url,
    }
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

/// 既定値を遅延評価する `setting_string`。既定値の計算が高価なとき（実機の GPU 検出など）や、
/// 保存値があるときに評価したくないときに使う。
fn setting_string_or_else(
    settings: &AppSettings,
    provider: ProviderId,
    field: &str,
    default: impl FnOnce() -> &'static str,
) -> String {
    settings
        .values
        .get(&setting_key(provider, field))
        .and_then(Value::as_str)
        .map_or_else(|| default().to_owned(), str::to_owned)
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

/// 移行時に資格情報を調べる順序。`save_api_key` が受け付けるプロバイダーをすべて含める。
/// ログインだけで使い始められる ChatGPT を先頭に置き、複数の資格情報が残っている場合は
/// そちらを優先する。Ollama と LM Studio は画面に API キー欄を出さないが、ストアには
/// 保存できるため、取りこぼすと保存済みの設定が新しい既定へ置き換わってしまう。
const CREDENTIAL_PROVIDERS_BY_MIGRATION_PRIORITY: &[ProviderId] = &[
    ProviderId::ChatGpt,
    ProviderId::OpenAi,
    ProviderId::Anthropic,
    ProviderId::Gemini,
    ProviderId::OpenRouter,
    ProviderId::DeepSeek,
    ProviderId::Groq,
    ProviderId::OpenAiCompatible,
    ProviderId::DeepL,
    ProviderId::Ollama,
    ProviderId::LmStudio,
];

/// 既定プロバイダーを PLaMo へ変えたことによる、一度きりの起動時移行。
///
/// `ai.provider` を保存していない利用者は、資格情報だけを保存していることがある。既定を
/// 変えただけでその設定を黙って置き換えないよう、保存済みの資格情報から使っていた
/// プロバイダーを推定して `ai.provider` に書き戻す。資格情報ストアが使えない環境
/// （Linux で Secret Service が無い等）や資格情報が無い場合は何も保存せず、新しい既定
/// （PLaMo）のままにする。資格情報そのものは読むだけで、ログにも出さない。
pub(crate) async fn migrate_default_ai_provider(state: &AppState) {
    // 起動時のタスクと最初の設定読み込みが同じセルを通る。先に始めた側だけが実際に判定し、
    // 後から来た側はその完了を待つ。待つのは読み込み側だけで、起動は止めない。
    state
        .ai_provider_migration
        .get_or_init(|| async {
            migrate_default_ai_provider_at(
                &state.paths.settings_file,
                state.credentials.as_deref(),
            )
            .await;
        })
        .await;
}

async fn migrate_default_ai_provider_at(
    settings_file: &std::path::Path,
    credentials: Option<&CredentialManager>,
) {
    // 保存済みの提供元が最優先なので、資格情報ストアより先に設定を見る。
    match load_settings(settings_file) {
        Ok(settings) if settings.values.contains_key("ai.provider") => {
            tracing::info!("AI provider migration not needed because a provider is already saved");
            return;
        }
        Ok(_) => {}
        Err(error) => {
            tracing::info!(%error, "AI provider migration skipped because settings could not be read");
            return;
        }
    }
    let Some(credentials) = credentials else {
        tracing::info!(
            "AI credential store is unavailable, so the default AI provider stays PLaMo"
        );
        return;
    };
    for provider in CREDENTIAL_PROVIDERS_BY_MIGRATION_PRIORITY {
        match credentials.status(*provider).await {
            Ok(status) if status.configured => {
                persist_migrated_provider(settings_file, *provider);
                return;
            }
            Ok(_) => {}
            Err(error) => {
                // 壊れたストアでは「資格情報が無い」と区別できないため、何も保存しない。
                tracing::info!(%error, "AI provider migration skipped because the credential store could not be read");
                return;
            }
        }
    }
    tracing::info!("no stored AI credential was found, so the default AI provider stays PLaMo");
}

fn persist_migrated_provider(settings_file: &std::path::Path, provider: ProviderId) {
    let result = super::settings::update_settings_file(
        settings_file,
        |settings| {
            // 判定中に利用者が保存した選択を上書きしない。
            settings
                .values
                .entry("ai.provider".to_owned())
                .or_insert_with(|| Value::String(provider.as_str().to_owned()));
            Ok(())
        },
        settings_error,
        settings_error,
    );
    match result {
        Ok(_) => tracing::info!(
            provider = provider.as_str(),
            "kept the AI provider that already has a stored credential"
        ),
        Err(error) => tracing::info!(
            error_code = error.code,
            "AI provider migration could not save the settings file"
        ),
    }
}

fn provider_from_settings(settings: &AppSettings) -> Result<ProviderId, AppError> {
    let provider = settings
        .values
        .get("ai.provider")
        .and_then(Value::as_str)
        // 既定は PLaMo。この PC 内だけで動き、無料で、アカウントも要らず、原文が PC の外へ出ない。
        // モデルとランタイムは自動取得しない（設定画面でダウンロードを選んだときだけ取得する）。
        .unwrap_or("plamo");
    parse_provider(provider)
}

fn parse_provider(provider: &str) -> Result<ProviderId, AppError> {
    match provider {
        "openai" => Ok(ProviderId::OpenAi),
        "openai-compatible" => Ok(ProviderId::OpenAiCompatible),
        "deepl" => Ok(ProviderId::DeepL),
        "chatgpt" => Ok(ProviderId::ChatGpt),
        "gemini" => Ok(ProviderId::Gemini),
        "openrouter" => Ok(ProviderId::OpenRouter),
        "deepseek" => Ok(ProviderId::DeepSeek),
        "groq" => Ok(ProviderId::Groq),
        "ollama" => Ok(ProviderId::Ollama),
        "lm-studio" => Ok(ProviderId::LmStudio),
        "anthropic" => Ok(ProviderId::Anthropic),
        "plamo" => Ok(ProviderId::Plamo),
        _ => Err(AppError::simple(
            "invalid_ai_provider",
            &yc_locale::t!("ai.providerSelect"),
        )),
    }
}

fn validate_ai_config(config: &SaveAiProviderInput) -> Result<(), AppError> {
    let provider = parse_provider(&config.provider)?;
    if !(1..=4).contains(&config.concurrency) {
        return Err(AppError::simple(
            "invalid_ai_settings",
            &yc_locale::t!("ai.settingsCheck"),
        ));
    }
    // PLaMo はカタログのモデル ID とデバイスだけを受け付ける。接続先・追加指示・
    // 送信件数は持たないため、値が来ても検証しない（画面は空文字や既定値を送る）。
    if provider == ProviderId::Plamo {
        if yc_plamo::find_model(config.model.trim()).is_none() {
            return Err(AppError::simple(
                "invalid_ai_settings",
                &yc_locale::t!("ai.modelRequired"),
            ));
        }
        if let Some(device) = config.device.as_deref()
            && plamo::PlamoDevice::parse(device).is_none()
        {
            return Err(plamo::invalid_settings_error());
        }
        return Ok(());
    }
    if config.batch_size == 0 || config.batch_size > MAX_BATCH_ENTRIES {
        return Err(AppError::simple(
            "invalid_ai_settings",
            &yc_locale::t!("ai.settingsCheck"),
        ));
    }
    if provider != ProviderId::DeepL && config.model.trim().is_empty() {
        return Err(AppError::simple(
            "invalid_ai_settings",
            &yc_locale::t!("ai.modelRequired"),
        ));
    }
    if provider == ProviderId::ChatGpt && !yc_ai::chatgpt::valid_model_id(&config.model) {
        return Err(AppError::simple(
            "invalid_ai_settings",
            &yc_locale::t!("ai.chatgptModelSelect"),
        ));
    }
    if matches!(
        provider,
        ProviderId::OpenAiCompatible
            | ProviderId::Gemini
            | ProviderId::OpenRouter
            | ProviderId::DeepSeek
            | ProviderId::Groq
            | ProviderId::Ollama
            | ProviderId::LmStudio
    ) {
        OpenAiCompatibleProvider::new(
            provider_base_url(provider, &config.base_url),
            None,
            &config.model,
        )
        .map_err(ai_error)?;
    }
    if provider == ProviderId::Anthropic {
        AnthropicProvider::new("validation-key", &config.model).map_err(ai_error)?;
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
        ProviderId::Gemini
        | ProviderId::OpenRouter
        | ProviderId::DeepSeek
        | ProviderId::Groq
        | ProviderId::Ollama
        | ProviderId::LmStudio => Box::new(
            OpenAiCompatibleProvider::new(
                provider_base_url(provider, &config.base_url),
                credential,
                config.model.clone(),
            )
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
        ProviderId::Anthropic => Box::new(
            AnthropicProvider::new(
                credential.ok_or_else(not_configured_error)?,
                config.model.clone(),
            )
            .map_err(ai_error)?,
        ),
        // PLaMo は起動済みサーバの情報が必要なため、`start_ai_job` が別経路で組み立てる。
        ProviderId::Plamo => return Err(not_configured_error()),
    };
    Ok(client)
}

/// 起動中のサーバを指す PLaMo Provider を作る。
///
/// `shader_entries` はシェーダーパック由来の項目 ID。PFML3 のコンテキストに
/// 「設定項目名」の語調を入れるためだけに使い、原文や訳文は渡さない。
fn build_plamo_provider(
    lease: &yc_llama::ServerLease,
    shader_entries: Option<Arc<HashSet<i64>>>,
) -> Result<Box<dyn TranslationProvider>, AppError> {
    let options = yc_plamo::PlamoProviderOptions {
        shader_option_detector: shader_entries.map(|ids| {
            Arc::new(move |entry: &AiTranslationEntry| ids.contains(&entry.id))
                as yc_plamo::ShaderOptionDetector
        }),
        ..Default::default()
    };
    let provider =
        yc_plamo::PlamoProvider::new(lease.base_url(), Some(lease.api_key().to_owned()), options)
            .map_err(ai_error)?;
    Ok(Box::new(provider))
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
        ValidationIssue::MissingResponse => yc_locale::t!("ai.rejectionMissingResponse"),
        ValidationIssue::DuplicateId => yc_locale::t!("ai.rejectionDuplicate"),
        ValidationIssue::UnknownId => yc_locale::t!("ai.rejectionUnknown"),
        ValidationIssue::EmptyTranslation => yc_locale::t!("ai.rejectionEmpty"),
        ValidationIssue::PlaceholderMismatch { .. } => yc_locale::t!("ai.rejectionPlaceholder"),
        ValidationIssue::InvalidControlCharacter { .. } => {
            yc_locale::t!("ai.rejectionControlCharacter")
        }
    }
}

fn settings_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "settings_unavailable",
        &yc_locale::t!("ai.settingsReadWrite"),
        Some(error.to_string()),
    )
}

fn credential_error(error: AiError) -> AppError {
    #[cfg(target_os = "linux")]
    let user_message = match &error {
        AiError::CredentialStore => yc_locale::t!("ai.credentialsLinuxReadWrite"),
        _ => yc_locale::t!("ai.credentialsReadWrite"),
    };
    #[cfg(not(target_os = "linux"))]
    let user_message = yc_locale::t!("ai.credentialsReadWrite");

    AppError::new(
        "ai_credential_error",
        &user_message,
        Some(error.to_string()),
    )
}

fn credential_unavailable() -> AppError {
    AppError::simple(
        "ai_credential_unavailable",
        &yc_locale::t!("ai.credentialsUnavailable"),
    )
}

fn not_configured_error() -> AppError {
    AppError::simple("ai_not_configured", &yc_locale::t!("ai.notConfigured"))
}

fn ai_error(error: AiError) -> AppError {
    let (code, user_message) = match &error {
        AiError::JobAlreadyRunning => ("ai_job_running", yc_locale::t!("ai.jobAlreadyRunning")),
        AiError::Cancelled => ("ai_job_cancelled", yc_locale::t!("ai.cancelled")),
        AiError::InvalidUrl => ("ai_invalid_url", yc_locale::t!("ai.invalidProviderUrl")),
        AiError::ChatGptReauthenticationRequired => (
            "chatgpt_reauthentication_required",
            yc_locale::t!("ai.chatgptReauthenticationRequired"),
        ),
        AiError::UnsupportedTargetLanguage => (
            "ai_target_unsupported",
            yc_locale::t!("ai.deeplTargetUnsupported"),
        ),
        _ => ("ai_provider_error", yc_locale::t!("ai.providerFailed")),
    };
    AppError::new(code, &user_message, Some(error.to_string()))
}

/// `ai_error` のプロバイダー別版。未対応の翻訳先は DeepL 専用の文言ではなく、
/// 選んだプロバイダー名を差し込んだ文言にする。
fn ai_error_for_provider(error: AiError, provider: ProviderId) -> AppError {
    if matches!(error, AiError::UnsupportedTargetLanguage) && provider != ProviderId::DeepL {
        return AppError::new(
            "ai_target_unsupported",
            &yc_locale::t!(
                "ai.targetUnsupportedByProvider",
                provider = provider_display_name(provider)
            ),
            Some(error.to_string()),
        );
    }
    ai_error(error)
}

/// 利用者向けのプロバイダー名。翻訳キーではなく製品名なので、ここで固定する。
fn provider_display_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::OpenAi => "OpenAI",
        ProviderId::OpenAiCompatible => "OpenAI-compatible",
        ProviderId::DeepL => "DeepL",
        ProviderId::ChatGpt => "ChatGPT",
        ProviderId::Gemini => "Gemini",
        ProviderId::OpenRouter => "OpenRouter",
        ProviderId::DeepSeek => "DeepSeek",
        ProviderId::Groq => "Groq",
        ProviderId::Ollama => "Ollama",
        ProviderId::LmStudio => "LM Studio",
        ProviderId::Anthropic => "Claude",
        ProviderId::Plamo => "PLaMo",
    }
}

fn cancelled_error() -> AppError {
    AppError::simple("ai_job_cancelled", &yc_locale::t!("ai.cancelled"))
}

fn channel_error() -> AppError {
    AppError::simple(
        "ai_channel_closed",
        &yc_locale::t!("ai.progressUnavailable"),
    )
}

fn project_changed_error() -> AppError {
    AppError::simple("project_changed", &yc_locale::t!("ai.projectChanged"))
}

fn database_error(detail: impl Into<String>) -> AppError {
    AppError::new(
        "database",
        &yc_locale::t!("ai.dataSaveFailed"),
        Some(detail.into()),
    )
}

fn internal_state_error(detail: &str) -> AppError {
    AppError::new(
        "internal_state",
        &yc_locale::t!("ai.appStateFailed"),
        Some(detail.to_owned()),
    )
}

fn task_error(detail: String) -> AppError {
    AppError::new("task_failed", &yc_locale::t!("ai.taskFailed"), Some(detail))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        io::Write,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        AiEntryBaseline, AiJobEntry, AiSettings, AiTargetSupportLevel, SaveAiProviderInput,
        ai_error_for_provider, ai_settings_from, ai_settings_with_device_default,
        apply_outcome_to_project, migrate_default_ai_provider_at, parse_provider,
        prepare_ai_job_entries, provider_base_url, provider_from_settings, provider_is_configured,
        split_job_batches, target_support_level, validate_ai_config,
    };
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;
    use yc_ai::{
        AiError, CredentialBackend, CredentialManager, CredentialStoreError, ProviderId,
        TranslationOutcome,
    };
    use yc_language::{EntryFlag, LanguageFileFormat, TranslationOrigin};
    use yc_project::{
        AnalysisData, EntryUpdate, GlossaryTermInput, LanguageRecord, ModRecord, ModUnitSourceKind,
        NamespaceRecord, NestedArchiveRecord, ProjectEntry, ProjectSource, UpdateOrigin,
        create_project, create_project_with_sources_and_target_locale, open_project,
    };
    use yc_validation::{
        AiTranslation, PlaceholderProfile, ProfiledRequestedEntry, RequestedEntry,
        ValidatedTranslation, ValidationIssue, validate_ai_batch, validate_ai_batch_with_profiles,
    };
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

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

    #[cfg(target_os = "linux")]
    #[test]
    fn credential_store_error_suggests_starting_a_keyring_service() {
        let error = super::credential_error(AiError::CredentialStore);

        assert!(error.user_message.contains("GNOME Keyring"));
        assert!(error.user_message.contains("is running"));
    }

    /// 資格情報ストアが壊れている環境を再現する。PLaMo の経路がここへ触れないことを確かめる。
    #[derive(Default)]
    struct FailingCredentialStore;

    impl CredentialBackend for FailingCredentialStore {
        fn get(&mut self, _: ProviderId) -> Result<Option<String>, CredentialStoreError> {
            Err(CredentialStoreError)
        }

        fn set(&mut self, _: ProviderId, _: &str) -> Result<(), CredentialStoreError> {
            Err(CredentialStoreError)
        }

        fn delete(&mut self, _: ProviderId) -> Result<(), CredentialStoreError> {
            Err(CredentialStoreError)
        }
    }

    #[tokio::test]
    async fn a_broken_credential_store_still_reports_plamo_settings() {
        // Secret Service が落ちていても、PLaMo の設定表示は資格情報ストアを参照しないため
        // 失敗しない。`get_ai_settings` と同じ分岐をここで直接確かめる。
        let manager = CredentialManager::new(FailingCredentialStore).unwrap();
        assert!(manager.status(ProviderId::OpenAi).await.is_err());

        let mut settings = yc_project::AppSettings::new();
        settings.values.insert(
            "ai.plamo.model".into(),
            serde_json::Value::String(yc_plamo::DEFAULT_MODEL_ID.into()),
        );
        assert!(!ProviderId::Plamo.uses_credential_store());
        // 既定デバイスを注入し、実機の GPU 検出を呼ばない。
        let config =
            ai_settings_with_device_default(&settings, ProviderId::Plamo, false, None, || "cpu");
        assert_eq!(config.provider, "plamo");
        assert_eq!(config.model, yc_plamo::DEFAULT_MODEL_ID);
    }

    #[tokio::test]
    async fn plamo_settings_never_read_or_write_credentials() {
        // 資格情報を渡しても保存しない。壊れたストアでも PLaMo の保存は成立する。
        let manager = CredentialManager::new(FailingCredentialStore).unwrap();
        assert!(!ProviderId::Plamo.uses_credential_store());
        // 資格情報経路を使う他プロバイダーは同じストアで失敗する（分岐の対照）。
        assert!(manager.status(ProviderId::Anthropic).await.is_err());
    }

    fn settings_file_in(directory: &TemporaryDirectory) -> PathBuf {
        directory.0.join("settings.json")
    }

    fn saved_provider(settings_file: &std::path::Path) -> Option<String> {
        yc_project::load_settings(settings_file)
            .unwrap()
            .values
            .get("ai.provider")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }

    fn chatgpt_credential_json() -> String {
        serde_json::json!({
            "type": "oauth",
            "access": "access-token",
            "refresh": "refresh-token",
            "expires": 1_900_000_000_000u64,
            "accountId": "account-1",
            "email": "user@example.com",
        })
        .to_string()
    }

    #[tokio::test]
    async fn default_provider_is_plamo_when_nothing_was_saved() {
        let settings = yc_project::AppSettings::new();

        assert_eq!(
            provider_from_settings(&settings).unwrap(),
            ProviderId::Plamo
        );
    }

    #[tokio::test]
    async fn migration_keeps_a_stored_chatgpt_credential_as_the_provider() {
        // 既定を変えても、ログイン済みで ai.provider を持たない利用者の設定を黙って
        // 置き換えない。ChatGPT を他の資格情報より先に調べる。
        let temporary = TemporaryDirectory::create();
        let settings_file = settings_file_in(&temporary);
        let manager = CredentialManager::new(MemoryCredentialStore::default()).unwrap();
        manager
            .save_api_key(ProviderId::OpenAi, "openai-secret")
            .await
            .unwrap();
        manager
            .save_chatgpt_credential(&chatgpt_credential_json())
            .await
            .unwrap();

        migrate_default_ai_provider_at(&settings_file, Some(&manager)).await;

        assert_eq!(saved_provider(&settings_file).as_deref(), Some("chatgpt"));
    }

    #[tokio::test]
    async fn migration_keeps_any_other_stored_credential_as_the_provider() {
        let temporary = TemporaryDirectory::create();
        let settings_file = settings_file_in(&temporary);
        let manager = CredentialManager::new(MemoryCredentialStore::default()).unwrap();
        manager
            .save_api_key(ProviderId::DeepL, "deepl-secret")
            .await
            .unwrap();

        migrate_default_ai_provider_at(&settings_file, Some(&manager)).await;

        assert_eq!(saved_provider(&settings_file).as_deref(), Some("deepl"));
    }

    #[tokio::test]
    async fn migration_never_overrides_an_explicitly_saved_provider() {
        let temporary = TemporaryDirectory::create();
        let settings_file = settings_file_in(&temporary);
        yc_project::save_settings(
            &settings_file,
            &yc_project::AppSettings {
                schema_version: 1,
                values: [(
                    "ai.provider".to_owned(),
                    serde_json::Value::String("openai".to_owned()),
                )]
                .into_iter()
                .collect(),
            },
        )
        .unwrap();
        let manager = CredentialManager::new(MemoryCredentialStore::default()).unwrap();
        manager
            .save_chatgpt_credential(&chatgpt_credential_json())
            .await
            .unwrap();

        migrate_default_ai_provider_at(&settings_file, Some(&manager)).await;

        assert_eq!(saved_provider(&settings_file).as_deref(), Some("openai"));
    }

    #[tokio::test]
    async fn migration_saves_nothing_without_credentials_or_a_usable_store() {
        // 資格情報が無い新規利用者は新しい既定（PLaMo）のままにする。
        let temporary = TemporaryDirectory::create();
        let empty_file = settings_file_in(&temporary);
        let manager = CredentialManager::new(MemoryCredentialStore::default()).unwrap();
        migrate_default_ai_provider_at(&empty_file, Some(&manager)).await;
        assert_eq!(saved_provider(&empty_file), None);

        // Secret Service が無い環境では「資格情報が無い」と区別できないため、何も保存しない。
        let broken_file = temporary.0.join("broken-settings.json");
        let broken = CredentialManager::new(FailingCredentialStore).unwrap();
        migrate_default_ai_provider_at(&broken_file, Some(&broken)).await;
        assert_eq!(saved_provider(&broken_file), None);

        // ストア自体が作れなかった場合も同じ。
        let missing_store_file = temporary.0.join("no-store-settings.json");
        migrate_default_ai_provider_at(&missing_store_file, None).await;
        assert_eq!(saved_provider(&missing_store_file), None);
    }

    // Windows では並列に走るテストが同じ時刻を読むことがあり、時刻だけの名前だと別のテストと
    // ディレクトリを共有して互いの settings.json を上書きする。連番で必ず別の場所にする。
    static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn create() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("yakucraft-ai-{}-{nonce}-{id}", std::process::id()));
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
        project_with_entry_target(yc_language::DEFAULT_TARGET_LOCALE)
    }

    fn project_with_entry_target(
        target_locale: &str,
    ) -> (TemporaryDirectory, yc_project::OpenedProject, ProjectEntry) {
        let temporary = TemporaryDirectory::create();
        let source_path = temporary.0.join("source.jar");
        fs::write(&source_path, b"test JAR").unwrap();
        let summary = create_project_with_sources_and_target_locale(
            temporary.0.join("projects"),
            &[ProjectSource {
                path: source_path.clone(),
                kind: ModUnitSourceKind::Jar,
            }],
            None,
            None,
            target_locale,
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

    fn legacy_patchouli_project_with_entry()
    -> (TemporaryDirectory, yc_project::OpenedProject, ProjectEntry) {
        let temporary = TemporaryDirectory::create();
        let source_path = temporary.0.join("patchouli.jar");
        let file = fs::File::create(&source_path).unwrap();
        let mut jar = ZipWriter::new(file);
        jar.start_file(
            "data/test/patchouli_books/guide/book.json",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
        jar.write_all(br#"{"i18n":true,"landing_text":"book.landing"}"#)
            .unwrap();
        jar.finish().unwrap();

        let projects_root = temporary.0.join("projects");
        let summary =
            create_project(&projects_root, std::slice::from_ref(&source_path), None).unwrap();
        let opened = open_project(&projects_root, Uuid::parse_str(&summary.id).unwrap()).unwrap();
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
                    // A v1.0.0 analysis result had no Patchouli classification flag.
                    entries: vec![yc_language::TranslationEntry {
                        namespace: "test".into(),
                        key: "book.landing".into(),
                        source_text: Some("Welcome $(bold)world$(/bold)".into()),
                        source_locale: Some("en_us".into()),
                        target_text: String::new(),
                        target_original_text: None,
                        translated_source_text: None,
                        origin: TranslationOrigin::Missing,
                        flags: Vec::new(),
                        sort_index: 0,
                    }],
                },
            )
            .unwrap();
        let entry = opened.repository().list_entries(None).unwrap().remove(0);
        assert!(!entry.flags.contains(&EntryFlag::PatchouliText));
        let project_id = Uuid::parse_str(&summary.id).unwrap();
        drop(opened);

        let opened = open_project(&projects_root, project_id).unwrap();
        let entry = opened.repository().list_entries(None).unwrap().remove(0);
        assert!(entry.flags.contains(&EntryFlag::PatchouliText));
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

    #[test]
    fn ai_job_preparation_uses_the_project_target_locale() {
        let (_temporary, opened, entry) = project_with_entry_target("de_de");
        let project_id = opened.summary().id.clone();
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

        assert_eq!(prepared[0].translation.target_locale, "de_de");
    }

    #[test]
    fn ai_target_support_uses_provider_language_limits() {
        assert_eq!(
            target_support_level(ProviderId::DeepL, "de_de", Some(false)),
            AiTargetSupportLevel::Unsupported,
        );
        assert_eq!(
            target_support_level(ProviderId::DeepL, "tlh_aa", Some(true)),
            AiTargetSupportLevel::Unsupported,
        );
        assert_eq!(
            target_support_level(ProviderId::DeepL, "de_de", None),
            AiTargetSupportLevel::Supported,
        );
        assert_eq!(
            target_support_level(ProviderId::OpenAi, "tlh_aa", None),
            AiTargetSupportLevel::Limited,
        );
        assert_eq!(
            target_support_level(ProviderId::Anthropic, "de_de", None),
            AiTargetSupportLevel::Supported,
        );
    }

    #[test]
    fn plamo_target_support_follows_the_catalog_language_table() {
        // PLaMo の対応表はカタログ由来。冗談言語や方言も「限定」ではなく「非対応」にする。
        assert_eq!(
            target_support_level(ProviderId::Plamo, "ja_jp", None),
            AiTargetSupportLevel::Supported,
        );
        assert_eq!(
            target_support_level(ProviderId::Plamo, "zh_cn", None),
            AiTargetSupportLevel::Supported,
        );
        assert_eq!(
            target_support_level(ProviderId::Plamo, "pt_br", None),
            AiTargetSupportLevel::Unsupported,
        );
        assert_eq!(
            target_support_level(ProviderId::Plamo, "en_ud", None),
            AiTargetSupportLevel::Unsupported,
        );
        // DeepL の応答は PLaMo の判定に影響しない。
        assert_eq!(
            target_support_level(ProviderId::Plamo, "de_de", Some(false)),
            AiTargetSupportLevel::Supported,
        );
    }

    #[test]
    fn unsupported_target_errors_name_the_selected_provider() {
        let deepl = ai_error_for_provider(AiError::UnsupportedTargetLanguage, ProviderId::DeepL);
        assert_eq!(
            deepl.user_message,
            yc_locale::t!("ai.deeplTargetUnsupported")
        );

        let plamo = ai_error_for_provider(AiError::UnsupportedTargetLanguage, ProviderId::Plamo);
        assert_eq!(plamo.code, "ai_target_unsupported");
        assert!(plamo.user_message.contains("PLaMo"));
        assert!(!plamo.user_message.contains("DeepL"));

        // 未対応言語以外の変換は従来どおり。
        let network = ai_error_for_provider(AiError::Network, ProviderId::Plamo);
        assert_eq!(network.user_message, yc_locale::t!("ai.providerFailed"));
    }

    #[test]
    fn plamo_settings_accept_only_catalog_models_and_known_devices() {
        let valid = SaveAiProviderInput {
            provider: "plamo".into(),
            model: yc_plamo::DEFAULT_MODEL_ID.into(),
            base_url: String::new(),
            batch_size: 50,
            concurrency: 2,
            custom_prompt: String::new(),
            device: Some("cpu".into()),
        };
        assert!(validate_ai_config(&valid).is_ok());
        // device 未指定は既定へ倒すので通す。
        assert!(
            validate_ai_config(&SaveAiProviderInput {
                device: None,
                ..valid.clone()
            })
            .is_ok()
        );

        let unknown_model = SaveAiProviderInput {
            model: "plamo-3-translate-31b".into(),
            ..valid.clone()
        };
        assert_eq!(
            validate_ai_config(&unknown_model).unwrap_err().code,
            "invalid_ai_settings"
        );

        let unknown_device = SaveAiProviderInput {
            device: Some("tpu".into()),
            ..valid.clone()
        };
        assert_eq!(
            validate_ai_config(&unknown_device).unwrap_err().code,
            "invalid_ai_settings"
        );

        // 3 値すべてと、旧値 `gpu` を受け付ける。
        for device in ["cuda", "vulkan", "cpu", "gpu"] {
            assert!(
                validate_ai_config(&SaveAiProviderInput {
                    device: Some(device.into()),
                    ..valid.clone()
                })
                .is_ok(),
                "{device} must be accepted"
            );
        }

        // 並列度の範囲は既存プロバイダーと同じ規則。
        let bad_concurrency = SaveAiProviderInput {
            concurrency: 5,
            ..valid
        };
        assert_eq!(
            validate_ai_config(&bad_concurrency).unwrap_err().code,
            "invalid_ai_settings"
        );
    }

    #[test]
    fn plamo_settings_report_the_device_and_catalog_model() {
        let mut settings = yc_project::AppSettings::new();
        settings.values.insert(
            "ai.plamo.device".into(),
            serde_json::Value::String("cpu".into()),
        );
        // 保存済みのデバイスがあるので、既定値（実機の検出）は評価されない。
        let config = ai_settings_from(&settings, ProviderId::Plamo, false, None);
        assert_eq!(config.provider, "plamo");
        assert_eq!(config.model, yc_plamo::DEFAULT_MODEL_ID);
        assert_eq!(config.device.as_deref(), Some("cpu"));
        assert_eq!(config.base_url, "");
        assert_eq!(config.custom_prompt, "");

        // デバイス未保存なら既定値を使う。ここでは注入して実機の検出を避ける。
        let unsaved = yc_project::AppSettings::new();
        let config =
            ai_settings_with_device_default(&unsaved, ProviderId::Plamo, false, None, || "vulkan");
        assert_eq!(config.device.as_deref(), Some("vulkan"));

        // 他のプロバイダーにはデバイスを出さない（画面側の契約を増やさない）。
        let openai = ai_settings_from(&settings, ProviderId::OpenAi, false, None);
        assert!(openai.device.is_none());
    }

    #[test]
    fn plamo_never_reports_configured_through_the_credential_path() {
        // 資格情報の有無だけで PLaMo を「設定済み」にしてはいけない。実行可否は
        // モデル・ランタイム・ライセンスで決まるため、この経路は常に false を返す。
        let mut settings = yc_project::AppSettings::new();
        settings.values.insert(
            "ai.plamo.model".into(),
            serde_json::Value::String(yc_plamo::DEFAULT_MODEL_ID.into()),
        );
        assert!(!provider_is_configured(&settings, ProviderId::Plamo, true));
    }

    #[test]
    fn plamo_is_the_only_provider_that_skips_the_credential_store() {
        for provider in [
            ProviderId::OpenAi,
            ProviderId::OpenAiCompatible,
            ProviderId::DeepL,
            ProviderId::ChatGpt,
            ProviderId::Gemini,
            ProviderId::OpenRouter,
            ProviderId::DeepSeek,
            ProviderId::Groq,
            ProviderId::Ollama,
            ProviderId::LmStudio,
            ProviderId::Anthropic,
        ] {
            assert!(provider.uses_credential_store(), "{provider:?}");
        }
        assert!(!ProviderId::Plamo.uses_credential_store());
        assert_eq!(parse_provider("plamo").unwrap(), ProviderId::Plamo);
        assert_eq!(ProviderId::Plamo.as_str(), "plamo");
    }

    #[test]
    fn plamo_outer_batches_hold_exactly_one_entry() {
        // PLaMo は 1 リクエスト 1 項目しか扱えない。外側のバッチを 1 項目に切らないと、
        // Mod 名・用語集・進捗がバッチ先頭の項目のものに固定される。
        let entries: Vec<_> = (0..5)
            .map(|index| AiJobEntry {
                translation: yc_ai::TranslationEntry::new(
                    index,
                    format!("key.{index}"),
                    "Apple %s",
                    "en_us",
                    "ja_jp",
                ),
                glossary_terms: Vec::new(),
                namespace: "test".into(),
                mod_name: format!("Mod {index}"),
                before_target: String::new(),
                before_origin: TranslationOrigin::Missing,
                shader_format: false,
            })
            .collect();

        let batches = split_job_batches(entries, 1);
        assert_eq!(batches.len(), 5);
        for (index, batch) in batches.iter().enumerate() {
            assert_eq!(batch.len(), 1);
            assert_eq!(batch[0].mod_name, format!("Mod {index}"));
        }
    }

    #[test]
    fn other_providers_keep_the_configured_batch_size() {
        let entries: Vec<_> = (0..5)
            .map(|index| AiJobEntry {
                translation: yc_ai::TranslationEntry::new(
                    index,
                    format!("key.{index}"),
                    "Apple %s",
                    "en_us",
                    "ja_jp",
                ),
                glossary_terms: Vec::new(),
                namespace: "test".into(),
                mod_name: "Test".into(),
                before_target: String::new(),
                before_origin: TranslationOrigin::Missing,
                shader_format: false,
            })
            .collect();

        assert_eq!(split_job_batches(entries, 3).len(), 2);
    }

    #[test]
    fn legacy_patchouli_flag_selects_patchouli_profile_for_ai_validation() {
        let (_temporary, opened, entry) = legacy_patchouli_project_with_entry();
        let project_id = opened.summary().id.clone();
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
        let translation = &prepared[0].translation;
        assert_eq!(translation.format_profile, PlaceholderProfile::Patchouli);
        assert_eq!(translation.placeholders.len(), 2);

        let validation = validate_ai_batch_with_profiles(
            &[ProfiledRequestedEntry {
                id: translation.id,
                source: translation.source.clone(),
                format_profile: translation.format_profile,
            }],
            &[AiTranslation {
                id: translation.id,
                translation: "ようこそ 世界".into(),
            }],
        );
        assert!(validation.accepted.is_empty());
        assert!(matches!(
            validation.rejected[0].reasons.as_slice(),
            [ValidationIssue::PlaceholderMismatch { .. }]
        ));
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

    #[tokio::test]
    async fn api_keys_are_stored_independently_for_each_provider() {
        let manager = CredentialManager::new(MemoryCredentialStore::default()).unwrap();
        manager
            .save_api_key(ProviderId::Gemini, "gemini-secret")
            .await
            .unwrap();
        manager
            .save_api_key(ProviderId::OpenRouter, "openrouter-secret")
            .await
            .unwrap();

        assert_eq!(
            manager
                .load_api_key(ProviderId::Gemini)
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            "gemini-secret"
        );
        assert_eq!(
            manager
                .load_api_key(ProviderId::OpenRouter)
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            "openrouter-secret"
        );
        assert!(
            manager
                .load_api_key(ProviderId::DeepSeek)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn previously_saved_custom_provider_settings_load_without_migration() {
        let mut settings = yc_project::AppSettings::new();
        settings.values.insert(
            "ai.provider".into(),
            serde_json::Value::String("openai-compatible".into()),
        );
        settings.values.insert(
            "ai.openai-compatible.model".into(),
            serde_json::Value::String("legacy-model".into()),
        );
        settings.values.insert(
            "ai.openai-compatible.baseUrl".into(),
            serde_json::Value::String("https://legacy.example/v1".into()),
        );

        let provider = provider_from_settings(&settings).unwrap();
        let config = ai_settings_from(&settings, provider, true, None);

        assert_eq!(provider, ProviderId::OpenAiCompatible);
        assert_eq!(config.provider, "openai-compatible");
        assert_eq!(config.model, "legacy-model");
        assert_eq!(config.base_url, "https://legacy.example/v1");
    }

    #[test]
    fn fixed_provider_presets_use_their_expected_api_urls() {
        let cases = [
            (ProviderId::OpenAi, "https://api.openai.com/v1"),
            (ProviderId::OpenAiCompatible, "https://custom.example/v1"),
            (
                ProviderId::Gemini,
                "https://generativelanguage.googleapis.com/v1beta/openai/",
            ),
            (ProviderId::OpenRouter, "https://openrouter.ai/api/v1"),
            (ProviderId::DeepSeek, "https://api.deepseek.com"),
            (ProviderId::Groq, "https://api.groq.com/openai/v1"),
            (ProviderId::Ollama, "http://localhost:11434/v1"),
            (ProviderId::LmStudio, "http://localhost:1234/v1"),
            (ProviderId::Anthropic, "https://api.anthropic.com"),
        ];

        for (provider, expected_url) in cases {
            assert_eq!(
                provider_base_url(provider, "https://custom.example/v1"),
                expected_url
            );
        }
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
            shader_format: false,
        }];
        let outcome = TranslationOutcome {
            accepted: vec![ValidatedTranslation {
                id: entry.id,
                translation: "りんご %s".into(),
            }],
            rejected: Vec::new(),
        };
        let (updated, rejected) =
            apply_outcome_to_project(&opened, &batch, &outcome, &CancellationToken::new(), None)
                .unwrap();
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
            shader_format: false,
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
        assert!(apply_outcome_to_project(&opened, &batch, &outcome, &cancellation, None).is_err());
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
            shader_format: false,
        }];
        let outcome = TranslationOutcome {
            accepted: validation.accepted,
            rejected: validation.rejected,
        };
        let (updated, rejected) =
            apply_outcome_to_project(&opened, &batch, &outcome, &CancellationToken::new(), None)
                .unwrap();
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
            shader_format: false,
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
            apply_outcome_to_project(&opened, &batch, &outcome, &CancellationToken::new(), None)
                .unwrap();

        assert!(updated.is_empty());
        assert!(rejected.is_empty());
        let stored = opened.repository().list_entries(None).unwrap().remove(0);
        assert_eq!(stored.target_text.as_deref(), Some("手動の訳"));
        assert_eq!(stored.origin, TranslationOrigin::Edited);
    }
}
