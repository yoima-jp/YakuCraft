use std::{io, path::PathBuf};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ProjectError>;

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("ファイル操作に失敗しました: {0}")]
    Io(#[from] io::Error),
    #[error("SQLite操作に失敗しました: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSONを読み書きできません: {0}")]
    Json(#[from] serde_json::Error),
    #[error("用語集CSVを読み書きできません: {0}")]
    Csv(#[from] csv::Error),
    #[error("時刻を処理できません: {0}")]
    TimeFormat(#[from] time::error::Format),
    #[error("時刻を解析できません: {0}")]
    TimeParse(#[from] time::error::Parse),
    #[error("source JARのSHA-256を計算できません: {0}")]
    Archive(#[from] yc_archive::ArchiveError),
    #[error("Project IDがUUIDではありません: {0}")]
    InvalidProjectId(String),
    #[error("Projectが見つかりません: {0}")]
    ProjectNotFound(PathBuf),
    #[error("Projectを削除用フォルダーへ移動できません: {0}")]
    ProjectDeleteRename(#[source] io::Error),
    #[error("Projectは別プロセスですでに開かれています")]
    ProjectAlreadyOpen,
    #[error("最後のmodはプロジェクトから外せません。")]
    LastModUnit,
    #[error("{0}")]
    InvalidProjectName(String),
    #[error("project.lockの内容を読み取れません")]
    InvalidLockRecord,
    #[error("DBのschema version {current}はこのアプリでは扱えません (最大 {maximum})")]
    UnsupportedSchemaVersion { current: u32, maximum: usize },
    #[error("migration {migration} を適用できません: {source}")]
    Migration {
        migration: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("DBの値が不正です: {0}")]
    InvalidDatabaseValue(String),
    #[error("translation entryが見つかりません: {0}")]
    EntryNotFound(i64),
    #[error("Project writer threadを開始できません: {0}")]
    WriterStartup(String),
    #[error("Project writer threadが停止しています")]
    WriterStopped,
}
