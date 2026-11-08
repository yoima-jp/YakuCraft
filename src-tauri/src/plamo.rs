//! PLaMo 3 Translate をローカル実行するための状態・設定・エラー変換。
//!
//! モデルとランタイムはアプリへ同梱せず、必要になった時点で取得する。取得元・サイズ・
//! SHA-256 は `yc_plamo` と `yc_llama` の固定表だけを信頼し、ここでは保存場所、設定キー、
//! 実行可否の判定だけを扱う。

use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde_json::Value;
use tokio_util::sync::CancellationToken;
use yc_llama::{
    Backend, LlamaError, RuntimeManager, ServerConfig, current_target_build, installed_runtime,
};
use yc_plamo::{DEFAULT_MODEL_ID, PlamoModel, find_model};
use yc_project::AppSettings;

use crate::error::AppError;

/// 設定キーの接頭辞。既存の `ai.<provider>.<field>` 規則に合わせる。
const SETTING_PREFIX: &str = "ai.plamo";

/// 既定のデバイス。Vulkan が使える環境では GPU の方が速い。
const DEFAULT_DEVICE: PlamoDevice = PlamoDevice::Gpu;

/// 既定デバイスの保存値。設定画面が未保存時に使う。
pub fn default_device_str() -> &'static str {
    DEFAULT_DEVICE.as_str()
}

/// 推論に使うデバイス。`Gpu` は Vulkan ビルドを指す。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlamoDevice {
    Gpu,
    Cpu,
}

impl PlamoDevice {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "gpu" => Some(Self::Gpu),
            "cpu" => Some(Self::Cpu),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }

    pub fn backend(self) -> Backend {
        match self {
            Self::Gpu => Backend::Gpu,
            Self::Cpu => Backend::Cpu,
        }
    }
}

/// サーバ起動までに揃っているべき情報。
#[derive(Debug)]
pub struct PlamoRunConfig {
    pub model_id: String,
    pub device: PlamoDevice,
    pub model_path: PathBuf,
    pub server_path: PathBuf,
}

/// 進行中のダウンロード。同時に 1 つだけ持つ。
struct ActiveDownload {
    id: u64,
    cancellation: CancellationToken,
}

/// サーバの停止を info で記録するラッパー。
///
/// `yc-llama` はアイドル停止を debug でしか記録せず、`RunEvent::Exit` からの停止は
/// 非同期を待てないためログを挟む場所が無い。サーバの起動・停止はサポート時に必ず見る
/// 出来事なので、停止の全経路（アイドル・明示・終了）をここで 1 か所に集めて info に出す。
/// `yc-llama` 側の公開 API は変えずに済ませるためのラッパーでもある。
struct LoggingLauncher {
    inner: yc_llama::LlamaServerLauncher,
}

impl yc_llama::ServerLauncher for LoggingLauncher {
    fn spawn<'a>(
        &'a self,
        config: &'a ServerConfig,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = yc_llama::Result<Box<dyn yc_llama::ServerHandle>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let handle = self.inner.spawn(config).await?;
            Ok(Box::new(LoggingHandle { inner: handle }) as Box<dyn yc_llama::ServerHandle>)
        })
    }
}

/// 停止を info で記録するハンドル。実際に止めた経路（非同期・同期）で 1 行ずつ残す。
struct LoggingHandle {
    inner: Box<dyn yc_llama::ServerHandle>,
}

impl yc_llama::ServerHandle for LoggingHandle {
    fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    fn api_key(&self) -> &str {
        self.inner.api_key()
    }

    fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.inner.shutdown().await;
            tracing::info!("plamo server stopped");
        })
    }

    fn shutdown_blocking(&self) {
        self.inner.shutdown_blocking();
        tracing::info!("plamo server stopped");
    }
}

/// 停止を info で記録する管理者を作る。
fn new_runtime_manager() -> RuntimeManager {
    RuntimeManager::with_launcher(
        std::sync::Arc::new(LoggingLauncher {
            inner: yc_llama::LlamaServerLauncher,
        }),
        yc_llama::DEFAULT_IDLE_TIMEOUT,
    )
}

