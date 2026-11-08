//! llama.cpp ランタイム層。
//!
//! llama.cpp のサーバーバイナリと GGUF モデルはアプリへ同梱せず、必要になった時点で
//! 固定ビルドをダウンロード・検証して起動する。ここではその汎用部分
//! （固定ビルド表、検証付きダウンロード、展開、`llama-server` のプロセス管理）だけを扱う。
//! モデルの選定やプロンプト、HTTP 越しの翻訳呼び出しは `yc-plamo` が担う。

mod builds;
mod download;
mod error;
mod install;
mod manager;
mod server;

#[cfg(test)]
mod tests;

pub use builds::{
    ArchiveKind, Backend, PINNED_TAG, RUNTIME_BUILDS, RuntimeBuild, current_target_build,
};
pub use download::{download_verified, validate_download_url};
pub use error::{LlamaError, Result};
pub use install::{ensure_runtime, installed_runtime, remove_runtime};
pub use manager::{DEFAULT_IDLE_TIMEOUT, RuntimeManager, ServerLease};
pub use server::{
    DEFAULT_CTX_PER_SLOT, DEFAULT_STARTUP_TIMEOUT, LlamaServer, LlamaServerLauncher, MAX_SLOTS,
    MIN_SLOTS, ServerConfig, ServerHandle, ServerLauncher,
};
