use std::collections::HashSet;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder};

use crate::{GlossaryTerm, GlossaryTermInput, ProjectError, Result};

struct Pattern {
    term_index: usize,
    text: String,
}

/// Builds one multi-pattern index per glossary, so matching cost follows the input text length.
pub struct GlossaryMatcher {
    terms: Vec<GlossaryTerm>,
    patterns: Vec<Pattern>,
    matcher: Option<AhoCorasick>,
}

impl GlossaryMatcher {
    pub fn new(terms: &[GlossaryTerm]) -> Result<Self> {
        let mut patterns = Vec::with_capacity(terms.len() * 2);
        for (term_index, term) in terms.iter().enumerate() {
            let source = term.source_term.trim();
            if source.is_empty() {
                continue;
            }
            patterns.push(Pattern {
                term_index,
                text: source.to_owned(),
            });
            let plural = pluralize(source);
            if plural != source {
                patterns.push(Pattern {
                    term_index,
                    text: plural,
                });
            }
        }
        let matcher = if patterns.is_empty() {
            None
        } else {
            let texts = patterns.iter().map(|pattern| pattern.text.as_str());
            Some(
                AhoCorasickBuilder::new()
                    .ascii_case_insensitive(true)
                    .build(texts)
                    .map_err(|error| ProjectError::InvalidDatabaseValue(error.to_string()))?,
            )
        };
        Ok(Self {
            terms: terms.to_vec(),
            patterns,
            matcher,
        })
    }

    /// Returns matched terms once each; overlaps keep the longest match at the earliest position.
    pub fn matching_terms(&self, source: &str) -> Vec<GlossaryTerm> {
        let Some(matcher) = &self.matcher else {
            return Vec::new();
        };
        let mut matches = matcher
            .find_overlapping_iter(source)
            .filter_map(|found| {
                let pattern = self.patterns.get(found.pattern().as_usize())?;
                if !is_word_boundary(source, found.start(), true)
                    || !is_word_boundary(source, found.end(), false)
                {
                    return None;
                }
                let term = self.terms.get(pattern.term_index)?;
                if term.case_sensitive && source.get(found.start()..found.end())? != pattern.text {
                    return None;
                }
                Some((found.start(), found.end(), pattern.term_index))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.cmp(&right.2))
        });

        let mut result = Vec::new();
        let mut seen_terms = HashSet::new();
        let mut selected_end = 0;
        for (start, end, term_index) in matches {
            if start < selected_end {
                continue;
            }
            if let Some(term) = self.terms.get(term_index) {
                if seen_terms.insert(term_index) {
                    result.push(term.clone());
                }
                selected_end = end;
            }
        }
        result
    }
}

fn is_word_boundary(source: &str, offset: usize, before: bool) -> bool {
    let adjacent = if before {
        source
            .get(..offset)
            .and_then(|prefix| prefix.chars().next_back())
    } else {
        source
            .get(offset..)
            .and_then(|suffix| suffix.chars().next())
    };
    adjacent.is_none_or(|character| !character.is_alphanumeric() && character != '_')
}

fn pluralize(source: &str) -> String {
    // 空白の直後へ進める際もUTF-8境界を守る。全角空白やNBSPは1バイトではない。
    let split_at = source
        .char_indices()
        .rfind(|(_, character)| character.is_whitespace())
        .map_or(0, |(index, character)| index + character.len_utf8());
    let (prefix, word) = source.split_at(split_at);
    let lowercase = word.to_ascii_lowercase();
    let plural = if lowercase.ends_with('y')
        && word
            .as_bytes()
            .get(word.len().saturating_sub(2))
            .is_some_and(|character| !b"aeiouAEIOU".contains(character))
    {
        format!("{}ies", &word[..word.len() - 1])
    } else if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|suffix| lowercase.ends_with(suffix))
    {
        format!("{word}es")
    } else {
        format!("{word}s")
    };
    format!("{prefix}{plural}")
}

pub fn parse_glossary_import(contents: &str) -> Result<Vec<GlossaryTermInput>> {
    let first_line = contents.lines().next().unwrap_or_default();
    // TSVのメモには未引用のカンマが入るため、区切りの個数では判別しない。
    // CSVの引用済みフィールド内のタブは区切りとして扱わない。
    let mut quoted = false;
    let has_tab_delimiter = first_line.chars().any(|character| {
        if character == '"' {
            quoted = !quoted;
        }
        character == '\t' && !quoted
    });
    let delimiter = if has_tab_delimiter { b'\t' } else { b',' };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(contents.as_bytes());
    let mut rows = reader
        .records()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.first().is_some_and(is_header) {
        rows.remove(0);
    }
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            if !(2..=4).contains(&row.len()) {
                return Err(ProjectError::InvalidDatabaseValue(format!(
                    "glossary row {} must have two to four columns",
                    index + 1
                )));
            }
            let source_term = row.get(0).unwrap_or_default().trim().to_owned();
            let target_term = row.get(1).unwrap_or_default().trim().to_owned();
            if source_term.is_empty() || target_term.is_empty() {
                return Err(ProjectError::InvalidDatabaseValue(format!(
                    "glossary row {} has an empty term",
                    index + 1
                )));
            }
            let note = row
                .get(2)
                .map(str::trim)
                .filter(|note| !note.is_empty())
                .map(str::to_owned);
            // 既存の2・3列CSV/TSVも取り込めるよう、大小区別は末尾の省略可能な列に置く。
            // 値が不正な場合は無効扱いにせず、用語の適用範囲を意図せず変えないよう取込を止める。
            let case_sensitive = match row.get(3).map(str::trim) {
                None | Some("" | "false") => false,
                Some("true") => true,
                Some(_) => {
                    return Err(ProjectError::InvalidDatabaseValue(format!(
                        "glossary row {} case sensitivity must be true or false",
                        index + 1
                    )));
                }
            };
            Ok(GlossaryTermInput {
                source_term,
                target_term,
                note,
                case_sensitive,
            })
        })
        .collect()
}

