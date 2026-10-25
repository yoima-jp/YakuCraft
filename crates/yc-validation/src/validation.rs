use std::collections::HashMap;

use serde::Serialize;

use crate::{
    PlaceholderComparison, PlaceholderDifference, PlaceholderProfile,
    compare_placeholders_for_profile,
};

/// 制御文字の Unicode スカラー位置とコードポイント。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidControlCharacter {
    pub char_index: usize,
    pub code_point: u32,
}

/// AI 翻訳を要求したエントリ。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestedEntry {
    pub id: i64,
    pub source: String,
}

/// 書式プロファイルを指定して AI 翻訳を検証するための入力。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfiledRequestedEntry {
    pub id: i64,
    pub source: String,
    pub format_profile: PlaceholderProfile,
}

/// AI が返した翻訳。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTranslation {
    pub id: i64,
    pub translation: String,
}

/// 検証を通過し、適用可能な翻訳。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedTranslation {
    pub id: i64,
    pub translation: String,
}

/// AI 翻訳の拒否理由。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "code",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ValidationIssue {
    MissingResponse,
    DuplicateId,
    UnknownId,
    EmptyTranslation,
    PlaceholderMismatch {
        difference: PlaceholderDifference,
    },
    InvalidControlCharacter {
        characters: Vec<InvalidControlCharacter>,
    },
}

/// 適用できなかった翻訳とその理由。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedTranslation {
    pub id: i64,
    pub translation: Option<String>,
    pub reasons: Vec<ValidationIssue>,
}

/// 1 バッチの部分検証結果。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchValidation {
    pub accepted: Vec<ValidatedTranslation>,
    pub rejected: Vec<RejectedTranslation>,
}

/// 手動編集時の Placeholder 警告。保存可否は決めない。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualEditValidation {
    pub placeholder_error: bool,
    pub difference: PlaceholderDifference,
}

