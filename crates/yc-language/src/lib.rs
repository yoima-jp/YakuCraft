mod discovery;
mod error;
mod merge;
mod model;
mod parser;
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
    legacy_language_uses_parse_escapes, parse_json_language, parse_language_file,
    parse_legacy_language,
};
pub use writer::{
    compare_minecraft_versions, language_export_filename, language_export_source_file,
    resource_pack_language_filename, write_json_language, write_language_file,
    write_legacy_language,
};

#[cfg(test)]
mod tests;
