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
    #[error("{}", yc_locale::t!("ai.chatgptReauthenticationRequired"))]
    ChatGptReauthenticationRequired,
    #[error("{}", yc_locale::t!("ai.chatgptCallbackPortInUse"))]
    ChatGptCallbackPortInUse,
    #[error("{}", yc_locale::t!("ai.chatgptLoginCancelled"))]
    ChatGptLoginCancelled,
    #[error("{}", yc_locale::t!("ai.chatgptLoginTimedOut"))]
    ChatGptLoginTimedOut,
    #[error("{}", yc_locale::t!("ai.chatgptLoginFailed"))]
    ChatGptLoginFailed,
    #[error("Provider rate limit was reached")]
    ProviderRateLimit { retry_after: Option<Duration> },
    #[error("Provider returned an unsuccessful response (HTTP {status})")]
    ProviderResponse { status: u16 },
    #[error("Provider returned an invalid response")]
    InvalidProviderResponse,
    #[error("{}", yc_locale::t!("ai.deeplTargetUnsupported"))]
    UnsupportedTargetLanguage,
    #[error("Credential store operation failed")]
    CredentialStore,
    #[error("A translation job is already running")]
    JobAlreadyRunning,
    #[error("Translation was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, AiError>;
