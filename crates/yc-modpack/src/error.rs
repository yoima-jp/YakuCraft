use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModpackError {
    #[error("unsafe or invalid instance-relative path: {0}")]
    InvalidPath(String),
    #[error("invalid FTB Quests SNBT in {path} at byte {offset}: {message}")]
    Snbt {
        path: String,
        offset: usize,
        line: usize,
        column: usize,
        message: String,
    },
    #[error("invalid KubeJS language JSON in {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("KubeJS language file {0} must be a JSON object containing only string values")]
    InvalidLanguageJson(String),
}

impl ModpackError {
    pub fn safe_detail(&self) -> String {
        match self {
            Self::Snbt {
                path, line, column, ..
            } => yc_language::parse_error_detail("SNBT", path, *line, *column),
            Self::Json { path, source } => yc_language::parse_error_detail(
                &format!("JSON {:?}", source.classify()),
                path,
                source.line(),
                source.column(),
            ),
            _ => self.to_string(),
        }
    }
}
