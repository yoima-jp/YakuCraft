use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use yc_language::{EntryFlag, LanguageFileFormat, TranslationOrigin};

pub const CURRENT_SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const CURRENT_RECENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModUnitSourceKind {
    #[default]
    Jar,
    Modpack,
}

impl ModUnitSourceKind {
    pub(crate) fn as_db(self) -> &'static str {
        match self {
            Self::Jar => "jar",
            Self::Modpack => "modpack",
        }
    }

    pub(crate) fn from_db(value: &str) -> crate::Result<Self> {
        match value {
            "jar" => Ok(Self::Jar),
            "modpack" => Ok(Self::Modpack),
            _ => Err(crate::ProjectError::InvalidDatabaseValue(format!(
                "mod_units.source_kind={value}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationEntryKind {
    #[default]
    Lang,
    Patchouli,
    Ftbquests,
}

impl TranslationEntryKind {
    pub(crate) fn as_db(self) -> &'static str {
        match self {
            Self::Lang => "lang",
            Self::Patchouli => "patchouli",
            Self::Ftbquests => "ftbquests",
        }
    }

    pub(crate) fn from_db(value: &str) -> crate::Result<Self> {
        match value {
            "lang" => Ok(Self::Lang),
            "patchouli" => Ok(Self::Patchouli),
            "ftbquests" => Ok(Self::Ftbquests),
            _ => Err(crate::ProjectError::InvalidDatabaseValue(format!(
                "translation_entries.entry_kind={value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// 設定キーは他のcrateで解釈するため、読み書き時に未知の値も保持する。
    #[serde(flatten)]
    pub values: BTreeMap<String, serde_json::Value>,
}

impl AppSettings {
    pub fn new() -> Self {
        Self {
            schema_version: CURRENT_SETTINGS_SCHEMA_VERSION,
            values: BTreeMap::new(),
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::new()
    }
}

fn default_schema_version() -> u32 {
    CURRENT_SETTINGS_SCHEMA_VERSION
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModRecord {
    #[serde(default)]
    pub mod_unit_id: i64,
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub loader: Option<String>,
    pub minecraft_version: Option<String>,
    pub metadata_source: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModUnitRecord {
    pub id: i64,
    #[serde(default)]
    pub source_kind: ModUnitSourceKind,
    pub sort_index: usize,
    pub source_filename: String,
    pub source_sha256: String,
    pub stored_path: String,
    pub original_path: String,
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub loader: Option<String>,
    pub minecraft_version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JarReplacementRecord {
    pub id: i64,
    pub mod_unit_id: i64,
    pub target_path: String,
    pub backup_path: String,
    pub previous_sha256: String,
    pub written_sha256: String,
    pub created_at: String,
    pub restored_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDeploymentRecord {
    pub id: i64,
    pub mod_unit_id: i64,
    /// インスタンス内の配置先パス。
    pub target_path: String,
    /// false の場合、復元時は配置後ファイルを削除する。
    pub existed_before: bool,
    /// 既存ファイルがあった場合の配置前 SHA-256。
    pub previous_sha256: Option<String>,
    /// 既存ファイルがあった場合のバックアップ保存先。
    pub backup_path: Option<String>,
    /// 復元時に配置後ファイルが変更されていないか照合する SHA-256。
    pub deployed_sha256: String,
    pub deployed_at: String,
    pub restored_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryTerm {
    pub id: i64,
    pub source_term: String,
    pub target_term: String,
    pub note: Option<String>,
    pub case_sensitive: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryTermInput {
    pub source_term: String,
    pub target_term: String,
    pub note: Option<String>,
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncludedNamespace {
    pub mod_unit_id: i64,
    pub namespace: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceRecord {
    pub mod_unit_id: i64,
    pub namespace: String,
    pub container_path: String,
    pub included: bool,
    pub entry_count: usize,
}

impl<'de> Deserialize<'de> for NamespaceRecord {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct NamespaceRecordInput {
            #[serde(default)]
            mod_unit_id: i64,
            namespace: String,
            #[serde(default)]
            container_path: String,
            #[serde(default)]
            included: Option<bool>,
            #[serde(default)]
            entry_count: usize,
        }

        let input = NamespaceRecordInput::deserialize(deserializer)?;
        Ok(Self {
            mod_unit_id: input.mod_unit_id,
            namespace: input.namespace,
            //Root由来は既定で対象、Nested由来は利用者が選ぶまで対象外にする。
            included: input.included.unwrap_or(input.container_path.is_empty()),
            container_path: input.container_path,
            entry_count: input.entry_count,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedArchiveRecord {
    #[serde(default)]
    pub mod_unit_id: i64,
    pub archive_path: String,
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub has_languages: bool,
    pub skipped_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageRecord {
    #[serde(default)]
    pub mod_unit_id: i64,
    pub namespace: String,
    pub locale: String,
    pub format: LanguageFileFormat,
    #[serde(default)]
    pub container_path: String,
    pub archive_path: String,
    pub entry_count: usize,
    #[serde(default)]
    pub is_target: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisData {
    pub mods: Vec<ModRecord>,
    pub namespaces: Vec<NamespaceRecord>,
    pub nested_archives: Vec<NestedArchiveRecord>,
    pub languages: Vec<LanguageRecord>,
    pub entries: Vec<yc_language::TranslationEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct ProjectMetadata {
    pub mods: Vec<ModRecord>,
    pub namespaces: Vec<NamespaceRecord>,
    pub nested_archives: Vec<NestedArchiveRecord>,
    pub languages: Vec<LanguageRecord>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProjectEntrySummary {
    pub total: usize,
    pub translated: usize,
    pub needs_review: usize,
    pub existing_japanese: usize,
    pub unresolved_assets: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetRecord {
    pub namespace: String,
    pub asset_type: String,
    pub resource_id: String,
    pub archive_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationAssetMatch {
    pub namespace: String,
    pub key: String,
    pub asset_kind: Option<String>,
    pub asset_id: Option<String>,
    pub confidence: Option<String>,
    pub missing: bool,
    pub ambiguous: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectAssetIndex {
    pub assets: Vec<AssetRecord>,
    pub matches: Vec<TranslationAssetMatch>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntry {
    pub id: i64,
    pub mod_unit_id: i64,
    pub namespace: String,
    pub key: String,
    pub source_text: Option<String>,
    pub source_locale: Option<String>,
    pub target_text: Option<String>,
    pub target_original_text: Option<String>,
    pub translated_source_text: Option<String>,
    pub origin: TranslationOrigin,
    pub sort_index: usize,
    pub flags: Vec<EntryFlag>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentProjectEntry {
    pub id: i64,
    pub mod_unit_id: i64,
    pub namespace: String,
    pub key: String,
    pub entry_kind: TranslationEntryKind,
    /// 元文書のコンテナ内相対パス。
    pub document_path: String,
    /// Patchouli は RFC 6901 JSON Pointer、FTB Quests は lang キーと配列 index の識別文字列。
    pub document_pointer: String,
    pub source_text: Option<String>,
    pub source_locale: Option<String>,
    pub target_text: Option<String>,
    pub target_original_text: Option<String>,
    pub translated_source_text: Option<String>,
    pub origin: TranslationOrigin,
    pub sort_index: usize,
    pub flags: Vec<EntryFlag>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentEntryInput {
    pub namespace: String,
    pub key: String,
    pub entry_kind: TranslationEntryKind,
    /// 元文書のコンテナ内相対パス。
    pub document_path: String,
    /// Patchouli は RFC 6901 JSON Pointer、FTB Quests は lang キーと配列 index の識別文字列。
    pub document_pointer: String,
    pub source_text: Option<String>,
    pub source_locale: Option<String>,
    #[serde(default)]
    pub target_text: Option<String>,
    #[serde(default)]
    pub target_original_text: Option<String>,
    #[serde(default)]
    pub translated_source_text: Option<String>,
    #[serde(default = "default_missing_translation_origin")]
    pub origin: TranslationOrigin,
    pub sort_index: usize,
    #[serde(default)]
    pub flags: Vec<EntryFlag>,
}

fn default_missing_translation_origin() -> TranslationOrigin {
    TranslationOrigin::Missing
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UpdateOrigin {
    Ai,
    Manual,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryUpdate {
    pub id: i64,
    pub target_text: String,
    pub origin: UpdateOrigin,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReviewSettings {
    pub source_locale: Option<String>,
    pub fallback_priority: Vec<String>,
    pub preserve_existing_japanese: bool,
    pub fill_missing_from_other_languages: bool,
    pub setup_completed: bool,
    pub included_namespaces: Vec<IncludedNamespace>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub display_name: String,
    pub mod_count: usize,
    pub instance_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationProgress {
    pub translated: usize,
    pub missing: usize,
    pub needs_review: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentProject {
    pub id: String,
    pub display_name: String,
    pub mod_version: Option<String>,
    pub mod_count: usize,
    pub instance_path: Option<String>,
    pub last_opened: String,
    pub translation_progress: TranslationProgress,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentProjects {
    #[serde(default = "default_recent_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub projects: Vec<RecentProject>,
}

impl Default for RecentProjects {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_RECENT_SCHEMA_VERSION,
            projects: Vec::new(),
        }
    }
}

fn default_recent_schema_version() -> u32 {
    CURRENT_RECENT_SCHEMA_VERSION
}

impl RecentProjects {
    /// App Data外へ脱線しないUUIDだけを残し、削除済みProjectをRecent一覧から除く。
    pub fn retain_existing(&mut self, projects_root: &std::path::Path) -> usize {
        let previous_len = self.projects.len();
        self.projects.retain(|project| {
            uuid::Uuid::parse_str(&project.id)
                .ok()
                .is_some_and(|id| projects_root.join(id.to_string()).is_dir())
        });
        previous_len - self.projects.len()
    }
}

pub(crate) fn origin_to_db(origin: &TranslationOrigin) -> &'static str {
    match origin {
        TranslationOrigin::Missing => "missing",
        TranslationOrigin::Existing => "existing",
        TranslationOrigin::Ai => "ai",
        TranslationOrigin::Edited => "edited",
    }
}

pub(crate) fn origin_from_db(value: &str) -> crate::Result<TranslationOrigin> {
    match value {
        "missing" => Ok(TranslationOrigin::Missing),
        "existing" => Ok(TranslationOrigin::Existing),
        "ai" => Ok(TranslationOrigin::Ai),
        "edited" => Ok(TranslationOrigin::Edited),
        _ => Err(crate::ProjectError::InvalidDatabaseValue(format!(
            "target_origin={value}"
        ))),
    }
}

pub(crate) fn flag_to_db(flag: &EntryFlag) -> &'static str {
    match flag {
        EntryFlag::SourceFallback => "source_fallback",
        EntryFlag::NeedsReview => "needs_review",
        EntryFlag::PlaceholderError => "placeholder_error",
        EntryFlag::MalformedSource => "malformed_source",
        EntryFlag::AssetAmbiguous => "asset_ambiguous",
        EntryFlag::AssetMissing => "asset_missing",
        EntryFlag::PatchouliText => "patchouli_text",
    }
}

pub(crate) fn flag_from_db(value: &str) -> crate::Result<EntryFlag> {
    match value {
        "source_fallback" => Ok(EntryFlag::SourceFallback),
        "needs_review" => Ok(EntryFlag::NeedsReview),
        "placeholder_error" => Ok(EntryFlag::PlaceholderError),
        "malformed_source" => Ok(EntryFlag::MalformedSource),
        "asset_ambiguous" => Ok(EntryFlag::AssetAmbiguous),
        "asset_missing" => Ok(EntryFlag::AssetMissing),
        "patchouli_text" => Ok(EntryFlag::PatchouliText),
        _ => Err(crate::ProjectError::InvalidDatabaseValue(format!(
            "entry_flags.flag={value}"
        ))),
    }
}
