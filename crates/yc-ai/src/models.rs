use serde::Serialize;
use yc_validation::PlaceholderToken;

/// 1件の翻訳入力。placeholder は原文から生成し、指示文にも明示する。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationEntry {
    pub id: i64,
    pub key: String,
    pub source: String,
    pub source_locale: String,
    pub target_locale: String,
    pub placeholders: Vec<PlaceholderToken>,
}

impl TranslationEntry {
    pub fn new(
        id: i64,
        key: impl Into<String>,
        source: impl Into<String>,
        source_locale: impl Into<String>,
        target_locale: impl Into<String>,
    ) -> Self {
        let source = source.into();
        Self {
            id,
            key: key.into(),
            placeholders: yc_validation::extract_placeholders(&source),
            source,
            source_locale: source_locale.into(),
            target_locale: target_locale.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranslationContext {
    pub mod_name: String,
    pub namespace: String,
    pub custom_prompt: Option<String>,
    pub glossary_terms: Vec<GlossaryTerm>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlossaryTerm {
    pub source_term: String,
    pub target_term: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranslationProgress {
    pub completed: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationOutcome {
    pub accepted: Vec<yc_validation::ValidatedTranslation>,
    pub rejected: Vec<yc_validation::RejectedTranslation>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PromptEntry<'a> {
    pub id: i64,
    pub key: &'a str,
    pub source: &'a str,
    pub source_locale: &'a str,
    pub target_locale: &'a str,
    pub placeholders: Vec<String>,
}

pub(crate) fn parse_translation_payload(
    payload: &str,
) -> crate::Result<Vec<yc_validation::AiTranslation>> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|_| crate::AiError::InvalidProviderResponse)?;
    let translations = value
        .as_array()
        .or_else(|| {
            value
                .get("translations")
                .and_then(serde_json::Value::as_array)
        })
        .ok_or(crate::AiError::InvalidProviderResponse)?;

    // 1行の型崩れで有効な他の翻訳まで捨てない。ID が読める行は空文字へ寄せて
    // yc-validation に渡し、空訳として個別に reject してもらう。
    Ok(translations
        .iter()
        .filter_map(|translation| {
            let id = translation.get("id")?.as_i64()?;
            let translation = translation
                .get("translation")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Some(yc_validation::AiTranslation { id, translation })
        })
        .collect())
}
