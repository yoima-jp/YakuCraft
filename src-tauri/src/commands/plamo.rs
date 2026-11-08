//! PLaMo の状態表示・ライセンス同意・ダウンロード・削除のコマンド。
//!
//! ダウンロードはランタイム（`llama-server`）とモデル（GGUF）の 2 段階で、どちらも
//! `yc_llama` の検証付きダウンローダを通す。ここでは進捗の間引きとイベント送信だけを行う。

use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, State, ipc::Channel};
use tauri_plugin_opener::OpenerExt;
use tokio_util::sync::CancellationToken;
use yc_llama::{LlamaError, current_target_build, download_verified, ensure_runtime};
use yc_plamo::{
    LICENSE_URL_EN, LICENSE_URL_JA, PlamoModel, find_model, models, supported_target_locales,
};
use yc_project::{AppSettings, load_settings};

use crate::{
    error::AppError,
    plamo::{
        self, PlamoDevice, download_error, download_running_error, invalid_settings_error,
        license_not_accepted_error, model_downloaded, model_path, part_path, runtime_ready,
        runtime_size_bytes,
    },
    state::AppState,
};

/// 進捗イベントの最小間隔。UI の描画頻度を抑えつつ、固まって見えない程度に出す。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlamoModelDto {
    pub id: String,
    pub parameter_label: String,
    pub size_bytes: u64,
    pub downloaded: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlamoRuntimeReadyDto {
    pub cpu: bool,
    pub gpu: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlamoRuntimeSizeDto {
    pub cpu: u64,
    pub gpu: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlamoStatusDto {
    pub models: Vec<PlamoModelDto>,
    pub runtime_ready: PlamoRuntimeReadyDto,
    pub runtime_size_bytes: PlamoRuntimeSizeDto,
    pub license_accepted: bool,
    pub supported_target_locales: Vec<String>,
    pub downloading: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlamoDownloadEvent {
    Progress {
        phase: PlamoDownloadPhase,
        received_bytes: u64,
        total_bytes: u64,
    },
    Completed,
    Cancelled,
    Failed {
        error: AppError,
    },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlamoDownloadPhase {
    Runtime,
    Model,
}

#[tauri::command]
pub fn get_plamo_status(state: State<'_, AppState>) -> Result<PlamoStatusDto, AppError> {
    let settings = load_settings(&state.paths.settings_file).map_err(settings_error)?;
    Ok(plamo_status(&state, &settings))
}

fn plamo_status(state: &AppState, settings: &AppSettings) -> PlamoStatusDto {
    PlamoStatusDto {
        models: models()
            .iter()
            .map(|model| PlamoModelDto {
                id: model.id.to_owned(),
                parameter_label: model.parameters.to_owned(),
                size_bytes: model.size_bytes,
                downloaded: model_downloaded(&state.plamo.models_dir, model),
            })
            .collect(),
        runtime_ready: PlamoRuntimeReadyDto {
            cpu: runtime_ready(&state.plamo.runtime_dir, PlamoDevice::Cpu),
            gpu: runtime_ready(&state.plamo.runtime_dir, PlamoDevice::Gpu),
        },
        runtime_size_bytes: PlamoRuntimeSizeDto {
            cpu: runtime_size_bytes(PlamoDevice::Cpu),
            gpu: runtime_size_bytes(PlamoDevice::Gpu),
        },
        license_accepted: plamo::license_accepted(settings),
        supported_target_locales: supported_target_locales()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        downloading: state.plamo.is_downloading(),
    }
}

#[tauri::command]
pub fn accept_plamo_license(state: State<'_, AppState>) -> Result<(), AppError> {
    super::settings::update_settings_file(
        &state.paths.settings_file,
        |settings| {
            settings
                .values
                .insert(plamo::license_key(), serde_json::Value::Bool(true));
            Ok(())
        },
        settings_error,
        settings_error,
    )?;
    Ok(())
}

#[tauri::command]
pub async fn download_plamo_files(
    model_id: String,
    device: String,
    on_event: Channel<PlamoDownloadEvent>,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let model = find_model(&model_id).ok_or_else(invalid_settings_error)?;
    let device = PlamoDevice::parse(&device).ok_or_else(invalid_settings_error)?;
    let settings = load_settings(&state.paths.settings_file).map_err(settings_error)?;
    if !plamo::license_accepted(&settings) {
        return Err(license_not_accepted_error());
    }

    // ダウンロードとモデル削除は同じファイルを触る。削除が走っている間は始めず、
    // 始めたら削除が割り込めないよう、guard を取得の終わりまで保持する。
    let file_gate = state
        .plamo
        .try_begin_file_operation()
        .ok_or_else(file_busy_error)?;
    let guard = state
        .plamo
        .begin_download()
        .ok_or_else(download_running_error)?;
    let cancellation = guard.cancellation().clone();
    tracing::info!(
        model_id = model.id,
        device = device.as_str(),
        "plamo download started"
    );

    let result = download_files(&state, model, device, &on_event, &cancellation).await;
    let event = match &result {
        Ok(()) => {
            tracing::info!(model_id = model.id, "plamo download finished");
            PlamoDownloadEvent::Completed
        }
        Err(DownloadFailure::Cancelled) => {
            tracing::info!(model_id = model.id, "plamo download cancelled");
            PlamoDownloadEvent::Cancelled
        }
        Err(DownloadFailure::Error(error)) => {
            tracing::warn!(
                model_id = model.id,
                error_code = error.code,
                "plamo download failed"
            );
            PlamoDownloadEvent::Failed {
                error: error.clone(),
            }
        }
    };
    // 端末イベントはチャネルが閉じていても結果を変えない。閉じた後は誰も見ていない。
    let _ = on_event.send(event);
    // スロットの解放は guard の drop に任せる。ここで明示的に落とすと、失敗時も含めて
    // 次のダウンロードがすぐ始められる。
    drop(guard);
    drop(file_gate);

    match result {
        // キャンセルは利用者の操作なので、コマンドをエラーにせず画面に失敗を出さない。
        Ok(()) | Err(DownloadFailure::Cancelled) => Ok(()),
        Err(DownloadFailure::Error(error)) => Err(error),
    }
}

enum DownloadFailure {
    Cancelled,
    Error(AppError),
}

async fn download_files(
    state: &AppState,
    model: &'static PlamoModel,
    device: PlamoDevice,
    channel: &Channel<PlamoDownloadEvent>,
    cancellation: &CancellationToken,
) -> Result<(), DownloadFailure> {
    let build = current_target_build(device.backend()).ok_or_else(|| {
        DownloadFailure::Error(AppError::simple(
            "plamo_unsupported_platform",
            &yc_locale::t!("ai.taskFailed"),
        ))
    })?;

    ensure_runtime(
        build,
        &state.plamo.runtime_dir,
        progress_reporter(channel, PlamoDownloadPhase::Runtime),
        cancellation.clone(),
    )
    .await
    .map_err(download_failure)?;

    download_verified(
        &model.download_url(),
        &model_path(&state.plamo.models_dir, model),
        model.size_bytes,
        model.sha256,
        progress_reporter(channel, PlamoDownloadPhase::Model),
        cancellation.clone(),
    )
    .await
    .map_err(download_failure)?;
    Ok(())
}

fn download_failure(error: LlamaError) -> DownloadFailure {
    match error {
        LlamaError::Cancelled => DownloadFailure::Cancelled,
        error => DownloadFailure::Error(download_error(error)),
    }
}

/// 進捗を間引いて送るコールバックを作る。
///
/// ダウンローダは 250ms または 1MiB ごとに呼ぶが、モデルは GB 単位なのでそのまま送ると
/// IPC の往復が無駄になる。完了時だけは必ず送り、バーが 100% で止まらないようにする。
fn progress_reporter(
    channel: &Channel<PlamoDownloadEvent>,
    phase: PlamoDownloadPhase,
) -> impl Fn(u64, u64) + Send + Sync + 'static {
    let channel = channel.clone();
    let last_emit = Mutex::new(Instant::now());
    move |received, total| {
        let complete = received >= total;
        let elapsed = last_emit
            .lock()
            .map(|last| last.elapsed() >= PROGRESS_INTERVAL)
            .unwrap_or(true);
        if !complete && !elapsed {
            return;
        }
        if let Ok(mut last) = last_emit.lock() {
            *last = Instant::now();
        }
        let _ = channel.send(PlamoDownloadEvent::Progress {
            phase,
            received_bytes: received,
            total_bytes: total,
        });
    }
}

#[tauri::command]
pub fn cancel_plamo_download(state: State<'_, AppState>) -> bool {
    state.plamo.cancel_download()
}

#[tauri::command]
pub async fn delete_plamo_model(
    model_id: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let model = find_model(&model_id).ok_or_else(invalid_settings_error)?;
    // ダウンロードと排他する。検査してから消すまでの間に取得が始まると、書き込み中の
    // ファイルや `.part` を消してしまうため、guard はファイル削除まで保持する。
    let _file_gate = state
        .plamo
        .try_begin_file_operation()
        .ok_or_else(download_running_error)?;
    // サーバ起動とも排他する。`start_server` は起動が終わるまで使用中モデルを記録しないため、
    // ここで待たずに判定すると、読み込み中のファイルを「未使用」と誤認して消してしまう。
    // この gate は削除が終わる（管理者の差し替えまで済む）ところまで保持する。
    let _server_gate = state.plamo.begin_deletion().await;
    let path = model_path(&state.plamo.models_dir, model);
    // 起動中のサーバがこのモデルを開いている間は消せない（Windows は開いているファイルの
    // 削除を拒否する）。先に止めてから消す。`RuntimeManager::shutdown` はアイドル監視を
    // 恒久的に止めるため、管理者ごと作り直して次の翻訳でもアイドル停止が効くようにする。
    if state.plamo.uses_model(&path) {
        state.plamo.shutdown_and_reset().await;
        state.plamo.clear_active_model();
        tracing::info!(
            model_id = model.id,
            "plamo server stopped to delete its model"
        );
    }

    remove_file_if_exists(&path).await?;
    // 中断したダウンロードの `.part` も残さない。残すと次の取得が古い断片から再開する。
    remove_file_if_exists(&part_path(&path)).await?;
    tracing::info!(model_id = model.id, "plamo model deleted");
    Ok(())
}

async fn remove_file_if_exists(path: &Path) -> Result<(), AppError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::new(
            "plamo_delete_failed",
            &yc_locale::t!("ai.plamoDownloadWriteFailed"),
            Some(error.to_string()),
        )),
    }
}

