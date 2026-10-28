mod discovery;
mod error;
mod merge;
mod model;
mod parse_error;
mod parser;
pub use parse_error::parse_error_detail;
mod patchouli;
mod writer;

pub use discovery::{LanguageArchivePath, discover_language_files, normalize_locale};
pub use error::LanguageError;
pub use merge::{merge_languages, recommend_source_locale, source_language_recommendation};
pub use model::{
    DiscoveredLanguageFile, EntryFlag, LanguageFileFormat, LanguageParseWarning,
    ParsedLanguageContent, ParsedLanguageFile, ParsedLanguageValue, ReviewSettings,
    SourceLanguageRecommendation, TranslationEntry, TranslationOrigin, WriterOptions,
};
pub use parser::{
    java_properties_has_invalid_unicode, legacy_language_uses_parse_escapes, parse_java_properties,
    parse_json_language, parse_language_file, parse_legacy_language,
};
pub use patchouli::{
    PatchouliArchiveFile, PatchouliBook, PatchouliBookConfig, PatchouliDiscovery,
    PatchouliDocument, PatchouliDocumentKind, PatchouliEntry, PatchouliExtraction,
    PatchouliRebuild, PatchouliWarning, discover_books, extract_entries, extract_i18n_keys,
    patchouli_target_document_path, rebuild_document, rebuild_document_with_warnings,
};
pub use writer::{
    compare_minecraft_versions, is_empty_translation, language_export_filename,
    language_export_source_file, resource_pack_language_filename, write_json_language,
    write_language_file, write_legacy_language,
};

#[cfg(test)]
mod tests;
