//! `llama-server` のプロセス管理。
//!
//! サーバーはループバック（127.0.0.1）にだけバインドし、ランダムな API キーを要求する。
//! ローカルで動く llama.cpp は認証が無いと同一マシンの他プロセスから自由に使えてしまい、
//! モデルの読み込みや推論を外部から叩ける状態になるため、キーは必ず付ける。
//!
//! ポートは固定値を使わず、その都度空きポートを取る。Mozc など他のアプリが
//! 同じ既定ポート（9931）で llama-server を動かしている環境があり、固定値だと衝突する。

use std::{
    collections::VecDeque,
    future::Future,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

use crate::{LlamaError, Result, builds::Backend};

/// 同時に持てるスロット数の下限・上限。翻訳ジョブの並列度に合わせて 4 までを許す。
pub const MIN_SLOTS: u8 = 1;
pub const MAX_SLOTS: u8 = 4;

/// 1 スロットあたりのコンテキスト長の既定値。
///
/// PLaMo 3 Translate のプロンプトは用語集と本文で数千トークンになることがあるため、
/// 8192 を既定にしている。`-c` へは「スロット数 × この値」を渡す。
pub const DEFAULT_CTX_PER_SLOT: u32 = 8192;

/// 起動待ちの既定値。CPU 推論で大きなモデルを読むと数分かかることがある。
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(180);

/// 起動ログから保持する行数。起動失敗時のエラーへ添える。
const LOG_BUFFER_LINES: usize = 50;

/// 起動時のログを先頭から残す上限。
///
/// [`LOG_BUFFER_LINES`] の輪状バッファは「直近 50 行」しか残らないため、モデル読み込み中の
/// 情報（どのデバイスへ何レイヤー載ったか等）は起動完了時点で押し出されていることがある。
/// 起動時の診断情報として先頭から一定量を別に保持する。
const STARTUP_LOG_LINES: usize = 512;

/// ヘルスチェックの間隔。
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// 停止時にプロセスの終了を待つ時間。過ぎたら強制終了する。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `llama-server` の起動設定。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerConfig {
    pub server_path: PathBuf,
    pub model_path: PathBuf,
    pub backend: Backend,
    pub slots: u8,
    pub ctx_per_slot: u32,
}

impl ServerConfig {
    /// 既定値（1 スロット、8192 コンテキスト）で設定を作る。
    pub fn new(
        server_path: impl Into<PathBuf>,
        model_path: impl Into<PathBuf>,
        backend: Backend,
    ) -> Self {
        Self {
            server_path: server_path.into(),
            model_path: model_path.into(),
            backend,
            slots: 1,
            ctx_per_slot: DEFAULT_CTX_PER_SLOT,
        }
    }

    /// スロット数を設定する。範囲外は [`LlamaError::InvalidConfiguration`] になる。
    pub fn with_slots(mut self, slots: u8) -> Result<Self> {
        if !(MIN_SLOTS..=MAX_SLOTS).contains(&slots) {
            return Err(LlamaError::InvalidConfiguration {
                message: format!("slots must be between {MIN_SLOTS} and {MAX_SLOTS}"),
            });
        }
        self.slots = slots;
        Ok(self)
    }

    /// 1 スロットあたりのコンテキスト長を設定する。
    pub fn with_ctx_per_slot(mut self, ctx_per_slot: u32) -> Result<Self> {
        if ctx_per_slot == 0 {
            return Err(LlamaError::InvalidConfiguration {
                message: "ctx_per_slot must be greater than zero".to_string(),
            });
        }
        self.ctx_per_slot = ctx_per_slot;
        Ok(self)
    }

    /// `llama-server` へ渡す `-c` の値。全スロット分をまとめた総コンテキスト長。
    pub fn total_ctx(&self) -> u64 {
        u64::from(self.ctx_per_slot) * u64::from(self.slots)
    }

    fn validate(&self) -> Result<()> {
        if !(MIN_SLOTS..=MAX_SLOTS).contains(&self.slots) {
            return Err(LlamaError::InvalidConfiguration {
                message: format!("slots must be between {MIN_SLOTS} and {MAX_SLOTS}"),
            });
        }
        if self.ctx_per_slot == 0 {
            return Err(LlamaError::InvalidConfiguration {
                message: "ctx_per_slot must be greater than zero".to_string(),
            });
        }
        if !self.server_path.is_file() {
            return Err(LlamaError::NotInstalled);
        }
        if !self.model_path.is_file() {
            return Err(LlamaError::InvalidConfiguration {
                message: "model file does not exist".to_string(),
            });
        }
        Ok(())
    }

