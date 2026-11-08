//! 起動中サーバーを 1 つだけ保持するマネージャ。
//!
//! 翻訳ジョブは並行に走るが、llama.cpp のサーバーを 2 つ同時に立てるとモデルを二重に
//! メモリへ載せることになり、CPU 推論では現実的でない。そこで「要求された設定が同じなら
//! 使い回し、違えば入れ替える」という方針をここへ集約し、起動処理を async ミューテックスで
//! 直列化する。これが無いと、2 つのジョブが同時に起動を試みてサーバーが 2 つ立ち上がる。

use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::{
    LlamaError, Result,
    server::{LlamaServerLauncher, ServerConfig, ServerHandle, ServerLauncher},
};

/// 既定のアイドル停止時間。翻訳は連続して走ることが多く、短すぎると再ロードが頻発する。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// サーバーを使っている間だけ有効なリース。
///
/// このガードが生きている間はアイドル停止しない。落とすとアクティブ数が減り、
/// 0 になった時点からアイドル時間の計測が始まる。
pub struct ServerLease {
    base_url: String,
    api_key: String,
    inner: Arc<RuntimeManagerInner>,
}

impl ServerLease {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }
}

impl Drop for ServerLease {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::AcqRel);
        self.inner.touch();
    }
}

struct RunningServer {
    config: ServerConfig,
    handle: Box<dyn ServerHandle>,
}

/// 起動中サーバーを最大 1 つ保持するマネージャ。`Arc` で共有して使う。
pub struct RuntimeManager {
    inner: Arc<RuntimeManagerInner>,
}

impl Clone for RuntimeManager {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct RuntimeManagerInner {
    launcher: Arc<dyn ServerLauncher>,
    idle_timeout: Duration,
    /// 起動・停止を直列化する。2 つのジョブが同時に起動しないようにするため。
    operation: AsyncMutex<()>,
    running: Mutex<Option<RunningServer>>,
    /// リースを保持している呼び出し元の数。
    active: AtomicUsize,
    /// 最後にリースが返された時刻。アイドル判定の基準。
    last_used: Mutex<Instant>,
    /// アイドル監視タスクを 1 回だけ起動するためのフラグ。
    idle_task_started: Mutex<bool>,
    cancel: CancellationToken,
}

impl RuntimeManagerInner {
    fn touch(&self) {
        let mut guard = self.last_used.lock().expect("last_used mutex poisoned");
        *guard = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        let guard = self.last_used.lock().expect("last_used mutex poisoned");
        guard.elapsed()
    }

    fn is_idle(&self) -> bool {
        self.active.load(Ordering::Acquire) == 0
    }

    /// アイドル監視タスクを 1 度だけ起動する。
    fn start_idle_task_once(self: &Arc<Self>) {
        let mut started = self
            .idle_task_started
            .lock()
            .expect("idle_task_started mutex poisoned");
        if *started {
            return;
        }
        *started = true;
        drop(started);

        let idle_timeout = self.idle_timeout;
        if idle_timeout.is_zero() {
            return;
        }
        let cancel = self.cancel.clone();
        // Weak で持つ。タスクがマネージャの寿命を延ばすと、アプリ終了まで残り続けてしまう。
        let weak = Arc::downgrade(self);
        let tick = (idle_timeout / 4).max(Duration::from_millis(100));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(tick) => {}
                }
                let Some(inner) = Weak::upgrade(&weak) else {
                    break;
                };
                if inner.is_idle() && inner.idle_for() >= idle_timeout {
                    inner.stop_idle().await;
                }
            }
        });
    }

    async fn stop_idle(&self) {
        let _operation = self.operation.lock().await;
        // ロック待ちの間に使われ始めた可能性があるので、取得後にもう一度確認する。
        if !self.is_idle() {
            return;
        }
        let running = {
            let mut guard = self.running.lock().expect("running mutex poisoned");
            guard.take()
        };
        if let Some(running) = running {
            tracing::debug!("stopping idle llama-server");
            stop_handle(running.handle).await;
        }
    }
}