/// PLaMo のローカル実行状態。`AppState` から共有する。
pub struct PlamoState {
    /// 起動中サーバの管理者。`RuntimeManager::shutdown` はアイドル監視を恒久的に止めるため、
    /// 明示的に停止した後は新しいものへ差し替える（[`PlamoState::shutdown_and_reset`]）。
    runtime: Mutex<RuntimeManager>,
    pub models_dir: PathBuf,
    pub runtime_dir: PathBuf,
    /// ダウンロードとモデル削除を排他する。どちらも同じモデルファイルと `.part` を触るため、
    /// 検査と実際のファイル操作の間で割り込まれると、書き込み中のファイルを消してしまう。
    file_gate: tokio::sync::Mutex<()>,
    /// サーバ起動とモデル削除を排他する。`ensure_server` が返る前は「使用中のモデル」がまだ
    /// 記録されていないため、この gate が無いと削除が読み込み中のファイルを消してしまう。
    /// ダウンロードとは別の gate にしてある（数 GB の取得中に翻訳まで止めないため）。
    server_gate: tokio::sync::Mutex<()>,
    download: Mutex<Option<ActiveDownload>>,
    /// 直近に起動したサーバが開いているモデル。削除時にサーバを止めるべきか判断する。
    active_model: Mutex<Option<PathBuf>>,
    next_download_id: AtomicU64,
}

impl PlamoState {
    pub fn new(data_dir: &Path) -> Self {
        let root = data_dir.join("plamo");
        Self {
            runtime: Mutex::new(new_runtime_manager()),
            models_dir: root.join("models"),
            runtime_dir: root.join("runtime"),
            file_gate: tokio::sync::Mutex::new(()),
            server_gate: tokio::sync::Mutex::new(()),
            download: Mutex::new(None),
            active_model: Mutex::new(None),
            next_download_id: AtomicU64::new(0),
        }
    }

    /// 現在のサーバ管理者。`RuntimeManager` は内部の `Arc` を共有するだけなので clone は軽い。
    pub fn runtime(&self) -> RuntimeManager {
        self.runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// サーバを止め、アイドル監視も含めて管理者を作り直す。
    ///
    /// `RuntimeManager::shutdown` はアイドル監視タスクのトークンを恒久的にキャンセルするため、
    /// 同じ管理者を使い続けると次の起動後にアイドル停止が二度と働かず、モデルを載せたままの
    /// サーバがアプリ終了まで残る。先に新しい管理者へ差し替えてから古いものを止めるので、
    /// 入れ替えの隙間に起動要求が来ても止める側へは行かない。
    pub async fn shutdown_and_reset(&self) {
        let previous = {
            let mut slot = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            std::mem::replace(&mut *slot, new_runtime_manager())
        };
        previous.shutdown().await;
    }

    /// ダウンロードとモデル削除の排他を取る。既に片方が動いていれば `None`。
    ///
    /// 返した guard を保持している間は、もう一方が同じファイルへ触ることはない。
    pub fn try_begin_file_operation(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        self.file_gate.try_lock().ok()
    }

    /// サーバ起動（とその後のモデル記録）とモデル削除を排他する。
    ///
    /// `ensure_server` が返るまで `active_model` は空なので、削除側は「誰も使っていない」と
    /// 誤認して読み込み中のファイルを消し得る。起動側はこの gate の中で起動と
    /// [`PlamoState::set_active_model`] を済ませ、削除側は同じ gate を取ってから判定する。
    /// 起動は数秒〜数十秒かかるが、削除は待たせて構わない（待たせないとファイルが壊れる）。
    pub async fn start_server(
        &self,
        config: &ServerConfig,
        model_path: PathBuf,
    ) -> Result<yc_llama::ServerLease, LlamaError> {
        let _server_gate = self.server_gate.lock().await;
        // gate を取った後に管理者を読む。削除側の `shutdown_and_reset` は同じ gate の中で
        // 差し替えるため、ここで得た管理者は差し替え後か差し替え前のどちらか一方になる。
        let lease = self.runtime().ensure_server(config).await?;
        self.set_active_model(model_path);
        Ok(lease)
    }

    /// モデル削除の前に、起動処理が終わるまで待つ。
    pub async fn begin_deletion(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.server_gate.lock().await
    }

    /// ダウンロードを 1 つだけ開始する。既に走っていれば `None`。
    ///
    /// 返した guard を落とすまでスロットは埋まったままなので、キャンセルや失敗の後始末を
    /// 呼び忘れても次のダウンロードが永久に始められなくなることはない。
    pub fn begin_download(&self) -> Option<PlamoDownloadGuard<'_>> {
        let mut download = self
            .download
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if download.is_some() {
            return None;
        }
        let id = self.next_download_id.fetch_add(1, Ordering::Relaxed);
        let cancellation = CancellationToken::new();
        *download = Some(ActiveDownload {
            id,
            cancellation: cancellation.clone(),
        });
        drop(download);
        Some(PlamoDownloadGuard {
            state: self,
            id,
            cancellation,
        })
    }