    /// テスト専用の入口。ファイルの存在確認を飛ばして範囲検証だけを行う。
    #[cfg(test)]
    pub(crate) fn validate_for_test(&self) -> Result<()> {
        if !(MIN_SLOTS..=MAX_SLOTS).contains(&self.slots) {
            return Err(LlamaError::InvalidConfiguration {
                message: format!("slots must be between {MIN_SLOTS} and {MAX_SLOTS}"),
            });
        }
        if self.ctx_per_slot == 0 {
            return Err(LlamaError::InvalidConfiguration {
                message: "ctx_per_slot must be greater than zero".to_string(),
            });
        }
        Ok(())
    }

    /// 相対パスを呼び出し元の CWD を基準に絶対パスへ直した設定を返す。
    ///
    /// 子プロセスの CWD はサーバーバイナリのディレクトリへ変えるので、相対パスをそのまま
    /// 渡すと別のファイルを指してしまう。`canonicalize` は存在しないパスで失敗するため、
    /// その場合は元のパスを残す（呼び出し前の検証で既に弾かれている）。
    fn with_absolute_paths(&self) -> Self {
        let mut resolved = self.clone();
        if let Ok(path) = std::fs::canonicalize(&self.server_path) {
            resolved.server_path = path;
        }
        if let Ok(path) = std::fs::canonicalize(&self.model_path) {
            resolved.model_path = path;
        }
        resolved
    }

    /// b12110 の `llama-server --help` で実在を確認したフラグだけを組み立てる。
    pub fn arguments(&self, port: u16, api_key: &str) -> Vec<String> {
        let mut args = vec![
            "--host".to_string(),
            "127.0.0.1".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--api-key".to_string(),
            api_key.to_string(),
            "-m".to_string(),
            self.model_path.display().to_string(),
            // 総コンテキスト長。1 スロットあたりの長さは -np と合わせて配分される。
            "-c".to_string(),
            self.total_ctx().to_string(),
            "-np".to_string(),
            self.slots.to_string(),
            // スロットごとに独立した KV キャッシュを持たせ、-c をちょうど slots で割った
            // 長さを各スロットへ割り当てる。unified のままだと 1 本の共有バッファを
            // 奪い合い、1 スロットあたりの実効長が要求より小さくなることがある。
            "--no-kv-unified".to_string(),
            // 同梱の Web UI は不要。外部からの操作面を減らすため無効化する。
            "--no-ui".to_string(),
            // 空きメモリに合わせて -ngl や -c を勝手に調整させない。こちらで明示した値を
            // そのまま使わせる（ctx_per_slot の保証を崩さないため）。
            "--fit".to_string(),
            "off".to_string(),
        ];
        match self.backend {
            Backend::Cpu => {
                // CPU ビルドには Vulkan デバイスが無いが、ドライバ由来のデバイスを拾わないよう
                // 明示的に無効化し、レイヤーも 1 枚もオフロードしない。
                args.push("--device".to_string());
                args.push("none".to_string());
                args.push("-ngl".to_string());
                args.push("0".to_string());
            }
            Backend::Gpu => {
                // Vulkan ビルドで全レイヤーを GPU へ載せる。
                args.push("-ngl".to_string());
                args.push("all".to_string());
            }
        }
        args
    }
}

/// 起動済みサーバーの操作面。テストでは偽実装に差し替える。
pub trait ServerHandle: Send + Sync {
    fn base_url(&self) -> &str;
    fn api_key(&self) -> &str;
    fn is_running(&self) -> bool;
    fn shutdown(&self) -> BoxFuture<'_, ()>;
    /// 同期文脈からの停止。`RunEvent::Exit` のような非同期を待てない場所で使う。
    fn shutdown_blocking(&self);
}

/// `llama-server` を起動する戦略。テストでプロセス起動を差し替えるために抽象化している。
pub trait ServerLauncher: Send + Sync {
    fn spawn<'a>(
        &'a self,
        config: &'a ServerConfig,
    ) -> BoxFuture<'a, Result<Box<dyn ServerHandle>>>;
}

/// 実プロセスを起動する既定のランチャー。
#[derive(Clone, Copy, Debug, Default)]
pub struct LlamaServerLauncher;

