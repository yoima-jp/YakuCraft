mod placeholders;
mod validation;

pub use placeholders::{
    PlaceholderComparison, PlaceholderDifference, PlaceholderKind, PlaceholderProfile,
    PlaceholderSeverity, PlaceholderToken, compare_placeholders, compare_placeholders_for_profile,
    extract_placeholders, extract_placeholders_for_profile,
};
pub use validation::{
    AiTranslation, BatchValidation, InvalidControlCharacter, ManualEditValidation,
    ProfiledRequestedEntry, RejectedTranslation, RequestedEntry, ValidatedTranslation,
    ValidationIssue, find_invalid_control_characters, validate_ai_batch,
    validate_ai_batch_with_profiles, validate_manual_edit, validate_manual_edit_with_profile,
};

/// シェーダーの書式コードの出現回数が異なる場合に警告を返す。
pub fn shader_format_warning(source: &str, translation: &str) -> bool {
    fn counts(text: &str) -> std::collections::BTreeMap<char, usize> {
        let mut counts = std::collections::BTreeMap::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '§'
                && let Some(&code) = chars.peek()
            {
                *counts.entry(code).or_default() += 1;
            }
        }
        counts
    }
    counts(source) != counts(translation)
}
