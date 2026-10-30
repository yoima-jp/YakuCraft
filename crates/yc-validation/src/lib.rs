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
    let introduces_leading_whitespace = [' ', '\t', '\u{000c}']
        .into_iter()
        .any(|character| translation.starts_with(character) && !source.starts_with(character));

    counts(source) != counts(translation)
        || (translation.contains('\n') && !source.contains('\n'))
        || (translation.contains('\r') && !source.contains('\r'))
        || (translation.contains('\\') && !source.contains('\\'))
        || introduces_leading_whitespace
}

#[cfg(test)]
mod shader_format_tests {
    use super::shader_format_warning;

    #[test]
    fn warns_when_translation_adds_optifine_sensitive_text() {
        for translation in ["改行\nあり", "slash\\text", " 先頭", "\t先頭"] {
            assert!(shader_format_warning("原文", translation));
        }
        assert!(shader_format_warning("改行\n原文", "改行\r訳文"));
        assert!(shader_format_warning("\t先頭", " 先頭"));
    }

    #[test]
    fn does_not_warn_for_optifine_sensitive_text_already_in_source() {
        assert!(!shader_format_warning("改行\n原文", "改行\n訳文"));
        assert!(!shader_format_warning("slash\\source", "slash\\訳"));
        assert!(!shader_format_warning(" 先頭", " 先頭訳"));
    }

    #[test]
    fn still_warns_when_shader_format_codes_change() {
        assert!(shader_format_warning("色 §a", "色 §b"));
        assert!(!shader_format_warning("色 §a", "色 §a"));
    }
}
