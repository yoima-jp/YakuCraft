use std::collections::{BTreeMap, BTreeSet, HashSet};

use indexmap::IndexMap;

use crate::discovery::{file_priority, normalize_locale};
use crate::{
    EntryFlag, ParsedLanguageFile, ParsedLanguageValue, ReviewSettings,
    SourceLanguageRecommendation, TranslationEntry, TranslationOrigin,
};

type EffectiveKey = (String, String, String);

#[derive(Clone)]
struct EffectiveValue {
    locale: String,
    value: ParsedLanguageValue,
}

/// Source locale候補と件数を返す。候補は提案用で、Review設定を自動変更しない。
pub fn source_language_recommendation(
    files: &[ParsedLanguageFile],
    included_namespaces: &[String],
) -> SourceLanguageRecommendation {
    let included: HashSet<&str> = included_namespaces.iter().map(String::as_str).collect();
    let (effective, locale_spellings) = build_effective_values(files, &included);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();

    for (_, locale, _) in effective.keys() {
        *counts.entry(locale.clone()).or_default() += 1;
    }

    let english = locale_spellings.get("en_us").cloned();
    let recommended_locale = english.or_else(|| {
        counts
            .iter()
            .max_by(|(left_locale, left_count), (right_locale, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| right_locale.cmp(left_locale))
            })
            .and_then(|(locale, _)| locale_spellings.get(locale).cloned())
    });

    let entry_counts = counts
        .into_iter()
        .map(|(locale, count)| {
            let display_locale = locale_spellings
                .get(&locale)
                .cloned()
                .unwrap_or_else(|| locale.clone());
            (display_locale, count)
        })
        .collect::<IndexMap<_, _>>();

    SourceLanguageRecommendation {
        recommended_locale,
        entry_counts,
    }
}