    pub fn is_downloading(&self) -> bool {
        self.download
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_some()
    }

    /// 進行中のダウンロードを取り消す。走っていなければ `false`。
    pub fn cancel_download(&self) -> bool {
        let download = self
            .download
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match download.as_ref() {
            Some(active) => {
                active.cancellation.cancel();
                true
            }
            None => false,
        }
    }

    /// 起動中のサーバが開いているモデルを記録する。
    pub fn set_active_model(&self, path: PathBuf) {
        if let Ok(mut active) = self.active_model.lock() {
            *active = Some(path);
        }
    }

    pub fn clear_active_model(&self) {
        if let Ok(mut active) = self.active_model.lock() {
            *active = None;
        }
    }

    /// 起動中のサーバがこのモデルを開いているか。
    pub fn uses_model(&self, path: &Path) -> bool {
        self.active_model
            .lock()
            .map(|active| active.as_deref() == Some(path))
            .unwrap_or(false)
    }
}

/// ダウンロードのスロットを保持する guard。drop でスロットを解放する。
pub struct PlamoDownloadGuard<'a> {
    state: &'a PlamoState,
    id: u64,
    cancellation: CancellationToken,
}

impl PlamoDownloadGuard<'_> {
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl Drop for PlamoDownloadGuard<'_> {
    fn drop(&mut self) {
        let mut download = self
            .state
            .download
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if download.as_ref().is_some_and(|active| active.id == self.id) {
            *download = None;
        }
    }
}

fn setting_key(field: &str) -> String {
    format!("{SETTING_PREFIX}.{field}")
}

pub fn model_key() -> String {
    setting_key("model")
}

pub fn device_key() -> String {
    setting_key("device")
}

pub fn concurrency_key() -> String {
    setting_key("concurrency")
}

pub fn license_key() -> String {
    setting_key("licenseAccepted")
}

/// 保存済みのモデル ID。未設定なら既定モデル。
pub fn saved_model_id(settings: &AppSettings) -> String {
    settings
        .values
        .get(&model_key())
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_MODEL_ID)
        .to_owned()
}

/// 保存済みのモデル。カタログに無い ID（旧バージョンの値や破損）は `None`。
pub fn saved_model(settings: &AppSettings) -> Option<&'static PlamoModel> {
    find_model(&saved_model_id(settings))
}

pub fn saved_device(settings: &AppSettings) -> PlamoDevice {
    settings
        .values
        .get(&device_key())
        .and_then(Value::as_str)
        .and_then(PlamoDevice::parse)
        .unwrap_or(DEFAULT_DEVICE)
}

pub fn license_accepted(settings: &AppSettings) -> bool {
    settings
        .values
        .get(&license_key())
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn model_path(models_dir: &Path, model: &PlamoModel) -> PathBuf {
    models_dir.join(model.file)
}

/// `<dest>.part` のパス。`yc_llama` のダウンローダと同じ規則で作る。
pub fn part_path(path: &Path) -> PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(".part");
    PathBuf::from(raw)
}

