use serde::{Deserialize, Serialize};

use crate::{kubejs::KubeJsDocument, snbt::SnbtDocument};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FtbQuestsStatus {
    None,
    Single,
    Split,
    LegacyInline,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModpackFileKind {
    FtbQuestsLanguage,
    KubeJsLanguage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackFile {
    pub path: String,
    pub kind: ModpackFileKind,
    pub locale: String,
    pub namespace: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackEntry {
    /// Stable source identifier, including the source document and language key.
    pub key: String,
    /// Instance-relative path to the English source document.
    pub document_path: String,
    /// SNBT lang key with `[index]` for array entries, or a KubeJS JSON key.
    pub pointer: String,
    pub source_text: String,
    pub existing_translation: Option<String>,
    /// Empty lines and standalone FTB display directives are retained but not translated.
    pub translatable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackScan {
    pub game_directory: String,
    pub ftb_quests: FtbQuestsStatus,
    pub files: Vec<ModpackFile>,
    pub kubejs_namespaces: Vec<String>,
    pub total_entries: usize,
    pub ftb_quest_entries: usize,
    pub kubejs_entries: usize,
    pub issues: Vec<String>,
    #[serde(skip)]
    pub safe_issues: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputFile {
    /// Instance-relative destination path.
    pub path: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ModpackData {
    pub scan: ModpackScan,
    pub entries: Vec<ModpackEntry>,
    pub(crate) ftb_documents: Vec<FtbDocument>,
    pub(crate) kubejs_documents: Vec<KubeJsOutputDocument>,
}

#[derive(Clone, Debug)]
pub(crate) struct FtbDocument {
    pub source_path: String,
    pub output_path: String,
    pub key_path: String,
    pub source: SnbtDocument,
    pub existing_translation: Option<SnbtDocument>,
}

#[derive(Clone, Debug)]
pub(crate) struct KubeJsOutputDocument {
    pub source_path: String,
    pub output_path: String,
    pub namespace: String,
    pub source: KubeJsDocument,
    pub existing_translation: Option<KubeJsDocument>,
}

impl ModpackData {
    pub(crate) fn new(
        scan: ModpackScan,
        entries: Vec<ModpackEntry>,
        ftb_documents: Vec<FtbDocument>,
        kubejs_documents: Vec<KubeJsOutputDocument>,
    ) -> Self {
        Self {
            scan,
            entries,
            ftb_documents,
            kubejs_documents,
        }
    }
}