impl ServerLauncher for LlamaServerLauncher {
    fn spawn<'a>(
        &'a self,
        config: &'a ServerConfig,
    ) -> BoxFuture<'a, Result<Box<dyn ServerHandle>>> {
        Box::pin(async move {
            let server = LlamaServer::start(config).await?;
            Ok(Box::new(server) as Box<dyn ServerHandle>)
        })
    }
}

/// 起動中の `llama-server`。
pub struct LlamaServer {
    base_url: String,
    api_key: String,
    child: Mutex<Option<Child>>,
    /// 終了済みかどうか。`child` を `None` にした後も状態を保つ。
    stopped: AtomicBool,
    logs: Arc<Mutex<LogBuffers>>,
    /// Windows の Job Object。ハンドルを閉じるとジョブ内のプロセスが道連れになる。
    #[cfg(windows)]
    _job: JobHandle,
    readers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// サーバーのログ保持領域。
struct LogBuffers {
    /// 直近 [`LOG_BUFFER_LINES`] 行。エラーメッセージに添える。
    recent: VecDeque<String>,
    /// 起動時の先頭 [`STARTUP_LOG_LINES`] 行。デバイスやオフロードの記録がここに残る。
    startup: Vec<String>,
}

impl LogBuffers {
    fn new() -> Self {
        Self {
            recent: VecDeque::with_capacity(LOG_BUFFER_LINES),
            startup: Vec::with_capacity(STARTUP_LOG_LINES),
        }
    }

    fn push(&mut self, line: String) {
        if self.startup.len() < STARTUP_LOG_LINES {
            self.startup.push(line.clone());
        }
        if self.recent.len() == LOG_BUFFER_LINES {
            self.recent.pop_front();
        }
        self.recent.push_back(line);
    }
}

impl LlamaServer {
    /// サーバーを起動し、`/health` が 200 を返すまで待つ。
    pub async fn start(config: &ServerConfig) -> Result<Self> {
        config.validate()?;
        // 子プロセスの CWD はサーバーバイナリのディレクトリへ変えるため、相対パスのまま渡すと
        // 呼び出し元の CWD ではなくそちらを基準に解決されてしまう。検証したときと同じ場所を
        // 指すよう、ここで絶対パスへ直してから起動する。
        let config = config.with_absolute_paths();
        let port = free_loopback_port()?;
        let api_key = random_api_key();
        let args = config.arguments(port, &api_key);

        let working_dir = config
            .server_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let mut command = Command::new(&config.server_path);
        command
            .args(&args)
            // サーバーバイナリと同じディレクトリを CWD にする。Windows は DLL、Linux は
            // 共有ライブラリを実行ファイルからの相対で探すため、CWD が違うと起動に失敗する。
            .current_dir(&working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // ハンドルを落としたらプロセスも道連れにする（最後の砦）。
            .kill_on_drop(true);

        // Windows は GUI アプリなので、コンソールウィンドウが一瞬でも出てはいけない。
        // CREATE_NO_WINDOW を付けないと子プロセスのコンソールが画面に現れる。
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        // Linux は親が死んだら子も死ぬようにする（YakuCraft がクラッシュしても孤児を残さない）。
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: pre_exec は fork 後 exec 前に走る。prctl は async-signal-safe な
            // システムコールだけで、確保やロックを行わない。
            unsafe {
                command.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    Ok(())
                });
            }
        }

        let mut child = command.spawn().map_err(|error| LlamaError::ServerSpawn {
            message: error.to_string(),
        })?;