/// 改行・タブを除く制御文字を Unicode スカラー位置付きで返す。
pub fn find_invalid_control_characters(text: &str) -> Vec<InvalidControlCharacter> {
    text.chars()
        .enumerate()
        .filter(|(_, character)| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        .map(|(char_index, character)| InvalidControlCharacter {
            char_index,
            code_point: character as u32,
        })
        .collect()
}

/// 要求・応答を個別に検証し、正常な応答だけを適用可能として返す。
pub fn validate_ai_batch(
    requested: &[RequestedEntry],
    responses: &[AiTranslation],
) -> BatchValidation {
    let profiled: Vec<_> = requested
        .iter()
        .map(|entry| ProfiledRequestedEntry {
            id: entry.id,
            source: entry.source.clone(),
            format_profile: PlaceholderProfile::Lang,
        })
        .collect();
    validate_ai_batch_with_profiles(&profiled, responses)
}

/// 各エントリの書式プロファイルに従って AI 翻訳を検証する。
pub fn validate_ai_batch_with_profiles(
    requested: &[ProfiledRequestedEntry],
    responses: &[AiTranslation],
) -> BatchValidation {
    let mut requested_counts = HashMap::new();
    let mut requested_sources = HashMap::new();
    for entry in requested {
        *requested_counts.entry(entry.id).or_insert(0usize) += 1;
        requested_sources
            .entry(entry.id)
            .or_insert((entry.source.as_str(), entry.format_profile));
    }

    let mut response_counts = HashMap::new();
    for response in responses {
        *response_counts.entry(response.id).or_insert(0usize) += 1;
    }

    let mut result = BatchValidation::default();
    for response in responses {
        let requested_count = requested_counts.get(&response.id).copied().unwrap_or(0);
        let response_count = response_counts[&response.id];
        let mut reasons = Vec::new();

        if requested_count == 0 {
            reasons.push(ValidationIssue::UnknownId);
        }
        if requested_count > 1 || response_count > 1 {
            reasons.push(ValidationIssue::DuplicateId);
        }
        if response.translation.trim().is_empty() {
            reasons.push(ValidationIssue::EmptyTranslation);
        }

        let invalid_characters = find_invalid_control_characters(&response.translation);
        if !invalid_characters.is_empty() {
            reasons.push(ValidationIssue::InvalidControlCharacter {
                characters: invalid_characters,
            });
        }

        if requested_count == 1
            && let Some((source, format_profile)) = requested_sources.get(&response.id)
        {
            let comparison =
                compare_placeholders_for_profile(source, &response.translation, *format_profile);
            push_placeholder_issue(&mut reasons, comparison);
        }

        if reasons.is_empty() {
            result.accepted.push(ValidatedTranslation {
                id: response.id,
                translation: response.translation.clone(),
            });
        } else {
            result.rejected.push(RejectedTranslation {
                id: response.id,
                translation: Some(response.translation.clone()),
                reasons,
            });
        }
    }

    for entry in requested {
        if response_counts.get(&entry.id).copied().unwrap_or(0) != 0 {
            continue;
        }

        let mut reasons = vec![ValidationIssue::MissingResponse];
        if requested_counts[&entry.id] > 1 {
            reasons.push(ValidationIssue::DuplicateId);
        }
        result.rejected.push(RejectedTranslation {
            id: entry.id,
            translation: None,
            reasons,
        });
    }

    result
}

/// 手動編集の Placeholder 不一致を警告として返す。
pub fn validate_manual_edit(source: &str, translation: &str) -> ManualEditValidation {
    validate_manual_edit_with_profile(source, translation, PlaceholderProfile::Lang)
}

/// 指定した書式プロファイルで手動編集の Placeholder 不一致を警告として返す。
pub fn validate_manual_edit_with_profile(
    source: &str,
    translation: &str,
    format_profile: PlaceholderProfile,
) -> ManualEditValidation {
    let PlaceholderComparison {
        is_valid,
        difference,
        ..
    } = compare_placeholders_for_profile(source, translation, format_profile);
    ManualEditValidation {
        placeholder_error: !is_valid,
        difference,
    }
}

fn push_placeholder_issue(reasons: &mut Vec<ValidationIssue>, comparison: PlaceholderComparison) {
    if !comparison.is_valid {
        reasons.push(ValidationIssue::PlaceholderMismatch {
            difference: comparison.difference,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_issue(
        entry: &RejectedTranslation,
        predicate: impl Fn(&ValidationIssue) -> bool,
    ) -> bool {
        entry.reasons.iter().any(predicate)
    }

    #[test]
    fn detects_only_control_characters_other_than_newlines_and_tabs() {
        let invalid = find_invalid_control_characters("a\n\t\r\u{0001}\u{007f}b");
        assert_eq!(
            invalid,
            [
                InvalidControlCharacter {
                    char_index: 4,
                    code_point: 1,
                },
                InvalidControlCharacter {
                    char_index: 5,
                    code_point: 127,
                },
            ]
        );
    }

    #[test]
    fn validates_manual_edits_as_warnings() {
        let valid = validate_manual_edit("Energy %s", "エネルギー %s");
        assert!(!valid.placeholder_error);
        assert!(valid.difference.missing.is_empty());

        let invalid = validate_manual_edit("Energy %s", "エネルギー");
        assert!(invalid.placeholder_error);
        assert_eq!(invalid.difference.missing[0].value, "%s");
    }

    #[test]
    fn printf_width_and_flag_rules_apply_to_manual_and_ai_translations() {
        for (source, target, valid) in [
            ("%s", "%-s", false),
            ("%d", "%0d", false),
            ("%d", "%-05d", false),
            ("%d", "%+ 5d", false),
            ("%s", "%--5s", false),
            ("%tY", "%-tY", false),
            ("%s", "%-5s", true),
            ("%d", "%05d", true),
            ("%d", "%-5d", true),
            ("%d", "%+05d", true),
            ("%tY", "%-5tY", true),
        ] {
            let manual = validate_manual_edit(source, target);
            assert_eq!(manual.placeholder_error, !valid, "manual: {target}");
            let batch = validate_ai_batch(
                &[RequestedEntry {
                    id: 1,
                    source: source.into(),
                }],
                &[AiTranslation {
                    id: 1,
                    translation: target.into(),
                }],
            );
            assert_eq!(batch.accepted.len(), usize::from(valid), "AI: {target}");
            assert_eq!(batch.rejected.len(), usize::from(!valid), "AI: {target}");
            if !valid {
                assert!(has_issue(&batch.rejected[0], |issue| matches!(
                    issue,
                    ValidationIssue::PlaceholderMismatch { .. }
                )));
            }
        }
    }

    #[test]
    fn rejects_ai_translation_that_adds_uppercase_printf_argument() {
        let result = validate_ai_batch(
            &[RequestedEntry {
                id: 1,
                source: "Value %s".into(),
            }],
            &[AiTranslation {
                id: 1,
                translation: "%1$s %2$S".into(),
            }],
        );

        assert!(result.accepted.is_empty());
        assert_eq!(result.rejected.len(), 1);
        assert!(has_issue(&result.rejected[0], |issue| matches!(
            issue,
            ValidationIssue::PlaceholderMismatch { .. }
        )));
    }

    #[test]
    fn accepts_a_complete_valid_ai_batch() {
        let result = validate_ai_batch(
            &[
                RequestedEntry {
                    id: 1,
                    source: "Hello %s".into(),
                },
                RequestedEntry {
                    id: 2,
                    source: "Number {0}".into(),
                },
            ],
            &[
                AiTranslation {
                    id: 1,
                    translation: "こんにちは %s".into(),
                },
                AiTranslation {
                    id: 2,
                    translation: "番号 {0}".into(),
                },
            ],
        );

        assert_eq!(result.accepted.len(), 2);
        assert!(result.rejected.is_empty());
    }

    #[test]
    fn validates_ai_batch_entries_independently() {
        let requested = [
            RequestedEntry {
                id: 1,
                source: "Hello %s".into(),
            },
            RequestedEntry {
                id: 2,
                source: "Keep".into(),
            },
            RequestedEntry {
                id: 3,
                source: "Number {0}".into(),
            },
            RequestedEntry {
                id: 4,
                source: "%d".into(),
            },
            RequestedEntry {
                id: 5,
                source: "%s".into(),
            },
            RequestedEntry {
                id: 6,
                source: "Not returned".into(),
            },
            RequestedEntry {
                id: 7,
                source: "Control".into(),
            },
        ];
        let responses = [
            AiTranslation {
                id: 1,
                translation: "こんにちは %s".into(),
            },
            AiTranslation {
                id: 2,
                translation: "そのまま".into(),
            },
            AiTranslation {
                id: 3,
                translation: "番号 {0}".into(),
            },
            AiTranslation {
                id: 4,
                translation: "%d".into(),
            },
            AiTranslation {
                id: 4,
                translation: "%d".into(),
            },
            AiTranslation {
                id: 5,
                translation: String::new(),
            },
            AiTranslation {
                id: 7,
                translation: "不正\u{0001}".into(),
            },
            AiTranslation {
                id: 99,
                translation: "unknown".into(),
            },
        ];

        let result = validate_ai_batch(&requested, &responses);
        assert_eq!(result.accepted.len(), 3);
        assert_eq!(
            result
                .accepted
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(result.rejected.len(), 6);

        assert_eq!(
            result.rejected.iter().filter(|entry| entry.id == 4).count(),
            2
        );
        assert!(
            result
                .rejected
                .iter()
                .filter(|entry| entry.id == 4)
                .all(|entry| {
                    has_issue(entry, |issue| matches!(issue, ValidationIssue::DuplicateId))
                })
        );

        let empty = result.rejected.iter().find(|entry| entry.id == 5).unwrap();
        assert!(has_issue(empty, |issue| matches!(
            issue,
            ValidationIssue::EmptyTranslation
        )));
        assert!(has_issue(empty, |issue| matches!(
            issue,
            ValidationIssue::PlaceholderMismatch { .. }
        )));

        let missing = result.rejected.iter().find(|entry| entry.id == 6).unwrap();
        assert!(missing.translation.is_none());
        assert!(has_issue(missing, |issue| matches!(
            issue,
            ValidationIssue::MissingResponse
        )));

        let control = result.rejected.iter().find(|entry| entry.id == 7).unwrap();
        assert!(has_issue(control, |issue| matches!(
            issue,
            ValidationIssue::InvalidControlCharacter { .. }
        )));

        let unknown = result.rejected.iter().find(|entry| entry.id == 99).unwrap();
        assert!(has_issue(unknown, |issue| matches!(
            issue,
            ValidationIssue::UnknownId
        )));
    }

    #[test]
    fn rejects_an_unknown_id_even_when_it_is_the_only_response() {
        let result = validate_ai_batch(
            &[RequestedEntry {
                id: 1,
                source: "source".into(),
            }],
            &[AiTranslation {
                id: 2,
                translation: "translated".into(),
            }],
        );
        assert!(result.accepted.is_empty());
        assert_eq!(result.rejected.len(), 2);
        assert!(
            result
                .rejected
                .iter()
                .any(|entry| entry.id == 1 && entry.translation.is_none())
        );
        assert!(result.rejected.iter().any(|entry| {
            entry.id == 2 && has_issue(entry, |issue| matches!(issue, ValidationIssue::UnknownId))
        }));
    }

    #[test]
    fn validates_ai_entries_with_their_format_profiles() {
        let result = validate_ai_batch_with_profiles(
            &[ProfiledRequestedEntry {
                id: 1,
                source: "$(br) Stone".into(),
                format_profile: PlaceholderProfile::Patchouli,
            }],
            &[AiTranslation {
                id: 1,
                translation: "石".into(),
            }],
        );
        assert!(result.accepted.is_empty());
        assert!(has_issue(&result.rejected[0], |issue| matches!(
            issue,
            ValidationIssue::PlaceholderMismatch { .. }
        )));

        let lang_default = validate_ai_batch(
            &[RequestedEntry {
                id: 1,
                source: "$(br) Stone".into(),
            }],
            &[AiTranslation {
                id: 1,
                translation: "石".into(),
            }],
        );
        assert_eq!(lang_default.accepted.len(), 1);
    }
}
