use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModpackError {
    #[error("{}", yc_locale::t!("packaging.modpack_invalid_path", path = .0))]
    InvalidPath(String),
    #[error(
        "{}",
        yc_locale::t!(
            "packaging.modpack_invalid_snbt",
            path = path,
            offset = offset,
            message = message
        )
    )]
    Snbt {
        path: String,
        offset: usize,
        line: usize,
        column: usize,
        message: String,
    },
    #[error(
        "{}",
        yc_locale::t!(
            "packaging.modpack_invalid_json",
            path = path,
            error = .source
        )
    )]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "{}",
        yc_locale::t!("packaging.modpack_language_json_object", path = .0)
    )]
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
