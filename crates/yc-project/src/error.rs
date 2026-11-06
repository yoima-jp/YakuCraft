use std::{io, path::PathBuf};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ProjectError>;

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("{}", yc_locale::t!("storage.io", error = .0))]
    Io(#[from] io::Error),
    #[error("{}", yc_locale::t!("storage.sqlite", error = .0))]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "{}",
        yc_locale::t!(
            "storage.json_read_write",
            detail = yc_language::parse_error_detail(
                &format!("JSON {:?}", .0.classify()),
                "<unknown>",
                .0.line(),
                .0.column()
            )
        )
    )]
    Json(#[from] serde_json::Error),
    #[error(
        "{}",
        yc_locale::t!(
            "storage.json_read_write",
            detail = yc_language::parse_error_detail(
                &format!("JSON {:?}", .source.classify()),
                &.path.to_string_lossy(),
                .source.line(),
                .source.column()
            )
        )
    )]
    JsonFile {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{}", yc_locale::t!("storage.glossary_csv", error = .0))]
    Csv(#[from] csv::Error),
    #[error("{}", yc_locale::t!("storage.time_format", error = .0))]
    TimeFormat(#[from] time::error::Format),
    #[error("{}", yc_locale::t!("storage.time_parse", error = .0))]
    TimeParse(#[from] time::error::Parse),
    #[error("{}", yc_locale::t!("storage.source_jar_hash", error = .0))]
    Archive(#[from] yc_archive::ArchiveError),
    #[error("{}", yc_locale::t!("storage.invalid_project_id", id = .0))]
    InvalidProjectId(String),
    #[error("{}", yc_locale::t!("storage.project_not_found", path = .0.display()))]
    ProjectNotFound(PathBuf),
    #[error("{}", yc_locale::t!("storage.project_delete_rename", error = .0))]
    ProjectDeleteRename(#[source] io::Error),
    #[error("{}", yc_locale::t!("storage.project_already_open"))]
    ProjectAlreadyOpen,
    #[error("{}", yc_locale::t!("storage.last_mod_unit"))]
    LastModUnit,
    #[error("{0}")]
    InvalidProjectName(String),
    #[error("{}", yc_locale::t!("storage.invalid_lock_record"))]
    InvalidLockRecord,
    #[error(
        "{}",
        yc_locale::t!(
            "storage.unsupported_schema_version",
            current = current,
            maximum = maximum
        )
    )]
    UnsupportedSchemaVersion { current: u32, maximum: usize },
    #[error(
        "{}",
        yc_locale::t!(
            "storage.migration_failed",
            migration = migration,
            error = .source
        )
    )]
    Migration {
        migration: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("{}", yc_locale::t!("storage.invalid_database_value", value = .0))]
    InvalidDatabaseValue(String),
    #[error("{}", yc_locale::t!("storage.entry_not_found", id = .0))]
    EntryNotFound(i64),
    #[error("{}", yc_locale::t!("storage.writer_startup", error = .0))]
    WriterStartup(String),
    #[error("{}", yc_locale::t!("storage.writer_stopped"))]
    WriterStopped,
}