impl RuntimeManager {
    /// 実プロセスを起動するマネージャを作る。
    pub fn new() -> Self {
        Self::with_launcher(Arc::new(LlamaServerLauncher), DEFAULT_IDLE_TIMEOUT)
    }

    /// ランチャーとアイドル時間を指定して作る（テスト・設定変更用）。
    pub fn with_launcher(launcher: Arc<dyn ServerLauncher>, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(RuntimeManagerInner {
                launcher,
                idle_timeout,
                operation: AsyncMutex::new(()),
                running: Mutex::new(None),
                active: AtomicUsize::new(0),
                last_used: Mutex::new(Instant::now()),
                idle_task_started: Mutex::new(false),
                cancel: CancellationToken::new(),
            }),
        }
    }

    /// 要求された設定のサーバーを用意し、接続先と API キーを返す。
    ///
    /// 同じ設定のサーバーが生きていれば再利用する。設定が違う、または死んでいる場合は
    /// 停止してから起動し直す。返した [`ServerLease`] を保持している間はアイドル停止しない。
    pub async fn ensure_server(&self, config: &ServerConfig) -> Result<ServerLease> {
        let _operation = self.inner.operation.lock().await;

        // 再利用できるかを判定し、できなければ古いものを取り出す。
        let (stale, needs_spawn) = {
            let mut guard = self.inner.running.lock().expect("running mutex poisoned");
            match guard.as_ref() {
                Some(running) if running.config == *config && running.handle.is_running() => {
                    (None, false)
                }
                Some(_) => (guard.take(), true),
                None => (None, true),
            }
        };
        if let Some(stale) = stale {
            stop_handle(stale.handle).await;
        }

        if needs_spawn {
            let handle = self.inner.launcher.spawn(config).await?;
            let mut guard = self.inner.running.lock().expect("running mutex poisoned");
            *guard = Some(RunningServer {
                config: config.clone(),
                handle,
            });
        }

        let (base_url, api_key) = {
            let guard = self.inner.running.lock().expect("running mutex poisoned");
            let running = guard.as_ref().ok_or(LlamaError::NotInstalled)?;
            (
                running.handle.base_url().to_string(),
                running.handle.api_key().to_string(),
            )
        };

        self.inner.active.fetch_add(1, Ordering::AcqRel);
        self.inner.touch();
        self.inner.start_idle_task_once();

        Ok(ServerLease {
            base_url,
            api_key,
            inner: Arc::clone(&self.inner),
        })
    }

    /// サーバーが動いているか。テストと UI 表示用。
    pub fn is_running(&self) -> bool {
        let guard = self.inner.running.lock().expect("running mutex poisoned");
        guard
            .as_ref()
            .is_some_and(|running| running.handle.is_running())
    }

    /// 起動中のサーバーを停止する。何度呼んでも安全。
    pub async fn shutdown(&self) {
        let _operation = self.inner.operation.lock().await;
        self.inner.cancel.cancel();
        let running = {
            let mut guard = self.inner.running.lock().expect("running mutex poisoned");
            guard.take()
        };
        if let Some(running) = running {
            stop_handle(running.handle).await;
        }
    }

    /// 同期文脈から停止する。
    ///
    /// Tauri の `RunEvent::Exit` は非同期を待てないため、そこからはこちらを呼ぶ。
    /// 実装はプロセスの終了要求と待機を同期 API だけで行うので、非同期ランタイムの上から
    /// 呼んでもデッドロックしない。`async` 版の [`RuntimeManager::shutdown`] は
    /// `operation` ロックを取るため、非同期ランタイムの外から `block_on` すると逆に危険。
    pub fn shutdown_blocking(&self) {
        self.inner.cancel.cancel();
        let running = {
            let mut guard = self.inner.running.lock().expect("running mutex poisoned");
            guard.take()
        };
        if let Some(running) = running {
            running.handle.shutdown_blocking();
        }
    }
}

impl Default for RuntimeManager {
    fn default() -> Self {
        Self::new()
    }
}

async fn stop_handle(handle: Box<dyn ServerHandle>) {
    handle.shutdown().await;
    drop(handle);
}