#[tauri::command]
pub fn open_plamo_license(app: AppHandle, language: String) -> Result<(), AppError> {
    // 呼び出し元の URL は受け取らず、固定のライセンス URL だけを開く。
    let url = match language.as_str() {
        "ja" => LICENSE_URL_JA,
        "en" => LICENSE_URL_EN,
        _ => return Err(invalid_settings_error()),
    };
    app.opener().open_url(url, None::<&str>).map_err(|_| {
        AppError::simple(
            "plamo_license_open_failed",
            &yc_locale::t!("ai.browserOpenFailed"),
        )
    })
}

fn settings_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "settings_unavailable",
        &yc_locale::t!("ai.settingsReadWrite"),
        Some(error.to_string()),
    )
}

/// 同じモデルファイルを触る別の操作（ダウンロードまたは削除）が進行中。
fn file_busy_error() -> AppError {
    AppError::simple("plamo_file_busy", &yc_locale::t!("ai.plamoDownloadRunning"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PlamoDownloadEvent, PlamoDownloadPhase, PlamoStatusDto};

    #[test]
    fn download_events_serialize_to_the_frontend_contract() {
        // フロントエンドの `PlamoDownloadEvent` は kind で分岐し、進捗は camelCase。
        let progress = serde_json::to_value(PlamoDownloadEvent::Progress {
            phase: PlamoDownloadPhase::Runtime,
            received_bytes: 1,
            total_bytes: 2,
        })
        .unwrap();
        assert_eq!(
            progress,
            json!({"kind": "progress", "phase": "runtime", "receivedBytes": 1, "totalBytes": 2})
        );

        let model = serde_json::to_value(PlamoDownloadEvent::Progress {
            phase: PlamoDownloadPhase::Model,
            received_bytes: 3,
            total_bytes: 4,
        })
        .unwrap();
        assert_eq!(model["phase"], json!("model"));

        assert_eq!(
            serde_json::to_value(PlamoDownloadEvent::Completed).unwrap(),
            json!({"kind": "completed"})
        );
        assert_eq!(
            serde_json::to_value(PlamoDownloadEvent::Cancelled).unwrap(),
            json!({"kind": "cancelled"})
        );
        let failed = serde_json::to_value(PlamoDownloadEvent::Failed {
            error: crate::error::AppError::simple("plamo_download_failed", "failed"),
        })
        .unwrap();
        assert_eq!(failed["kind"], json!("failed"));
        assert_eq!(failed["error"]["code"], json!("plamo_download_failed"));
    }

    #[test]
    fn status_serializes_with_camel_case_fields() {
        let status = PlamoStatusDto {
            models: Vec::new(),
            runtime_ready: super::PlamoRuntimeReadyDto {
                cpu: true,
                gpu: false,
            },
            runtime_size_bytes: super::PlamoRuntimeSizeDto { cpu: 1, gpu: 2 },
            license_accepted: true,
            supported_target_locales: vec!["ja_jp".into()],
            downloading: false,
        };
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["runtimeReady"], json!({"cpu": true, "gpu": false}));
        assert_eq!(value["runtimeSizeBytes"], json!({"cpu": 1, "gpu": 2}));
        assert_eq!(value["licenseAccepted"], json!(true));
        assert_eq!(value["supportedTargetLocales"], json!(["ja_jp"]));
        assert_eq!(value["downloading"], json!(false));
    }
}