        // 起動直後に Job Object へ入れる。これは必須の後始末機構なので、失敗したら
        // 起動を諦めて子プロセスを終了させる。ジョブに入らないまま動かすと、YakuCraft が
        // クラッシュしたときにモデルを読み込んだ llama-server が孤児として残り続ける。
        #[cfg(windows)]
        let job = match assign_to_kill_on_close_job(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.start_kill();
                let _ = child.try_wait();
                return Err(LlamaError::ProcessControl {
                    message: format!("could not assign llama-server to a job object: {error}"),
                });
            }
        };

        let logs = Arc::new(Mutex::new(LogBuffers::new()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(spawn_log_reader(stdout, Arc::clone(&logs), "stdout"));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(spawn_log_reader(stderr, Arc::clone(&logs), "stderr"));
        }

        let server = Self {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key,
            child: Mutex::new(Some(child)),
            stopped: AtomicBool::new(false),
            logs,
            #[cfg(windows)]
            _job: job,
            readers: Mutex::new(readers),
        };

        match server.wait_until_ready(DEFAULT_STARTUP_TIMEOUT).await {
            Ok(()) => {
                tracing::debug!(port, "llama-server is ready");
                Ok(server)
            }
            Err(error) => {
                // 起動に失敗したプロセスを残さない。
                server.shutdown().await;
                Err(error)
            }
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn is_running(&self) -> bool {
        if self.stopped.load(Ordering::Acquire) {
            return false;
        }
        let mut guard = self.child.lock().expect("child mutex poisoned");
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                Ok(Some(_)) => {
                    self.stopped.store(true, Ordering::Release);
                    false
                }
                Err(_) => false,
            },
            None => false,
        }
    }

    /// 直近のログ行（最大 [`LOG_BUFFER_LINES`] 行）。起動失敗の原因調査用。
    pub fn recent_log(&self) -> String {
        let guard = self.logs.lock().expect("log mutex poisoned");
        guard.recent.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// 起動時のログ（先頭から最大 [`STARTUP_LOG_LINES`] 行）。
    ///
    /// デバイスの選択や GPU へのレイヤーオフロードはモデル読み込み中に記録されるため、
    /// 起動完了後の [`LlamaServer::recent_log`] では押し出されていることがある。
    pub fn startup_log(&self) -> String {
        let guard = self.logs.lock().expect("log mutex poisoned");
        guard.startup.join("\n")
    }

    async fn wait_until_ready(&self, timeout: Duration) -> Result<()> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| LlamaError::Network)?;
        let health = format!("{}/health", self.base_url);
        let deadline = Instant::now() + timeout;

        loop {
            if !self.is_running() {
                return Err(LlamaError::ServerExited {
                    status: self.exit_status(),
                    log: self.recent_log(),
                });
            }
            if Instant::now() >= deadline {
                return Err(LlamaError::ServerStartTimeout {
                    seconds: timeout.as_secs(),
                    log: self.recent_log(),
                });
            }

            // b12110 の /health は認証不要だが、将来のビルドで要求されても困らないよう付ける。
            if let Ok(response) = client.get(&health).bearer_auth(&self.api_key).send().await
                && response.status().is_success()
            {
                return Ok(());
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    fn exit_status(&self) -> String {
        let mut guard = self.child.lock().expect("child mutex poisoned");
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => status.to_string(),
                Ok(None) => "still running".to_string(),
                Err(error) => format!("unknown ({error})"),
            },
            None => "already stopped".to_string(),
        }
    }

    /// プロセスを終了させ、終了を待つ。何度呼んでも安全。
    ///
    /// まず穏当な終了要求を送り、[`SHUTDOWN_GRACE`] の間だけ自力終了を待つ。
    /// それでも生きていれば強制終了する。
    pub async fn shutdown(&self) {
        let mut child = self.take_child();
        let Some(mut child) = child.take() else {
            return;
        };

        request_graceful_termination(&mut child);
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok(None) => {
                    // 猶予内に終わらなかったので強制終了する（`kill` は待機まで行う）。
                    let _ = child.kill().await;
                    break;
                }
                Err(_) => break,
            }
        }
        drop(child);
    }

    /// 同期文脈から停止する。Tauri の `RunEvent::Exit` のような、非同期を待てない場所向け。
    ///
    /// 終了要求と待機はすべて同期 API（`libc::kill` / `start_kill` / `try_wait`）で行うため、
    /// 非同期ランタイムの上から呼んでもデッドロックしない。
    pub fn shutdown_blocking(&self) {
        let mut child = self.take_child();
        let Some(mut child) = child.take() else {
            return;
        };
        request_graceful_termination(&mut child);
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    // 猶予内に終わらなかったので強制終了する。
                    let _ = child.start_kill();
                    let _ = child.try_wait();
                    break;
                }
                Err(_) => break,
            }
        }
    }

    /// 子プロセスを取り出し、読み取りタスクを止める。
    fn take_child(&self) -> Option<Child> {
        self.stopped.store(true, Ordering::Release);
        let child = {
            let mut guard = self.child.lock().expect("child mutex poisoned");
            guard.take()
        };
        let handles = {
            let mut guard = self.readers.lock().expect("reader mutex poisoned");
            std::mem::take(&mut *guard)
        };
        for handle in handles {
            handle.abort();
        }
        child
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        // 明示的な shutdown を忘れてもプロセスを残さない。
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.start_kill();
        }
        if let Ok(mut guard) = self.readers.lock() {
            for handle in guard.drain(..) {
                handle.abort();
            }
        }
    }
}