fn is_header(row: &csv::StringRecord) -> bool {
    let source = row
        .get(0)
        .unwrap_or_default()
        .trim()
        .trim_start_matches('\u{feff}');
    let target = row.get(1).unwrap_or_default().trim();
    matches!(source, "原文" | "source" | "source_term")
        && matches!(target, "訳語" | "target" | "target_term")
}

pub fn format_glossary_export(terms: &[GlossaryTerm]) -> Result<String> {
    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    writer.write_record(["原文", "訳語", "メモ", "大小区別"])?;
    for term in terms {
        writer.write_record([
            term.source_term.as_str(),
            term.target_term.as_str(),
            term.note.as_deref().unwrap_or_default(),
            if term.case_sensitive { "true" } else { "false" },
        ])?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| ProjectError::InvalidDatabaseValue(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| ProjectError::InvalidDatabaseValue(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(source_term: &str, target_term: &str, case_sensitive: bool) -> GlossaryTerm {
        GlossaryTerm {
            id: 1,
            source_term: source_term.to_owned(),
            target_term: target_term.to_owned(),
            note: None,
            case_sensitive,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn matcher_observes_boundaries_simple_plurals_longest_terms_and_case() {
        let terms = [
            term("Ingot", "インゴット", false),
            term("Iron Ingot", "鉄インゴット", false),
            term("Berry", "ベリー", false),
            term("Box", "箱", false),
            term("Netherite", "ネザライト", true),
        ];
        let matcher = GlossaryMatcher::new(&terms).unwrap();

        let matched = matcher.matching_terms(
            "Iron INGOTS, Ingots and Berries and Boxes; XIngot, Netherite, netherite",
        );
        let sources = matched
            .iter()
            .map(|term| term.source_term.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            sources,
            ["Iron Ingot", "Ingot", "Berry", "Box", "Netherite"]
        );
    }

    #[test]
    fn matcher_pluralizes_terms_with_multibyte_whitespace() {
        for separator in ['\u{3000}', '\u{00a0}'] {
            let source = format!("Iron{separator}Ingot");
            let terms = [term(&source, "鉄インゴット", false)];
            let matcher = GlossaryMatcher::new(&terms).unwrap();
            assert_eq!(matcher.matching_terms(&format!("{source}s")), terms);
        }
    }

    #[test]
    fn headerless_tsv_preserves_commas_in_notes_and_csv_preserves_quoted_tabs() {
        let imported = parse_glossary_import("Mana\tマナ\tmagic,power,energy\n").unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].source_term, "Mana");
        assert_eq!(imported[0].target_term, "マナ");
        assert_eq!(imported[0].note.as_deref(), Some("magic,power,energy"));

        let csv =
            parse_glossary_import("\"Iron\tIngot\",鉄インゴット,\"metal,material\"\n").unwrap();
        assert_eq!(csv[0].source_term, "Iron\tIngot");
        assert_eq!(csv[0].target_term, "鉄インゴット");
        assert_eq!(csv[0].note.as_deref(), Some("metal,material"));
    }

    #[test]
    fn csv_import_and_export_round_trip_quoted_values_and_tsv() {
        let imported =
            parse_glossary_import("原文,訳語,メモ\r\nIron Ingot,鉄インゴット,\"鉱石,素材\"\r\n")
                .unwrap();
        assert_eq!(imported[0].source_term, "Iron Ingot");
        assert_eq!(imported[0].note.as_deref(), Some("鉱石,素材"));

        let exported = format_glossary_export(&[GlossaryTerm {
            id: 7,
            source_term: imported[0].source_term.clone(),
            target_term: imported[0].target_term.clone(),
            note: imported[0].note.clone(),
            case_sensitive: false,
            created_at: String::new(),
            updated_at: String::new(),
        }])
        .unwrap();
        let round_trip = parse_glossary_import(&exported).unwrap();
        assert_eq!(round_trip, imported);

        let tsv = parse_glossary_import("原文\t訳語\tメモ\nMana\tマナ\t魔力\n").unwrap();
        assert_eq!(tsv[0].target_term, "マナ");
    }

    #[test]
    fn glossary_round_trip_preserves_case_sensitive_terms_and_matching() {
        let terms = [
            term("US", "米国", true),
            term("us", "私たち", true),
            term("Mana", "マナ", false),
        ];
        let imported = parse_glossary_import(&format_glossary_export(&terms).unwrap()).unwrap();
        let restored: Vec<_> = imported
            .iter()
            .map(|input| {
                let mut restored =
                    term(&input.source_term, &input.target_term, input.case_sensitive);
                restored.note = input.note.clone();
                restored
            })
            .collect();
        assert_eq!(restored, terms);
        let matcher = GlossaryMatcher::new(&restored).unwrap();
        let targets: Vec<_> = matcher
            .matching_terms("US and us use MANA")
            .into_iter()
            .map(|term| term.target_term)
            .collect();
        assert_eq!(targets, ["米国", "私たち", "マナ"]);
    }

    #[test]
    fn glossary_import_accepts_legacy_rows_and_case_sensitive_tsv() {
        let legacy = parse_glossary_import("Mana,マナ\n").unwrap();
        assert!(!legacy[0].case_sensitive);
        let tsv = parse_glossary_import("原文\t訳語\tメモ\t大小区別\nUS\t米国\t\ttrue\n").unwrap();
        assert!(tsv[0].case_sensitive);
        assert!(parse_glossary_import("US,米国,,yes\n").is_err());
    }
}