/// Reviewで明示選択されたsource localeを使い、namespaceごとにキーをUnionする。
pub fn merge_languages(
    files: &[ParsedLanguageFile],
    settings: &ReviewSettings,
) -> Vec<TranslationEntry> {
    let included: HashSet<&str> = settings
        .included_namespaces
        .iter()
        .map(String::as_str)
        .collect();
    let (effective, _) = build_effective_values(files, &included);
    let mut namespaces = BTreeSet::new();
    let mut namespace_keys: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut coverage: BTreeMap<(String, String), usize> = BTreeMap::new();

    for (namespace, locale, key) in effective.keys() {
        namespaces.insert(namespace.clone());
        namespace_keys
            .entry(namespace.clone())
            .or_default()
            .insert(key.clone());
        *coverage
            .entry((namespace.clone(), locale.clone()))
            .or_default() += 1;
    }

    let source_locale = normalize_locale(&settings.source_locale);
    let mut result = Vec::new();

    for namespace in namespaces {
        let fallback_order = fallback_locale_order(
            &effective,
            &coverage,
            &namespace,
            &source_locale,
            &settings.fallback_priority,
        );
        let mut ordered_keys = Vec::new();
        let mut seen = HashSet::new();

        // Sourceの順を保つ。Root JARの値は同名Nested namespaceより優先し、Mod本体の翻訳を保つ。
        let mut source_files: Vec<_> = files
            .iter()
            .filter(|file| {
                file.file.namespace == namespace
                    && file.file.normalized_locale == source_locale
                    && included.contains(file.file.namespace.as_str())
            })
            .collect();
        source_files
            .sort_by(|left, right| file_priority(&left.file).cmp(&file_priority(&right.file)));

        for file in source_files {
            for key in file.entries.keys() {
                if seen.insert(key.clone()) {
                    ordered_keys.push(key.clone());
                }
            }
        }

        if let Some(all_keys) = namespace_keys.get(&namespace) {
            ordered_keys.extend(all_keys.iter().filter(|key| !seen.contains(*key)).cloned());
        }

        for key in ordered_keys {
            let primary = effective.get(&(namespace.clone(), source_locale.clone(), key.clone()));
            let malformed_primary = primary.is_some_and(|value| value.value.malformed_source);

            let selected_primary = primary.filter(|value| value.value.text.is_some());
            let mut source_fallback = false;
            let selected_source = if let Some(primary) = selected_primary {
                Some(primary)
            } else if settings.fill_missing_from_other_languages {
                let mut fallback = None;
                for locale in &fallback_order {
                    if let Some(value) =
                        effective.get(&(namespace.clone(), locale.clone(), key.clone()))
                        && value.value.text.is_some()
                    {
                        fallback = Some(value);
                        break;
                    }
                }
                source_fallback = fallback.is_some();
                fallback
            } else {
                None
            };

            let source_text = selected_source.and_then(|source| source.value.text.clone());
            let selected_locale = selected_source
                .map(|source| source.locale.clone())
                .or_else(|| primary.map(|source| source.locale.clone()));
            let target = effective.get(&(namespace.clone(), "ja_jp".to_owned(), key.clone()));
            let preserve_target = settings.preserve_existing_japanese && target.is_some();
            let target_text = if preserve_target {
                target
                    .and_then(|value| value.value.text.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let target_original_text = preserve_target.then(|| target_text.clone());
            let mut flags = Vec::new();
            if source_fallback {
                flags.push(EntryFlag::SourceFallback);
            }
            if malformed_primary {
                flags.push(EntryFlag::MalformedSource);
            }

            let sort_index = result.len();
            result.push(TranslationEntry {
                namespace: namespace.clone(),
                key,
                source_text: source_text.clone(),
                source_locale: selected_locale,
                target_text,
                target_original_text,
                translated_source_text: preserve_target.then_some(source_text).flatten(),
                origin: if preserve_target {
                    TranslationOrigin::Existing
                } else {
                    TranslationOrigin::Missing
                },
                flags,
                sort_index,
            });
        }
    }

    result
}

/// Candidateとして表示するlocaleだけを返す便宜関数。
pub fn recommend_source_locale(
    files: &[ParsedLanguageFile],
    included_namespaces: &[String],
) -> Option<String> {
    source_language_recommendation(files, included_namespaces).recommended_locale
}

fn build_effective_values(
    files: &[ParsedLanguageFile],
    included: &HashSet<&str>,
) -> (
    BTreeMap<EffectiveKey, EffectiveValue>,
    BTreeMap<String, String>,
) {
    let mut ordered_files: Vec<_> = files
        .iter()
        .filter(|file| included.contains(file.file.namespace.as_str()))
        .collect();
    ordered_files.sort_by(|left, right| file_priority(&left.file).cmp(&file_priority(&right.file)));

    let mut effective = BTreeMap::new();
    let mut locale_spellings = BTreeMap::new();
    for file in ordered_files {
        locale_spellings
            .entry(file.file.normalized_locale.clone())
            .or_insert_with(|| file.file.locale.clone());

        for (key, value) in &file.entries {
            let effective_key = (
                file.file.namespace.clone(),
                file.file.normalized_locale.clone(),
                key.clone(),
            );
            // Rootを先に並べているため、同じnamespace・locale・keyではMod本体の値を残す。
            effective
                .entry(effective_key)
                .or_insert_with(|| EffectiveValue {
                    locale: file.file.locale.clone(),
                    value: value.clone(),
                });
        }
    }

    (effective, locale_spellings)
}

fn fallback_locale_order(
    effective: &BTreeMap<EffectiveKey, EffectiveValue>,
    coverage: &BTreeMap<(String, String), usize>,
    namespace: &str,
    source_locale: &str,
    explicit_priority: &[String],
) -> Vec<String> {
    let available: BTreeSet<_> = effective
        .keys()
        .map(|(_, locale, _)| locale.clone())
        .collect();
    let mut order = Vec::new();
    let mut seen = HashSet::new();

    for locale in explicit_priority {
        let normalized = normalize_locale(locale);
        if normalized != source_locale
            && normalized != "ja_jp"
            && available.contains(&normalized)
            && seen.insert(normalized.clone())
        {
            order.push(normalized);
        }
    }

    let mut remaining: Vec<_> = available
        .into_iter()
        .filter(|locale| locale != source_locale && locale != "ja_jp" && !seen.contains(locale))
        .collect();
    remaining.sort_by(|left, right| {
        coverage
            .get(&(namespace.to_owned(), right.clone()))
            .copied()
            .unwrap_or_default()
            .cmp(
                &coverage
                    .get(&(namespace.to_owned(), left.clone()))
                    .copied()
                    .unwrap_or_default(),
            )
            .then_with(|| left.cmp(right))
    });
    order.extend(remaining);
    order
}