/// モデルファイルが期待サイズで存在するか。内容の照合はダウンロード時に行う。
pub fn model_downloaded(models_dir: &Path, model: &PlamoModel) -> bool {
    match std::fs::metadata(model_path(models_dir, model)) {
        Ok(metadata) => metadata.is_file() && metadata.len() == model.size_bytes,
        Err(_) => false,
    }
}

/// 指定デバイス用の `llama-server` が展開済みか。
pub fn runtime_ready(runtime_dir: &Path, device: PlamoDevice) -> bool {
    server_path(runtime_dir, device).is_some()
}

pub fn server_path(runtime_dir: &Path, device: PlamoDevice) -> Option<PathBuf> {
    let build = current_target_build(device.backend())?;
    installed_runtime(build, runtime_dir)
}

/// ダウンロードが必要なバイト数。未対応プラットフォームでは 0。
pub fn runtime_size_bytes(device: PlamoDevice) -> u64 {
    current_target_build(device.backend()).map_or(0, |build| build.size)
}

/// 設定画面とジョブ開始判定で共有する「実行できる状態か」。
///
/// 資格情報ではなく、カタログのモデル・実ファイル・ランタイム・ライセンス同意の 4 点で決まる。
pub fn configured(plamo: &PlamoState, settings: &AppSettings) -> bool {
    let Some(model) = saved_model(settings) else {
        return false;
    };
    configured_for(plamo, settings, model)
}

fn configured_for(plamo: &PlamoState, settings: &AppSettings, model: &PlamoModel) -> bool {
    license_accepted(settings)
        && model_downloaded(&plamo.models_dir, model)
        && runtime_ready(&plamo.runtime_dir, saved_device(settings))
}

/// ジョブ開始に必要な情報を集める。足りないものがあれば、利用者が次に何をすべきかを返す。
pub fn run_config(plamo: &PlamoState, settings: &AppSettings) -> Result<PlamoRunConfig, AppError> {
    if !license_accepted(settings) {
        return Err(license_not_accepted_error());
    }
    let model = saved_model(settings).ok_or_else(not_ready_error)?;
    run_config_for(plamo, model, saved_device(settings))
}

fn run_config_for(
    plamo: &PlamoState,
    model: &PlamoModel,
    device: PlamoDevice,
) -> Result<PlamoRunConfig, AppError> {
    if !model_downloaded(&plamo.models_dir, model) {
        return Err(not_ready_error());
    }
    let server_path = server_path(&plamo.runtime_dir, device).ok_or_else(not_ready_error)?;
    Ok(PlamoRunConfig {
        model_id: model.id.to_owned(),
        device,
        model_path: model_path(&plamo.models_dir, model),
        server_path,
    })
}

/// ジョブの並列度に合わせたサーバ設定。コンテキスト長は既定値のまま使う。
pub fn server_config(config: &PlamoRunConfig, slots: usize) -> Result<ServerConfig, AppError> {
    let slots = u8::try_from(slots.clamp(1, 4)).unwrap_or(1);
    ServerConfig::new(
        config.server_path.clone(),
        config.model_path.clone(),
        config.device.backend(),
    )
    .with_slots(slots)
    .map_err(|error| {
        AppError::new(
            "plamo_runtime_start_failed",
            &yc_locale::t!("ai.providerFailed"),
            Some(error.to_string()),
        )
    })
}

pub fn invalid_settings_error() -> AppError {
    AppError::simple("invalid_ai_settings", &yc_locale::t!("ai.settingsCheck"))
}

pub fn license_not_accepted_error() -> AppError {
    AppError::simple(
        "plamo_license_not_accepted",
        &yc_locale::t!("ai.plamoLicenseNotAccepted"),
    )
}

pub fn not_ready_error() -> AppError {
    AppError::simple("plamo_not_ready", &yc_locale::t!("ai.plamoNotReady"))
}

