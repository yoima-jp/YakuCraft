use thiserror::Error;

#[derive(Debug, Error)]
pub enum LanguageError {
    #[error("言語JSONを解析できません: {0}")]
    Json(#[from] serde_json::Error),
}
