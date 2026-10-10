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
        Self {
            code: code.to_owned(),
            user_message: user_message.to_owned(),
            detail: detail.into(),
            recoverable: true,
        }
    }

    pub fn simple(code: &str, user_message: &str) -> Self {
        Self::new(code, user_message, None)
    }
}