pub fn download_running_error() -> AppError {
    AppError::simple(
        "plamo_download_running",
        &yc_locale::t!("ai.plamoDownloadRunning"),
    )
}

/// ダウンロード失敗を、利用者が次の行動を取れる文言へ変換する。
pub fn download_error(error: LlamaError) -> AppError {
    let message = match &error {
        LlamaError::Network | LlamaError::HttpStatus { .. } => {
            yc_locale::t!("ai.plamoDownloadNetworkFailed")
        }
        LlamaError::ChecksumMismatch
        | LlamaError::SizeMismatch { .. }
        | LlamaError::InvalidArchive { .. }
        | LlamaError::MissingServerBinary { .. } => {
            yc_locale::t!("ai.plamoDownloadChecksumFailed")
        }
        LlamaError::Io { .. } => yc_locale::t!("ai.plamoDownloadWriteFailed"),
        // キャンセルは呼び出し側が別のイベントとして扱う。
        _ => yc_locale::t!("ai.taskFailed"),
    };
    AppError::new("plamo_download_failed", &message, Some(error.to_string()))
}

/// サーバ起動失敗。GPU はドライバ由来で失敗しやすいため、CPU への切り替えを案内する。
pub fn runtime_start_error(device: PlamoDevice, error: LlamaError) -> AppError {
    let message = match device {
        PlamoDevice::Gpu => yc_locale::t!("ai.plamoRuntimeStartFailed"),
        PlamoDevice::Cpu => yc_locale::t!("ai.providerFailed"),
    };
    AppError::new(
        "plamo_runtime_start_failed",
        &message,
        Some(error.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use yc_plamo::default_model;

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn create() -> Self {
            let path = std::env::temp_dir().join(format!(
                "yakucraft-plamo-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&path).expect("temporary directory should be created");
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// カタログのモデルは GB 単位なので、判定のテストには同じ形の小さいモデルを使う。
    fn tiny_model() -> PlamoModel {
        PlamoModel {
            id: "plamo-test",
            parameters: "0B",
            repo: "example/test",
            commit: "0000000000000000000000000000000000000000",
            file: "plamo-test.gguf",
            size_bytes: 4,
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        }
    }

    fn settings_with(values: &[(&str, Value)]) -> AppSettings {
        let mut settings = AppSettings::new();
        for (key, value) in values {
            settings.values.insert((*key).to_owned(), value.clone());
        }
        settings
    }

    fn write_model(models_dir: &Path, model: &PlamoModel) {
        fs::create_dir_all(models_dir).expect("models directory should be created");
        fs::write(
            model_path(models_dir, model),
            vec![0_u8; model.size_bytes as usize],
        )
        .expect("model fixture should be written");
    }

    fn write_runtime(runtime_dir: &Path, device: PlamoDevice) {
        let build = current_target_build(device.backend()).expect("current platform build");
        let server = runtime_dir
            .join(build.install_dir_name())
            .join(build.server_relative_path());
        fs::create_dir_all(server.parent().expect("server parent"))
            .expect("runtime directory should be created");
        fs::write(server, b"binary").expect("runtime fixture should be written");
    }

    #[test]
    fn device_parsing_rejects_unknown_values() {
        assert_eq!(PlamoDevice::parse("gpu"), Some(PlamoDevice::Gpu));
        assert_eq!(PlamoDevice::parse("cpu"), Some(PlamoDevice::Cpu));
        assert_eq!(PlamoDevice::parse("GPU"), None);
        assert_eq!(PlamoDevice::parse(""), None);
        assert_eq!(PlamoDevice::Gpu.as_str(), "gpu");
        assert_eq!(PlamoDevice::Cpu.backend(), Backend::Cpu);
    }

    #[test]
    fn saved_values_fall_back_to_the_catalog_default() {
        let settings = settings_with(&[]);
        assert_eq!(saved_model_id(&settings), DEFAULT_MODEL_ID);
        assert_eq!(
            saved_model(&settings).map(|model| model.id),
            Some(DEFAULT_MODEL_ID)
        );
        assert_eq!(saved_device(&settings), DEFAULT_DEVICE);
        assert!(!license_accepted(&settings));

        // カタログに無い ID は既定へ読み替えず、未設定として扱う。
        let stale = settings_with(&[(&model_key(), Value::String("plamo-9".into()))]);
        assert!(saved_model(&stale).is_none());

        let cpu = settings_with(&[(&device_key(), Value::String("cpu".into()))]);
        assert_eq!(saved_device(&cpu), PlamoDevice::Cpu);
    }

    #[test]
    fn configured_requires_the_model_file_runtime_and_license() {
        let temporary = TemporaryDirectory::create();
        let plamo = PlamoState::new(&temporary.0);
        let model = tiny_model();
        let accepted = settings_with(&[(&license_key(), Value::Bool(true))]);

        assert!(
            !configured_for(&plamo, &accepted, &model),
            "nothing is installed yet"
        );

        write_model(&plamo.models_dir, &model);
        assert!(
            !configured_for(&plamo, &accepted, &model),
            "runtime is still missing"
        );

        write_runtime(&plamo.runtime_dir, PlamoDevice::Gpu);
        assert!(configured_for(&plamo, &accepted, &model));

        // ライセンス未同意なら、ファイルが揃っていても実行できない。
        assert!(!configured_for(&plamo, &settings_with(&[]), &model));

        // サイズが違うファイルは壊れたダウンロードとして扱う。
        fs::write(model_path(&plamo.models_dir, &model), b"short").expect("truncated fixture");
        assert!(!configured_for(&plamo, &accepted, &model));
    }

    #[test]
    fn configured_uses_the_runtime_of_the_selected_device() {
        let temporary = TemporaryDirectory::create();
        let plamo = PlamoState::new(&temporary.0);
        let model = tiny_model();
        write_model(&plamo.models_dir, &model);
        write_runtime(&plamo.runtime_dir, PlamoDevice::Cpu);
        let accepted = settings_with(&[(&license_key(), Value::Bool(true))]);

        assert!(
            !configured_for(&plamo, &accepted, &model),
            "gpu runtime is missing"
        );
        let cpu = settings_with(&[
            (&license_key(), Value::Bool(true)),
            (&device_key(), Value::String("cpu".into())),
        ]);
        assert!(configured_for(&plamo, &cpu, &model));
    }

    #[test]
    fn configured_rejects_a_model_id_outside_the_catalog() {
        let temporary = TemporaryDirectory::create();
        let plamo = PlamoState::new(&temporary.0);
        let settings = settings_with(&[
            (&license_key(), Value::Bool(true)),
            (&model_key(), Value::String("plamo-9".into())),
        ]);

        assert!(!configured(&plamo, &settings));
    }

    #[test]
    fn run_config_reports_the_first_missing_piece() {
        let temporary = TemporaryDirectory::create();
        let plamo = PlamoState::new(&temporary.0);
        let model = tiny_model();

        let error = run_config(&plamo, &settings_with(&[])).expect_err("license is missing");
        assert_eq!(error.code, "plamo_license_not_accepted");

        let accepted = settings_with(&[(&license_key(), Value::Bool(true))]);
        let error = run_config_for(&plamo, &model, PlamoDevice::Gpu)
            .expect_err("the model file is missing");
        assert_eq!(error.code, "plamo_not_ready");

        write_model(&plamo.models_dir, &model);
        let error =
            run_config_for(&plamo, &model, PlamoDevice::Gpu).expect_err("runtime is missing");
        assert_eq!(error.code, "plamo_not_ready");

        write_runtime(&plamo.runtime_dir, PlamoDevice::Gpu);
        let config =
            run_config_for(&plamo, &model, PlamoDevice::Gpu).expect("everything is installed");
        assert_eq!(config.device, PlamoDevice::Gpu);
        assert_eq!(config.model_id, "plamo-test");
        assert!(config.model_path.is_file());
        assert!(config.server_path.is_file());

        // 設定経路でも同じ結果になる（既定モデルは未ダウンロードなので not ready）。
        assert_eq!(
            run_config(&plamo, &accepted)
                .expect_err("default model is absent")
                .code,
            "plamo_not_ready"
        );
    }

    #[test]
    fn server_config_clamps_slots_to_the_supported_range() {
        let temporary = TemporaryDirectory::create();
        let config = PlamoRunConfig {
            model_id: default_model().id.to_owned(),
            device: PlamoDevice::Cpu,
            model_path: temporary.0.join("model.gguf"),
            server_path: temporary.0.join("llama-server.exe"),
        };

        assert_eq!(server_config(&config, 0).unwrap().slots, 1);
        assert_eq!(server_config(&config, 3).unwrap().slots, 3);
        assert_eq!(server_config(&config, 9).unwrap().slots, 4);
        assert_eq!(
            server_config(&config, 2).unwrap().ctx_per_slot,
            yc_llama::DEFAULT_CTX_PER_SLOT
        );
        assert_eq!(server_config(&config, 2).unwrap().backend, Backend::Cpu);
    }

    #[test]
    fn only_one_download_can_run_and_cancelling_reaches_the_token() {
        let temporary = TemporaryDirectory::create();
        let plamo = PlamoState::new(&temporary.0);

        assert!(!plamo.cancel_download(), "nothing is running yet");
        let guard = plamo.begin_download().expect("first download starts");
        assert!(plamo.is_downloading());
        assert!(
            plamo.begin_download().is_none(),
            "second download is refused"
        );

        assert!(plamo.cancel_download());
        assert!(guard.cancellation().is_cancelled());

        drop(guard);
        assert!(!plamo.is_downloading());
        assert!(plamo.begin_download().is_some(), "the slot is reusable");
    }

    #[test]
    fn the_active_model_is_tracked_for_deletion() {
        let temporary = TemporaryDirectory::create();
        let plamo = PlamoState::new(&temporary.0);
        let path = temporary.0.join("model.gguf");

        assert!(!plamo.uses_model(&path));
        plamo.set_active_model(path.clone());
        assert!(plamo.uses_model(&path));
        assert!(!plamo.uses_model(&temporary.0.join("other.gguf")));
        plamo.clear_active_model();
        assert!(!plamo.uses_model(&path));
    }

    #[test]
    fn part_path_matches_the_downloader_suffix() {
        assert_eq!(
            part_path(Path::new("models/a.gguf")),
            PathBuf::from("models/a.gguf.part")
        );
    }

    #[test]
    fn download_errors_map_to_actionable_messages() {
        assert_eq!(
            download_error(LlamaError::Network).user_message,
            yc_locale::t!("ai.plamoDownloadNetworkFailed")
        );
        assert_eq!(
            download_error(LlamaError::HttpStatus { status: 404 }).user_message,
            yc_locale::t!("ai.plamoDownloadNetworkFailed")
        );
        assert_eq!(
            download_error(LlamaError::ChecksumMismatch).user_message,
            yc_locale::t!("ai.plamoDownloadChecksumFailed")
        );
        assert_eq!(
            download_error(LlamaError::Io {
                message: "disk full".into()
            })
            .user_message,
            yc_locale::t!("ai.plamoDownloadWriteFailed")
        );
        assert_eq!(
            download_error(LlamaError::NotInstalled).user_message,
            yc_locale::t!("ai.taskFailed")
        );
    }

    #[test]
    fn runtime_start_errors_suggest_cpu_only_for_gpu() {
        assert_eq!(
            runtime_start_error(PlamoDevice::Gpu, LlamaError::NotInstalled).user_message,
            yc_locale::t!("ai.plamoRuntimeStartFailed")
        );
        assert_eq!(
            runtime_start_error(PlamoDevice::Cpu, LlamaError::NotInstalled).user_message,
            yc_locale::t!("ai.providerFailed")
        );
    }
}
