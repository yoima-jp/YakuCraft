use std::time::Duration;

use thiserror::Error;

/// AI Provider と資格情報操作で共通に使う、秘密情報を含まないエラー。
#[derive(Debug, Error, Eq, PartialEq)]
pub enum AiError {
    #[error("Provider URL is not allowed")]
    InvalidUrl,
    #[error("Provider configuration is invalid")]
    InvalidConfiguration,
    #[error("Network request failed")]
    Network,
    #[error("Provider authentication failed")]
    ProviderAuth,
    #[error("ChatGPTに再ログインしてください。")]
    ChatGptReauthenticationRequired,
    #[error("ChatGPTログインに使うポート1455が使用中です。使用中のアプリを閉じてください。")]
    ChatGptCallbackPortInUse,
    #[error("ChatGPTログインをキャンセルしました。")]
    ChatGptLoginCancelled,
    #[error("ChatGPTログインの有効期限が切れました。もう一度お試しください。")]
    ChatGptLoginTimedOut,
    #[error("ChatGPTにログインできませんでした。もう一度お試しください。")]
    ChatGptLoginFailed,
    #[error("Provider rate limit was reached")]
    ProviderRateLimit { retry_after: Option<Duration> },
    #[error("Provider returned an unsuccessful response (HTTP {status})")]
    ProviderResponse { status: u16 },
    #[error("Provider returned an invalid response")]
    InvalidProviderResponse,
    #[error("Credential store operation failed")]
    CredentialStore,
    #[error("A translation job is already running")]
    JobAlreadyRunning,
    #[error("Translation was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, AiError>;
