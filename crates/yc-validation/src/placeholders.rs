use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

static PRINTF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"%(?:(?P<escaped>(?:-?\d+)?%)|(?:(?P<index>[1-9]\d*)\$)?(?P<flags>[-#+ 0,(<]*)(?P<width>\d+)?(?:\.(?P<precision>\d+))?(?P<conversion>(?:[tT][HIklMSLNpzZsQBbhAaCYyjmedRTrDFc]|[bBhHsScCdoxXeEfgGaA]|n)))",
    )
    .expect("printf placeholder regex is a compile-time constant")
});

static PRINTF_CANDIDATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"%(?:\d+\$)?[-#+ 0,(<]*\d*(?:\.\d+)?[A-Za-z%]")
        .expect("printf-candidate regex is a compile-time constant")
});

static NATURAL_PERCENT_OF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d+%\s+of\b").expect("natural percentage regex is a compile-time constant")
});

static NUMBERED_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\d+\}").expect("numbered placeholder regex is a compile-time constant")
});

static NAMED_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{[A-Za-z_][A-Za-z0-9_]*\}")
        .expect("named placeholder regex is a compile-time constant")
});

static ESCAPE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\[nt\\]|\r\n|\r|\n|\t").expect("escape regex is a compile-time constant")
});

static MINECRAFT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:§[0-9a-fk-or])")
        .expect("Minecraft formatting regex is a compile-time constant")
});

/// 検出したトークンの分類。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaceholderKind {
    Printf,
    EscapedPercent,
    Numbered,
    Named,
    Escape,
    MinecraftFormatting,
}

/// 原文または訳文に含まれる Placeholder。
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceholderToken {
    pub kind: PlaceholderKind,
    pub value: String,
}

/// 原文と訳文で一致しなかったトークン。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceholderDifference {
    pub missing: Vec<PlaceholderToken>,
    pub extra: Vec<PlaceholderToken>,
}

/// Placeholder 比較結果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceholderComparison {
    pub is_valid: bool,
    pub severity: Option<PlaceholderSeverity>,
    pub difference: PlaceholderDifference,
}

/// 不一致が引き起こす可能性のある問題。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaceholderSeverity {
    Warning,
    Error,
}

/// Placeholder トークンを出現順に抽出する。
pub fn extract_placeholders(text: &str) -> Vec<PlaceholderToken> {
    let mut found = Vec::new();

    for captures in PRINTF_PATTERN.captures_iter(text) {
        let matched = captures
            .get(0)
            .expect("whole regex match is always present");
        let kind = if captures.name("escaped").is_some() {
            PlaceholderKind::EscapedPercent
        } else {
            if is_valid_printf_specifier(&captures)
                && !is_ambiguous_uppercase_word_start(text, &captures)
            {
                PlaceholderKind::Printf
            } else {
                continue;
            }
        };
        found.push((
            matched.start(),
            PlaceholderToken {
                kind,
                value: matched.as_str().to_owned(),
            },
        ));
    }

    for matched in NUMBERED_PATTERN.find_iter(text) {
        found.push((
            matched.start(),
            PlaceholderToken {
                kind: PlaceholderKind::Numbered,
                value: matched.as_str().to_owned(),
            },
        ));
    }

    for matched in NAMED_PATTERN.find_iter(text) {
        found.push((
            matched.start(),
            PlaceholderToken {
                kind: PlaceholderKind::Named,
                value: matched.as_str().to_owned(),
            },
        ));
    }

    for matched in ESCAPE_PATTERN.find_iter(text) {
        let value = match matched.as_str() {
            "\r\n" | "\r" | "\n" | "\\n" => "\\n",
            "\t" | "\\t" => "\\t",
            "\\\\" => "\\\\",
            _ => unreachable!("escape regex only matches supported escapes"),
        };
        found.push((
            matched.start(),
            PlaceholderToken {
                kind: PlaceholderKind::Escape,
                value: value.to_owned(),
            },
        ));
    }

    for matched in MINECRAFT_PATTERN.find_iter(text) {
        found.push((
            matched.start(),
            PlaceholderToken {
                kind: PlaceholderKind::MinecraftFormatting,
                value: matched.as_str().to_owned(),
            },
        ));
    }

    found.sort_by_key(|(position, _)| *position);
    found.into_iter().map(|(_, token)| token).collect()
}

