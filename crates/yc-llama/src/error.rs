use thiserror::Error;

/// llama.cpp ランタイムのダウンロード・展開・起動で発生するエラー。
///
/// メッセージには URL やローカルパスを含めない。呼び出し側（Tauri のコマンド層）が
/// そのまま UI へ出せるようにするため、原因の切り分けは variant で行う。
#[derive(Debug, Error)]
pub enum LlamaError {
    #[error("network request failed")]
    Network,
    #[error("server returned HTTP status {status}")]
    HttpStatus { status: u16 },
    #[error("downloaded size {actual} does not match expected {expected} bytes")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("downloaded data does not match the expected SHA-256 checksum")]
    ChecksumMismatch,
    /// ディスク容量不足やアクセス拒否など、ファイル操作の失敗。
    #[error("file operation failed: {message}")]
    Io { message: String },
    #[error("operation was cancelled")]
    Cancelled,
    #[error("download URL must use https")]
    InsecureUrl,
    #[error("archive entry is not safe to extract: {entry}")]
    UnsafeArchiveEntry { entry: String },
    #[error("archive does not contain the expected file: {path}")]
    MissingServerBinary { path: String },
    #[error("archive is not a valid {kind} archive: {message}")]
    InvalidArchive { kind: &'static str, message: String },
    #[error("runtime is not installed")]
    NotInstalled,
    #[error("server configuration is invalid: {message}")]
    InvalidConfiguration { message: String },
    #[error("llama-server exited before becoming ready ({status}):\n{log}")]
    ServerExited { status: String, log: String },
    #[error("llama-server did not become ready within {seconds} seconds:\n{log}")]
    ServerStartTimeout { seconds: u64, log: String },
    #[error("failed to start llama-server: {message}")]
    ServerSpawn { message: String },
    #[error("llama-server process control failed: {message}")]
    ProcessControl { message: String },
}

impl From<std::io::Error> for LlamaError {
    fn from(error: std::io::Error) -> Self {
        LlamaError::Io {
            message: error.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, LlamaError>;