/// 子プロセスへ「穏当な終了要求」を送る。
///
/// tokio の `Child::start_kill` は Unix では SIGKILL（即時強制終了）なので、猶予付きの停止には
/// 使えない。Unix では SIGTERM を送り、llama-server が後始末（ファイルの flush など）を
/// してから終了する機会を与える。
///
/// Windows には SIGTERM に相当する仕組みが無く、`TerminateProcess` は常に強制終了になる。
/// そのため Windows では猶予フェーズを省き、`start_kill` の結果をそのまま待つ。
fn request_graceful_termination(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SAFETY: 自分が起動した子プロセスへ SIGTERM を送るだけ。pid が再利用されていた
            // 場合でも、対象は同じプロセスグループの短命な子で、ここでは待機で検知する。
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            return;
        }
    }
    // フォールバック（および Windows）。強制終了の要求を出し、呼び出し側が終了を待つ。
    let _ = child.start_kill();
}

impl ServerHandle for LlamaServer {
    fn base_url(&self) -> &str {
        LlamaServer::base_url(self)
    }
    fn api_key(&self) -> &str {
        LlamaServer::api_key(self)
    }

    fn is_running(&self) -> bool {
        LlamaServer::is_running(self)
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(LlamaServer::shutdown(self))
    }

    fn shutdown_blocking(&self) {
        LlamaServer::shutdown_blocking(self);
    }
}

fn spawn_log_reader<R>(
    reader: R,
    logs: Arc<Mutex<LogBuffers>>,
    stream: &'static str,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    // プロンプト本文がログへ流れることはないが、念のため debug に留める。
                    tracing::debug!(target: "yc_llama::server", stream, "{line}");
                    let mut guard = logs.lock().expect("log mutex poisoned");
                    guard.push(line);
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    })
}

/// 127.0.0.1 の空きポートを 1 つ確保する。
///
/// バインドしてすぐ解放するため、取得から起動までの間に他プロセスが取る可能性はゼロではない。
/// その場合は `llama-server` の起動失敗として表面化する。
fn free_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// ランダムな API キー。推測されないことだけが要件なので UUID v4 で十分。
fn random_api_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` を設定した Job Object へ子プロセスを割り当てる。
///
/// YakuCraft がクラッシュ・強制終了したとき、ハンドルが閉じてジョブ内の `llama-server` も
/// 道連れになる。これが無いと、モデルを読み込んだままのプロセスが残り続ける。
#[cfg(windows)]
fn assign_to_kill_on_close_job(child: &Child) -> std::io::Result<JobHandle> {
    use std::io;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    // SAFETY: 引数はどちらも null 可（無名ジョブを作る）。
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = JobHandle { raw };

    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: info は正しい型・サイズで初期化済み。
    let configured = unsafe {
        SetInformationJobObject(
            job.raw,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(info).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if configured == 0 {
        return Err(io::Error::last_os_error());
    }

    let process_handle = child
        .raw_handle()
        .ok_or_else(|| io::Error::other("llama-server process handle is unavailable"))?;
    // SAFETY: raw_handle は子プロセスの有効なハンドル。ジョブは戻り値として保持される。
    let assigned = unsafe { AssignProcessToJobObject(job.raw, process_handle.cast()) };
    if assigned == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(job)
}

/// ジョブハンドルの寿命をサーバーの寿命に合わせるためのラッパー。
#[cfg(windows)]
pub struct JobHandle {
    raw: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        // SAFETY: CreateJobObjectW が返した有効なハンドルを 1 回だけ閉じる。
        // ここで閉じることで KILL_ON_JOB_CLOSE が働き、残った子プロセスも終了する。
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.raw) };
    }
}

// HANDLE はカーネルオブジェクトへの不透明なポインタで、複数スレッドから参照カウント付きで
// 安全に扱える。Rust の型としては `*mut c_void` なので Send/Sync を明示する。
#[cfg(windows)]
unsafe impl Send for JobHandle {}
#[cfg(windows)]
unsafe impl Sync for JobHandle {}
