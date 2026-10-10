//! Projectディレクトリ、SQLite永続化、アプリ設定ファイルを管理する。

mod database;
mod error;
mod files;
mod glossary;
mod lock;
mod model;
mod project;
mod repository;
mod writer;

pub use error::{ProjectError, Result};
pub use files::{load_recent_projects, load_settings, save_recent_projects, save_settings};
pub use glossary::{GlossaryMatcher, format_glossary_export, parse_glossary_import};
pub use lock::{LockRecord, ProjectLock};
pub use model::{
    AnalysisData, AppSettings, AssetRecord, EntryUpdate, GlossaryTerm, GlossaryTermInput,
    IncludedNamespace, JarReplacementRecord, LanguageRecord, ModRecord, ModUnitRecord,
    NamespaceRecord, NestedArchiveRecord, ProjectAssetIndex, ProjectEntry, ProjectEntrySummary,
    ProjectMetadata, ProjectReviewSettings, ProjectSummary, RecentProject, RecentProjects,
    TranslationAssetMatch, TranslationProgress, UpdateOrigin,
};
pub use project::{
    OpenedProject, cleanup_trash_directories, create_project, delete_project, list_projects,
    normalize_project_name, open_project, rename_project,
};
pub use repository::Repository;
pub use writer::ProjectWriter;

#[cfg(test)]
mod tests;
