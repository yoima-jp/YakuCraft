use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: String,
    pub user_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub recoverable: bool,
}

impl AppError {
    pub fn new(code: &str, user_message: &str, detail: impl Into<Option<String>>) -> Self {
        let detail = detail.into();
        let safe_detail = detail
            .as_deref()
            .map(crate::logging::sanitize_sensitive_lines);
        let safe_user_message = crate::logging::sanitize_sensitive_lines(user_message);
        // 全コマンドの共通エラー型で、画面文言に置き換わる前の原因を記録する。
        tracing::warn!(
            error_code = code,
            user_message = %safe_user_message,
            detail = ?safe_detail,
            "App command failed"
        );

        Self {
            code: code.to_owned(),
            user_message: user_message.to_owned(),
            detail,
            recoverable: true,
        }
    }

    pub fn simple(code: &str, user_message: &str) -> Self {
        Self::new(code, user_message, None)
    }
}