fn is_ambiguous_uppercase_word_start(text: &str, captures: &regex::Captures<'_>) -> bool {
    let Some(matched) = captures.get(0) else {
        return false;
    };
    let remainder = &text[matched.start()..];
    const LITERAL_PREFIX: &str = "%Creative";

    // `%Creative` は既存の言語データにある通常文なので例外扱いする。
    // 他の大文字変換（例: `%Sfoo`）はJava Formatterの引数として必ず検証する。
    remainder
        .get(..LITERAL_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(LITERAL_PREFIX))
        && remainder
            .as_bytes()
            .get(LITERAL_PREFIX.len())
            .is_none_or(|character| !character.is_ascii_alphanumeric())
}

/// printf引数の参照先と書式を比較する。順序変更は参照先が同じ場合に限って許可する。
pub fn compare_placeholders(source: &str, translation: &str) -> PlaceholderComparison {
    let source_tokens = extract_placeholders(source);
    let target_tokens = extract_placeholders(translation);
    let (mut difference, mut has_error) = compare_printf_arguments(&source_tokens, &target_tokens);

    // § の装飾、改行、brace / named token は通常の翻訳文字列ではただの文字列。
    // Minecraftや各modの別パーサーだけが解釈する記法を共通チェックで比較しない。
    let source_invalid = unsupported_printf_tokens(source);
    let target_invalid = unsupported_printf_tokens(translation);
    let invalid_difference = multiset_difference(&source_invalid, &target_invalid);
    if !invalid_difference.extra.is_empty() {
        has_error = true;
        difference.extra.extend(invalid_difference.extra);
    }

    difference
        .missing
        .sort_by(|left, right| left.value.cmp(&right.value));
    difference
        .extra
        .sort_by(|left, right| left.value.cmp(&right.value));
    let is_valid = difference.missing.is_empty() && difference.extra.is_empty();
    PlaceholderComparison {
        is_valid,
        severity: if is_valid {
            None
        } else if has_error {
            Some(PlaceholderSeverity::Error)
        } else {
            Some(PlaceholderSeverity::Warning)
        },
        difference,
    }
}

fn is_valid_printf_specifier(captures: &regex::Captures<'_>) -> bool {
    let flags = captures.name("flags").map_or("", |value| value.as_str());
    let conversion = captures
        .name("conversion")
        .expect("printf match always includes a conversion")
        .as_str();
    let has_index = captures.name("index").is_some();
    let has_width = captures.name("width").is_some();
    let has_precision = captures.name("precision").is_some();

    // Java Formatter はこれらの数値を int として読むため、範囲外の書式は Placeholder としない。
    if captures
        .name("index")
        .into_iter()
        .chain(captures.name("width"))
        .chain(captures.name("precision"))
        .any(|value| value.as_str().parse::<i32>().is_err())
    {
        return false;
    }

    let mut seen = [false; 128];
    for flag in flags.bytes() {
        if flag as usize >= seen.len() || seen[flag as usize] {
            return false;
        }
        seen[flag as usize] = true;
    }

    if conversion == "n" {
        return flags.is_empty() && !has_index && !has_width && !has_precision;
    }

    if has_index && flags.contains('<') {
        return false;
    }

    // Formatter requires a width for left justification and zero padding, and rejects
    // conflicting alignment/padding or sign flags before formatting any argument.
    if ((flags.contains('-') || flags.contains('0')) && !has_width)
        || (flags.contains('-') && flags.contains('0'))
        || (flags.contains('+') && flags.contains(' '))
    {
        return false;
    }

    let allowed_flags = match conversion.as_bytes()[0] {
        b'b' | b'B' | b'h' | b'H' => "-<",
        b's' | b'S' => "-#<",
        b'c' | b'C' => "-<",
        b'd' => "-+ 0,(<",
        b'o' | b'x' | b'X' => "-#0<",
        b'e' | b'E' => "-#+ 0(<",
        b'f' => "-#+ 0,(<",
        b'g' | b'G' => "-+ 0,(<",
        b'a' | b'A' => "-#+ 0(<",
        b't' | b'T' => "-<",
        _ => return false,
    };
    if flags.chars().any(|flag| !allowed_flags.contains(flag)) {
        return false;
    }

    if conversion.starts_with(['t', 'T']) {
        return !has_precision && flags.chars().all(|flag| flag == '-' || flag == '<');
    }

    let conversion = conversion.as_bytes()[0];
    if has_precision && matches!(conversion, b'c' | b'C' | b'd' | b'o' | b'x' | b'X' | b'n') {
        return false;
    }

    true
}

