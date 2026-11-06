use thiserror::Error;

#[derive(Debug, Error)]
pub enum LanguageError {
    #[error("{}", yc_locale::t!("language.json_parse_failed", error = .0))]
    Json(#[from] serde_json::Error),
}
