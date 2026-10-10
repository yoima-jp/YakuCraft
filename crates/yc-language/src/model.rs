use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageFileFormat {
    Json,
    LegacyLang,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredLanguageFile {
    pub namespace: String,
    pub locale: String,
    pub normalized_locale: String,
    pub format: LanguageFileFormat,
    pub container_path: String,
    pub archive_path: String,
    pub file_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedLanguageValue {
    pub text: Option<String>,
    pub malformed_source: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LanguageParseWarning {
    DuplicateKey { key: String },
    EncodingFallback { encoding: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedLanguageContent {
    pub entries: IndexMap<String, ParsedLanguageValue>,
    pub warnings: Vec<LanguageParseWarning>,
    /// Forge 1.12.xでは、このマーカーがある場合だけJava Properties形式で解析する。
    #[serde(default)]
    pub legacy_parse_escapes: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedLanguageFile {
    pub file: DiscoveredLanguageFile,
    pub entries: IndexMap<String, ParsedLanguageValue>,
    pub warnings: Vec<LanguageParseWarning>,
    #[serde(default)]
    pub legacy_parse_escapes: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ReviewSettings {
    pub source_locale: String,
    pub fallback_priority: Vec<String>,
    pub preserve_existing_japanese: bool,
    pub fill_missing_from_other_languages: bool,
    pub included_namespaces: Vec<String>,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            source_locale: String::new(),
            fallback_priority: Vec::new(),
            preserve_existing_japanese: true,
            fill_missing_from_other_languages: true,
            included_namespaces: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLanguageRecommendation {
    pub recommended_locale: Option<String>,
    pub entry_counts: IndexMap<String, usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationOrigin {
    Missing,
    Existing,
    Ai,
    Edited,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryFlag {
    SourceFallback,
    NeedsReview,
    PlaceholderError,
    MalformedSource,
    AssetAmbiguous,
    AssetMissing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationEntry {
    pub namespace: String,
    pub key: String,
    pub source_text: Option<String>,
    pub source_locale: Option<String>,
    pub target_text: String,
    pub target_original_text: Option<String>,
    pub translated_source_text: Option<String>,
    pub origin: TranslationOrigin,
    pub flags: Vec<EntryFlag>,
    pub sort_index: usize,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterOptions {
    pub include_empty_translations: bool,
    /// 原文の`.lang`がForge拡張形式なら、書き出しでもその形式を維持する。
    pub legacy_parse_escapes: bool,
}