fn multiset_difference(
    source: &[PlaceholderToken],
    target: &[PlaceholderToken],
) -> PlaceholderDifference {
    let mut target_counts = token_counts(target, |_| true);
    let mut missing = Vec::new();
    for token in source {
        let key = comparison_key(token);
        match target_counts.get_mut(&key) {
            Some(count) if *count > 0 => *count -= 1,
            _ => missing.push(token.clone()),
        }
    }

    let mut source_counts = token_counts(source, |_| true);
    let mut extra = Vec::new();
    for token in target {
        let key = comparison_key(token);
        match source_counts.get_mut(&key) {
            Some(count) if *count > 0 => *count -= 1,
            _ => extra.push(token.clone()),
        }
    }

    PlaceholderDifference { missing, extra }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TokenKey {
    kind: PlaceholderKind,
    value: String,
}

fn comparison_key(token: &PlaceholderToken) -> TokenKey {
    let value = if token.kind == PlaceholderKind::MinecraftFormatting {
        token.value.to_ascii_lowercase()
    } else {
        token.value.clone()
    };
    TokenKey {
        kind: token.kind,
        value,
    }
}

fn token_counts(
    tokens: &[PlaceholderToken],
    include: impl Fn(&PlaceholderToken) -> bool,
) -> HashMap<TokenKey, usize> {
    let mut counts = HashMap::new();
    for token in tokens.iter().filter(|token| include(token)) {
        *counts.entry(comparison_key(token)).or_default() += 1;
    }
    counts
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PrintfArgument {
    index: Option<usize>,
    reuses_previous: bool,
    signature: String,
}

#[derive(Clone, Debug)]
struct IndexedPrintfArgument {
    signature: String,
    token: PlaceholderToken,
}

#[derive(Default)]
struct PrintfReferences {
    by_index: HashMap<usize, Vec<IndexedPrintfArgument>>,
    relative_errors: Vec<PlaceholderToken>,
}

fn printf_argument(token: &PlaceholderToken) -> Option<PrintfArgument> {
    if token.kind != PlaceholderKind::Printf {
        return None;
    }

    let captures = PRINTF_PATTERN.captures(&token.value)?;
    if captures.name("escaped").is_some() || !is_valid_printf_specifier(&captures) {
        return None;
    }
    let conversion = captures.name("conversion")?.as_str();
    // %n は Java Formatter の改行指定で、TranslatableContents の引数ではない。
    if conversion == "n" {
        return None;
    }

    // Java Formatter の大文字変換は表示結果の大小文字だけを変え、参照する引数型は小文字と同じ。
    // Minecraft の翻訳では %s / %n$s が標準だが、古いmodの他の変換も引数参照を追跡する。
    let signature = if conversion.starts_with(['t', 'T']) {
        "datetime".to_owned()
    } else {
        match conversion.as_bytes()[0].to_ascii_lowercase() {
            b's' | b'd' | b'f' | b'b' | b'h' | b'c' | b'o' | b'x' | b'e' | b'g' | b'a' => {
                (conversion.as_bytes()[0] as char)
                    .to_ascii_lowercase()
                    .to_string()
            }
            _ => return None,
        }
    };

    let flags = captures.name("flags").map_or("", |flags| flags.as_str());
    let index = captures
        .name("index")
        .and_then(|value| value.as_str().parse().ok());

    Some(PrintfArgument {
        index,
        reuses_previous: flags.contains('<'),
        signature,
    })
}

fn compare_printf_arguments(
    source: &[PlaceholderToken],
    target: &[PlaceholderToken],
) -> (PlaceholderDifference, bool) {
    let source_references = printf_argument_references(source);
    let target_references = printf_argument_references(target);
    // 呼び出し側の実引数は言語ファイルから分からないため、原文が参照する最大番号を
    // 判定可能な範囲とする。訳文だけがそれを超える番号を要求する場合は実行時例外の恐れがある。
    let source_max_index = source_references
        .by_index
        .keys()
        .max()
        .copied()
        .unwrap_or(0);
    let mut difference = PlaceholderDifference::default();
    let mut has_error = false;

    let relative_error_difference = multiset_difference(
        &source_references.relative_errors,
        &target_references.relative_errors,
    );
    if !relative_error_difference.extra.is_empty() {
        has_error = true;
        difference.extra.extend(relative_error_difference.extra);
    }

    for (index, source_arguments) in &source_references.by_index {
        let Some(target_arguments) = target_references.by_index.get(index) else {
            difference.missing.extend(
                source_arguments
                    .iter()
                    .map(|argument| argument.token.clone()),
            );
            continue;
        };

        let source_signatures: std::collections::HashSet<_> = source_arguments
            .iter()
            .map(|argument| &argument.signature)
            .collect();
        let target_signatures: std::collections::HashSet<_> = target_arguments
            .iter()
            .map(|argument| &argument.signature)
            .collect();
        if source_signatures != target_signatures {
            difference.missing.extend(
                source_arguments
                    .iter()
                    .map(|argument| argument.token.clone()),
            );
            difference.extra.extend(
                target_arguments
                    .iter()
                    .map(|argument| argument.token.clone()),
            );
            has_error = true;
        }
    }

    for (index, target_arguments) in &target_references.by_index {
        if *index > source_max_index {
            difference.extra.extend(
                target_arguments
                    .iter()
                    .map(|argument| argument.token.clone()),
            );
            has_error = true;
        }
    }

    (difference, has_error)
}

fn printf_argument_references(tokens: &[PlaceholderToken]) -> PrintfReferences {
    let mut implicit_index = 0;
    let mut previous_index = None;
    let mut references = PrintfReferences::default();
    for token in tokens {
        let Some(argument) = printf_argument(token) else {
            continue;
        };
        // 明示番号の `%n$s` は順次カウンターを進めず、`%s` と独立して混在できる。
        let index = if argument.reuses_previous {
            let Some(index) = previous_index else {
                references.relative_errors.push(token.clone());
                continue;
            };
            index
        } else {
            let index = match argument.index {
                Some(index) => index,
                None => {
                    implicit_index += 1;
                    implicit_index
                }
            };
            previous_index = Some(index);
            index
        };
        references
            .by_index
            .entry(index)
            .or_insert_with(Vec::new)
            .push(IndexedPrintfArgument {
                signature: argument.signature,
                token: token.clone(),
            });
    }
    references
}

fn unsupported_printf_tokens(text: &str) -> Vec<PlaceholderToken> {
    PRINTF_CANDIDATE_PATTERN
        .find_iter(text)
        .filter_map(|matched| {
            let is_natural_percent_of = matched.as_str() == "% o"
                && NATURAL_PERCENT_OF_PATTERN
                    .find_iter(text)
                    .any(|natural_match| {
                        text[natural_match.start()..natural_match.end()]
                            .find('%')
                            .is_some_and(|offset| natural_match.start() + offset == matched.start())
                    });
            if is_natural_percent_of {
                return None;
            }

            let captures = PRINTF_PATTERN.captures_at(text, matched.start());
            let valid_format = captures
                .as_ref()
                .and_then(|captures| captures.get(0))
                .filter(|whole| whole.start() == matched.start())
                .is_some_and(|_| {
                    captures.as_ref().is_some_and(|captures| {
                        captures.name("escaped").is_some() || is_valid_printf_specifier(captures)
                    })
                });
            (!valid_format).then(|| PlaceholderToken {
                kind: PlaceholderKind::Printf,
                value: matched.as_str().to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(text: &str, kind: PlaceholderKind) -> Vec<String> {
        extract_placeholders(text)
            .into_iter()
            .filter(|token| token.kind == kind)
            .map(|token| token.value)
            .collect()
    }

    #[test]
    fn extracts_java_and_minecraft_printf_forms() {
        let tokens = extract_placeholders("%s %d %f %1$s %2$d %.2f %5s %% %5% %-5% %n %tY");
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.value.as_str())
                .collect::<Vec<_>>(),
            [
                "%s", "%d", "%f", "%1$s", "%2$d", "%.2f", "%5s", "%%", "%5%", "%-5%", "%n", "%tY"
            ]
        );
        assert_eq!(tokens[7].kind, PlaceholderKind::EscapedPercent);
        assert_eq!(tokens[8].kind, PlaceholderKind::EscapedPercent);
        assert_eq!(tokens[9].kind, PlaceholderKind::EscapedPercent);
    }

    #[test]
    fn extracts_numbered_and_named_placeholders() {
        assert_eq!(
            values("{0} then {12}", PlaceholderKind::Numbered),
            ["{0}", "{12}"]
        );
        assert_eq!(
            values("${name} ${snake_case9}", PlaceholderKind::Named),
            ["${name}", "${snake_case9}"]
        );
    }

    #[test]
    fn extracts_escaped_and_actual_newlines_and_tabs() {
        assert_eq!(
            values(r"\n \t \\", PlaceholderKind::Escape),
            [r"\n", r"\t", r"\\"]
        );
        assert_eq!(values("\n\t", PlaceholderKind::Escape), [r"\n", r"\t"]);
        assert_eq!(
            values("\r\n\t\r", PlaceholderKind::Escape),
            [r"\n", r"\t", r"\n"]
        );
    }

    #[test]
    fn extracts_minecraft_formatting_codes_in_both_cases() {
        let codes = values(
            "§0 §9 §a §F §k §O §r §R",
            PlaceholderKind::MinecraftFormatting,
        );
        assert_eq!(codes, ["§0", "§9", "§a", "§F", "§k", "§O", "§r", "§R"]);
    }

    #[test]
    fn compares_placeholder_multisets_without_ordering() {
        assert!(compare_placeholders("Energy %s costs %d", "%2$d を消費する %1$s").is_valid);

        let missing = compare_placeholders("Energy %s", "Energy");
        assert!(!missing.is_valid);
        assert_eq!(missing.severity, Some(PlaceholderSeverity::Warning));
        assert_eq!(missing.difference.missing[0].value, "%s");
        assert!(missing.difference.extra.is_empty());

        let extra = compare_placeholders("Energy %s", "Energy %s %s");
        assert!(!extra.is_valid);
        assert_eq!(extra.severity, Some(PlaceholderSeverity::Error));
        assert_eq!(extra.difference.extra.len(), 1);
        assert_eq!(extra.difference.extra[0].value, "%s");
    }

    #[test]
    fn compares_referenced_arguments_and_allows_japanese_reordering() {
        assert!(compare_placeholders("%s gave %s", "%2$sが%1$sに渡した").is_valid);
        assert!(
            compare_placeholders(
                "Edited the stat %s for %s player(s).",
                "%2$s人のプレイヤーの統計%1$sを編集しました"
            )
            .is_valid
        );
        assert!(compare_placeholders("%s %d", "%2$d %1$s").is_valid);
        assert!(compare_placeholders("%s %2$s", "%2$s が %s").is_valid);
        assert!(compare_placeholders("%s %2$s", "%1$s %2$s").is_valid);
        assert!(compare_placeholders("%s %2$d", "%2$d を %s").is_valid);
        assert!(compare_placeholders("%2$s %s", "%2$s %s %<s").is_valid);
        assert!(compare_placeholders("Current year: %tY", "現在の年: %1$tB").is_valid);

        let relative_argument_without_previous = compare_placeholders("plain text", "%<s");
        assert_eq!(
            relative_argument_without_previous.severity,
            Some(PlaceholderSeverity::Error)
        );

        let missing_argument = compare_placeholders("%s %2$s", "%1$s");
        assert_eq!(
            missing_argument.severity,
            Some(PlaceholderSeverity::Warning)
        );
        assert_eq!(missing_argument.difference.missing[0].value, "%2$s");

        let out_of_range_argument = compare_placeholders("%s gave %s", "%3$sが%1$sに渡した");
        assert_eq!(
            out_of_range_argument.severity,
            Some(PlaceholderSeverity::Error)
        );
        assert!(!out_of_range_argument.is_valid);
        assert_eq!(out_of_range_argument.difference.missing.len(), 1);
        assert_eq!(out_of_range_argument.difference.extra.len(), 1);

        let wrong_argument_set = compare_placeholders("%s %d", "%2$s %1$d");
        assert_eq!(
            wrong_argument_set.severity,
            Some(PlaceholderSeverity::Error)
        );
        assert!(!wrong_argument_set.is_valid);
        assert_eq!(wrong_argument_set.difference.missing.len(), 2);
        assert_eq!(wrong_argument_set.difference.extra.len(), 2);
    }

    #[test]
    fn uppercase_java_conversions_keep_argument_type_and_range_checks() {
        assert!(compare_placeholders("Value %s", "Value %S").is_valid);
        for (source, translation) in [
            ("%s", "%1$S"),
            ("%b", "%1$B"),
            ("%h", "%1$H"),
            ("%c", "%1$C"),
            ("%x", "%1$X"),
            ("%e", "%1$E"),
            ("%g", "%1$G"),
            ("%a", "%1$A"),
            ("%tY", "%1$TY"),
        ] {
            assert!(
                compare_placeholders(source, translation).is_valid,
                "{translation} should keep the argument type of {source}"
            );
        }

        let extra_argument = compare_placeholders("Value %s", "%1$s %2$S");
        assert_eq!(extra_argument.severity, Some(PlaceholderSeverity::Error));
        assert_eq!(extra_argument.difference.extra[0].value, "%2$S");

        let wrong_type = compare_placeholders("Value %s", "%1$X");
        assert_eq!(wrong_type.severity, Some(PlaceholderSeverity::Error));
    }

    #[test]
    fn ignores_literal_percent_newlines_formatting_and_mod_specific_syntax() {
        assert!(
            compare_placeholders(
                "A warm, red stone that makes up 90% of the Gold Dungeon's interior.",
                "ゴールドダンジョンの内装の9割を構成する、暖かく赤い石。"
            )
            .is_valid
        );
        assert!(
            compare_placeholders(
                "Is the attack strength below 100%?",
                "攻撃強度が100%%未満の場合。"
            )
            .is_valid
        );
        assert!(compare_placeholders(
            "A warm stone makes up 90% of the dungeon.\n§a%s§r {0} ${player} $(item) <green>text</green>",
            "ダンジョンの90％を占める石。\n§b%1$s§r {player} ${name} $(other) <red>テキスト</red>"
        )
        .is_valid);
        assert!(compare_placeholders("A literal %% sign", "全角％記号").is_valid);
        assert!(
            compare_placeholders("Formatted %s", "書式済み %%s").severity
                == Some(PlaceholderSeverity::Warning)
        );
    }

    #[test]
    fn real_mod_examples_distinguish_omitted_arguments_from_bad_formats() {
        assert!(
            compare_placeholders(
                "%Creative Tab Search Mode",
                "検索モード（%クリエイティブタブ）"
            )
            .is_valid
        );

        let random_mode_description = compare_placeholders(
            "Randomize Only First Time: %s",
            "このレイアウトのグループを設定します。  \n画面を開くときに、グループごとに1つのレイアウトが選ばれます。",
        );
        assert_eq!(
            random_mode_description.severity,
            Some(PlaceholderSeverity::Warning)
        );

        let omitted_argument = compare_placeholders(
            "You seem to have downloaded %1$s from a malicious site. Get the real version %2$s.",
            "%1$sを非公式のサイトでダウンロードしたようです。正しいバージョンを入手してください。",
        );
        assert_eq!(
            omitted_argument.severity,
            Some(PlaceholderSeverity::Warning)
        );
        assert_eq!(omitted_argument.difference.missing[0].value, "%2$s");

        let malformed_translation = compare_placeholders(
            "Mapping format: track1.ogg=>Cool Name%%:%%track2.wav=>Another Name",
            "マッピング形式: track1.ogg=>Cool Name%:%track2.wav=>Another Name",
        );
        assert_eq!(
            malformed_translation.severity,
            Some(PlaceholderSeverity::Error)
        );
        assert_eq!(malformed_translation.difference.extra[0].value, "%tr");

        let escaped_argument =
            compare_placeholders("%s Synced Standard Search Keep", "%%s連携標準検索-保持");
        assert_eq!(
            escaped_argument.severity,
            Some(PlaceholderSeverity::Warning)
        );
        assert_eq!(escaped_argument.difference.missing[0].value, "%s");

        let omitted_auto_search_argument =
            compare_placeholders("%s Synced Auto Search Keep", "s連携自動検索-保持");
        assert_eq!(
            omitted_auto_search_argument.severity,
            Some(PlaceholderSeverity::Warning)
        );

        let unprovided_beacon_level = compare_placeholders(
            r"A nearby beacon in an 8 block radius of the terminal\extends the range of the terminal.\The beacon's level determines the range:",
            r"紐づけされたターミナルから半径8ブロックの範囲に\レベル%dのビーコンを建設すれば同ディメンション内のどこからでも接続可能",
        );
        assert_eq!(
            unprovided_beacon_level.severity,
            Some(PlaceholderSeverity::Error)
        );
        assert_eq!(unprovided_beacon_level.difference.extra[0].value, "%d");

        let hidden_inventory_count = compare_placeholders(
            "Inventory Connector: %d free/%d slots (in %d inventories).",
            "インベントリコネクタ: 空きスロット %d/全スロット %d",
        );
        assert_eq!(
            hidden_inventory_count.severity,
            Some(PlaceholderSeverity::Warning)
        );
        assert_eq!(hidden_inventory_count.difference.missing[0].value, "%d");
    }

    #[test]
    fn added_unsupported_percent_format_is_an_error() {
        let comparison = compare_placeholders("Plain text", "書式 %q");
        assert_eq!(comparison.severity, Some(PlaceholderSeverity::Error));
        assert_eq!(comparison.difference.extra[0].value, "%q");

        let invalid_flag = compare_placeholders("Plain text", "書式 %#h");
        assert_eq!(invalid_flag.severity, Some(PlaceholderSeverity::Error));
        assert_eq!(invalid_flag.difference.extra[0].value, "%#h");
    }

    #[test]
    fn compares_minecraft_formatting_case_insensitively() {
        assert!(compare_placeholders("§aGreen §R", "§AGreen §r").is_valid);
    }
}
