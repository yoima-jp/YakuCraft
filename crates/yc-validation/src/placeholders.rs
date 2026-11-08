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

static PATCHOULI_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\([^)\r\n]*\)").expect("Patchouli formatting regex is a compile-time constant")
});

static PATCHOULI_TEMPLATE_VARIABLE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#[^#\s]+#").expect("Patchouli template variable regex is a compile-time constant")
});

static FTBQUESTS_FORMATTING_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:&[0-9a-fk-or])")
        .expect("FTB Quests formatting regex is a compile-time constant")
});

// `§x` は 6 桁の RGB を続けて書く 16 進カラー指定で、上の `§[0-9a-fk-or]` には含まれない。
static MINECRAFT_HEX_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:§x(?:§[0-9a-f]){6})")
        .expect("Minecraft hex color regex is a compile-time constant")
});

static FTBQUESTS_HEX_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:&x(?:&[0-9a-f]){6})")
        .expect("FTB Quests hex color regex is a compile-time constant")
});

// `{name}` 形式は Java の MessageFormat 由来で、`{0}` とは別に名前付き引数として使われる。
static BRACE_NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{[A-Za-z_][A-Za-z0-9_]*\}")
        .expect("brace name placeholder regex is a compile-time constant")
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
    PatchouliFormatting,
    PatchouliTemplateVariable,
    FtbQuestsFormatting,
    FtbQuestsTag,
    FtbQuestsJson,
    /// `{name}` 形式の名前付き引数。
    ///
    /// [`extract_placeholders_for_profile`] は比較対象にしない（既存の判定を変えないため）。
    /// [`extract_protected_spans`] だけが、マスク対象としてこの分類を返す。
    BraceName,
}

/// Placeholder の書式プロファイル。`lang` は既存の比較動作を維持する。
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaceholderProfile {
    #[default]
    Lang,
    Patchouli,
    FtbQuests,
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
    extract_placeholders_for_profile(text, PlaceholderProfile::Lang)
}

/// 指定した書式プロファイルに応じた Placeholder を出現順に抽出する。
pub fn extract_placeholders_for_profile(
    text: &str,
    profile: PlaceholderProfile,
) -> Vec<PlaceholderToken> {
    extract_placeholders_with_ranges(text, profile)
        .into_iter()
        .map(|(_, _, token)| token)
        .collect()
}

/// 既存の抽出器と同じ判定を、原文のバイト範囲付きで返す。
///
/// 範囲を持つ版を別に用意し、公開済みの [`extract_placeholders_for_profile`] の
/// 戻り値と順序はそのまま保つ。範囲はマスク処理のように原文の位置が必要な用途だけが使う。
fn extract_placeholders_with_ranges(
    text: &str,
    profile: PlaceholderProfile,
) -> Vec<(usize, usize, PlaceholderToken)> {
    let mut found = Vec::new();
    let mut opaque_ranges = Vec::new();

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
            matched.end(),
            PlaceholderToken {
                kind,
                value: matched.as_str().to_owned(),
            },
        ));
    }

    for matched in NUMBERED_PATTERN.find_iter(text) {
        found.push((
            matched.start(),
            matched.end(),
            PlaceholderToken {
                kind: PlaceholderKind::Numbered,
                value: matched.as_str().to_owned(),
            },
        ));
    }

    for matched in NAMED_PATTERN.find_iter(text) {
        found.push((
            matched.start(),
            matched.end(),
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
            matched.end(),
            PlaceholderToken {
                kind: PlaceholderKind::Escape,
                value: value.to_owned(),
            },
        ));
    }

    for matched in MINECRAFT_PATTERN.find_iter(text) {
        found.push((
            matched.start(),
            matched.end(),
            PlaceholderToken {
                kind: PlaceholderKind::MinecraftFormatting,
                value: matched.as_str().to_owned(),
            },
        ));
    }

    // Patchouli の lang 値にも書式が現れるが、通常の lang の既存判定を変えないため、
    // Patchouli formatting tokens and template variables are recognized only for this profile.
    // Patchouli 値では閉じた構文を保護し、書式そのものを説明する文の誤検出より改変防止を優先する。
    if profile == PlaceholderProfile::Patchouli {
        // マクロは本ごとに追加・再定義できるため、既知の名前で絞らず閉じ括弧までを保護する。
        for matched in PATCHOULI_PATTERN.find_iter(text) {
            opaque_ranges.push((matched.start(), matched.end()));
            found.push((
                matched.start(),
                matched.end(),
                PlaceholderToken {
                    kind: PlaceholderKind::PatchouliFormatting,
                    value: matched.as_str().to_owned(),
                },
            ));
        }
        for matched in PATCHOULI_TEMPLATE_VARIABLE_PATTERN.find_iter(text) {
            opaque_ranges.push((matched.start(), matched.end()));
            found.push((
                matched.start(),
                matched.end(),
                PlaceholderToken {
                    kind: PlaceholderKind::PatchouliTemplateVariable,
                    value: matched.as_str().to_owned(),
                },
            ));
        }
    }

    if profile == PlaceholderProfile::FtbQuests {
        for matched in FTBQUESTS_FORMATTING_PATTERN.find_iter(text) {
            found.push((
                matched.start(),
                matched.end(),
                PlaceholderToken {
                    kind: PlaceholderKind::FtbQuestsFormatting,
                    value: matched.as_str().to_owned(),
                },
            ));
        }

        let json_ranges = ftbquests_json_component_ranges(text);
        for (start, end) in &json_ranges {
            opaque_ranges.push((*start, *end));
            found.push((
                *start,
                *end,
                PlaceholderToken {
                    kind: PlaceholderKind::FtbQuestsJson,
                    value: text[*start..*end].to_owned(),
                },
            ));
        }

        for (start, end) in balanced_brace_ranges(text) {
            if json_ranges
                .iter()
                .any(|(json_start, json_end)| *json_start <= start && end <= *json_end)
            {
                continue;
            }
            if text[..start].ends_with('$') {
                continue;
            }
            let value = &text[start..end];
            let contents = &value[1..value.len() - 1];
            if !contents.is_empty() && contents.chars().all(|character| character.is_ascii_digit())
            {
                continue;
            }
            found.push((
                start,
                end,
                PlaceholderToken {
                    kind: PlaceholderKind::FtbQuestsTag,
                    value: value.to_owned(),
                },
            ));
            opaque_ranges.push((start, end));
        }
    }

    found.retain(|(start, end, _)| {
        !opaque_ranges.iter().any(|(opaque_start, opaque_end)| {
            *opaque_start <= *start
                && *end <= *opaque_end
                && (*opaque_start != *start || *opaque_end != *end)
        })
    });

    found.sort_by_key(|(start, _, _)| *start);
    found
}

/// 原文の保護対象区間。マスク処理が原文をそのまま復元できるよう、生の文字列も持つ。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectedSpan {
    pub kind: PlaceholderKind,
    pub start: usize,
    pub end: usize,
    pub raw: String,
}

/// 指定した書式プロファイルで、訳文へそのまま残す必要がある区間を原文のバイト範囲付きで返す。
///
/// [`extract_placeholders_for_profile`] は比較用に正規化した値だけを返し、原文の位置も
/// `§` の 16 進カラー、`{name}` 形式、FTB の `&x` カラーを持たない。翻訳前のマスクと
/// 翻訳後の復元には生の区間が必要なので、その用途専用にこの関数を別途用意する。
/// 既存の比較関数の判定はここでは変更しない。
pub fn extract_protected_spans(text: &str, profile: PlaceholderProfile) -> Vec<ProtectedSpan> {
    let mut spans: Vec<ProtectedSpan> = extract_placeholders_with_ranges(text, profile)
        .into_iter()
        .map(|(start, end, token)| ProtectedSpan {
            kind: token.kind,
            start,
            end,
            raw: text[start..end].to_owned(),
        })
        .collect();

    // 追加分は既存抽出器が対象外にしている記法。`§x` の 16 進カラーは内側の `§1` 等も
    // 既存抽出器が拾うため、区間が入れ子になる。マスクは区間の重なりを許さないので、
    // 同じ開始位置では長い区間を優先し、外側に完全に含まれる内側の区間は捨てる。
    let mut additions = Vec::new();
    for matched in MINECRAFT_HEX_PATTERN.find_iter(text) {
        additions.push((
            matched.start(),
            matched.end(),
            PlaceholderKind::MinecraftFormatting,
        ));
    }
    for matched in BRACE_NAME_PATTERN.find_iter(text) {
        // `{0}` 形式の数値引数は既存の Numbered が担当する。ここでは名前付きだけを足す。
        additions.push((matched.start(), matched.end(), PlaceholderKind::BraceName));
    }
    if profile == PlaceholderProfile::FtbQuests {
        for matched in FTBQUESTS_HEX_PATTERN.find_iter(text) {
            additions.push((
                matched.start(),
                matched.end(),
                PlaceholderKind::FtbQuestsFormatting,
            ));
        }
    }
    spans.extend(
        additions
            .into_iter()
            .map(|(start, end, kind)| ProtectedSpan {
                kind,
                start,
                end,
                raw: text[start..end].to_owned(),
            }),
    );

    spans.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.end.cmp(&left.end))
    });
    let mut selected: Vec<ProtectedSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        match selected.last_mut() {
            // 外側の区間が既に覆っている内側の区間は捨てる。
            Some(previous) if span.start < previous.end => {
                if span.end > previous.end {
                    // 入れ子ではなく部分的な重なり。区間を分割するとマーカーが壊れるため、
                    // 覆えていない末尾まで広げて 1 区間として扱う。
                    previous.end = span.end;
                    previous.raw = text[previous.start..span.end].to_owned();
                }
            }
            _ => selected.push(span),
        }
    }
    selected
}

fn ftbquests_json_component_ranges(text: &str) -> Vec<(usize, usize)> {
    // JSON コンポーネントは子オブジェクトを含むため、内側の波括弧ではなく全体を一つにする。
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !matches!(bytes[index], b'{' | b'[') {
            index += 1;
            continue;
        }
        let Some(end) = balanced_json_end(bytes, index) else {
            index += 1;
            continue;
        };
        let candidate = &text[index..end];
        if looks_like_json_text_component(candidate) {
            ranges.push((index, end));
            index = end;
        } else {
            index += 1;
        }
    }
    ranges
}

fn balanced_brace_ranges(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        if let Some(end) = balanced_json_end(bytes, index) {
            ranges.push((index, end));
            index = end;
        } else {
            index += 1;
        }
    }
    ranges
}

fn balanced_json_end(bytes: &[u8], start: usize) -> Option<usize> {
    let opening = *bytes.get(start)?;
    let mut closers = Vec::new();
    closers.push(match opening {
        b'{' => b'}',
        b'[' => b']',
        _ => return None,
    });
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes.iter().copied().enumerate().skip(start + 1) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => closers.push(b'}'),
            b'[' => closers.push(b']'),
            b'}' | b']' => {
                if closers.pop() != Some(byte) {
                    return None;
                }
                if closers.is_empty() {
                    return Some(offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn looks_like_json_text_component(candidate: &str) -> bool {
    let has_component_field = [
        "\"text\"",
        "\"translate\"",
        "\"selector\"",
        "\"score\"",
        "\"keybind\"",
        "\"nbt\"",
    ]
    .iter()
    .any(|field| candidate.contains(field));
    let is_json_array = candidate.starts_with("[")
        && candidate
            .find('{')
            .is_some_and(|index| candidate[index + 1..].trim_start().starts_with('"'));

    (candidate.starts_with('{') && has_component_field) || is_json_array
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
    compare_placeholders_for_profile(source, translation, PlaceholderProfile::Lang)
}

/// 指定した書式プロファイルで Placeholder を比較する。
pub fn compare_placeholders_for_profile(
    source: &str,
    translation: &str,
    profile: PlaceholderProfile,
) -> PlaceholderComparison {
    let source_tokens = extract_placeholders_for_profile(source, profile);
    let target_tokens = extract_placeholders_for_profile(translation, profile);
    let (mut difference, mut has_error) = compare_printf_arguments(&source_tokens, &target_tokens);

    let profiled_kinds: &[PlaceholderKind] = match profile {
        PlaceholderProfile::Lang => &[],
        PlaceholderProfile::Patchouli => &[
            PlaceholderKind::PatchouliFormatting,
            PlaceholderKind::PatchouliTemplateVariable,
        ],
        PlaceholderProfile::FtbQuests => &[
            PlaceholderKind::FtbQuestsFormatting,
            PlaceholderKind::FtbQuestsTag,
            PlaceholderKind::FtbQuestsJson,
            PlaceholderKind::Escape,
        ],
    };
    let profiled_difference = multiset_difference(
        &source_tokens
            .iter()
            .filter(|token| profiled_kinds.contains(&token.kind))
            .cloned()
            .collect::<Vec<_>>(),
        &target_tokens
            .iter()
            .filter(|token| profiled_kinds.contains(&token.kind))
            .cloned()
            .collect::<Vec<_>>(),
    );
    if !profiled_difference.extra.is_empty() {
        has_error = true;
    }
    difference.missing.extend(profiled_difference.missing);
    difference.extra.extend(profiled_difference.extra);

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

    #[test]
    fn patchouli_formatting_tokens_are_extracted_and_compared_as_a_multiset() {
        let source = "$(l) $(br) $(#ff0000) $(custom_macro:value)";
        let tokens = extract_placeholders_for_profile(source, PlaceholderProfile::Patchouli);
        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.kind == PlaceholderKind::PatchouliFormatting)
                .map(|token| token.value.as_str())
                .collect::<Vec<_>>(),
            ["$(l)", "$(br)", "$(#ff0000)", "$(custom_macro:value)"]
        );
        let nested_printf =
            extract_placeholders_for_profile("$(t:%s)", PlaceholderProfile::Patchouli);
        assert_eq!(nested_printf.len(), 1);
        assert_eq!(nested_printf[0].kind, PlaceholderKind::PatchouliFormatting);
        assert!(
            compare_placeholders_for_profile(
                "$(l) Stone $(br)",
                "$(br) 石 $(l)",
                PlaceholderProfile::Patchouli,
            )
            .is_valid
        );

        let missing =
            compare_placeholders_for_profile("$(item) Stone", "石", PlaceholderProfile::Patchouli);
        assert_eq!(missing.difference.missing[0].value, "$(item)");

        let modified = compare_placeholders_for_profile(
            "$(item) Stone",
            "$(thing) 石",
            PlaceholderProfile::Patchouli,
        );
        assert_eq!(modified.difference.missing[0].value, "$(item)");
        assert_eq!(modified.difference.extra[0].value, "$(thing)");

        // lang の既定判定を保ち、書式プロファイルが明示されたときだけ Patchouli を検証する。
        assert!(compare_placeholders("$(item) Stone", "$(thing) 石").is_valid);
    }

    #[test]
    fn ftbquests_formatting_tags_json_escapes_and_printf_are_protected() {
        let source = r#"&aTitle {@pagebreak} {image:mod:path width:200} {"text":"Link %s","clickEvent":{"action":"run_command","value":"/go"}}\n %s"#;
        let tokens = extract_placeholders_for_profile(source, PlaceholderProfile::FtbQuests);
        assert!(tokens.iter().any(|token| {
            token.kind == PlaceholderKind::FtbQuestsFormatting && token.value == "&a"
        }));
        assert!(tokens.iter().any(|token| {
            token.kind == PlaceholderKind::FtbQuestsTag && token.value == "{@pagebreak}"
        }));
        assert!(tokens.iter().any(|token| {
            token.kind == PlaceholderKind::FtbQuestsTag
                && token.value == "{image:mod:path width:200}"
        }));
        assert!(tokens.iter().any(|token| {
            token.kind == PlaceholderKind::FtbQuestsJson
                && token.value
                    == r#"{"text":"Link %s","clickEvent":{"action":"run_command","value":"/go"}}"#
        }));
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == PlaceholderKind::Escape)
        );
        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.kind == PlaceholderKind::Printf && token.value == "%s")
                .count(),
            1
        );

        assert!(compare_placeholders_for_profile(
            source,
            r#"&aTitle %1$s {"text":"Link %s","clickEvent":{"action":"run_command","value":"/go"}}{image:mod:path width:200}{@pagebreak}\n"#,
            PlaceholderProfile::FtbQuests,
        )
        .is_valid);

        let changed_code = compare_placeholders_for_profile(
            "&aTitle {@pagebreak} %s",
            "&cTitle {@pagebreak} %s",
            PlaceholderProfile::FtbQuests,
        );
        assert_eq!(changed_code.difference.missing[0].value, "&a");
        assert_eq!(changed_code.difference.extra[0].value, "&c");

        let changed_json = compare_placeholders_for_profile(
            r#"{"text":"Link"}"#,
            r#"{"text":"リンク"}"#,
            PlaceholderProfile::FtbQuests,
        );
        assert!(!changed_json.is_valid);
    }

    #[test]
    fn protected_spans_report_raw_byte_ranges_in_source_order() {
        let source = "§aDeals %1$s to {0} and ${name}\\n§x§1§2§3§4§5§6end";
        let spans = extract_protected_spans(source, PlaceholderProfile::Lang);
        let raws: Vec<_> = spans.iter().map(|span| span.raw.as_str()).collect();
        assert_eq!(
            raws,
            [
                "§a",
                "%1$s",
                "{0}",
                "${name}",
                "\\n",
                // 16 進カラーは既存抽出器が拾わない 7 文字ぶんを 1 区間にまとめる。
                "§x§1§2§3§4§5§6",
            ]
        );
        // 範囲は原文と一致し、重ならない。
        let mut cursor = 0;
        for span in &spans {
            assert!(span.start >= cursor, "spans must not overlap");
            assert_eq!(&source[span.start..span.end], span.raw);
            cursor = span.end;
        }
    }

    #[test]
    fn protected_spans_cover_profile_specific_syntax() {
        let patchouli = extract_protected_spans("$(l)Stone#var#", PlaceholderProfile::Patchouli);
        assert_eq!(
            patchouli
                .iter()
                .map(|span| span.raw.as_str())
                .collect::<Vec<_>>(),
            ["$(l)", "#var#"]
        );

        let ftbquests = extract_protected_spans(
            "&aTitle &x&1&2&3&4&5&6 {@pagebreak}",
            PlaceholderProfile::FtbQuests,
        );
        assert_eq!(
            ftbquests
                .iter()
                .map(|span| span.raw.as_str())
                .collect::<Vec<_>>(),
            ["&a", "&x&1&2&3&4&5&6", "{@pagebreak}"]
        );
    }

    #[test]
    fn protected_spans_never_overlap_even_when_patterns_nest() {
        // `§x§1§2§3§4§5§6` の内側の `§1` 等は既存抽出器も拾う。区間が重なるとマスクの
        // 置換位置がずれるため、返る区間は必ず昇順かつ非重複でなければならない。
        for source in [
            "§x§1§2§3§4§5§6text",
            "§x§a§b§c§d§e§f§rreset",
            "&x&1&2&3&4&5&6text",
            "{@pagebreak}{count}{0}",
        ] {
            for profile in [
                PlaceholderProfile::Lang,
                PlaceholderProfile::Patchouli,
                PlaceholderProfile::FtbQuests,
            ] {
                let spans = extract_protected_spans(source, profile);
                let mut cursor = 0;
                for span in &spans {
                    assert!(span.start >= cursor, "{source} ({profile:?}) overlaps");
                    assert!(span.start < span.end, "{source} ({profile:?}) is empty");
                    assert_eq!(&source[span.start..span.end], span.raw);
                    cursor = span.end;
                }
                assert!(cursor <= source.len());
            }
        }
    }

    #[test]
    fn brace_name_spans_do_not_change_existing_extraction() {
        // 追加した `{name}` 分類は保護区間だけが使い、比較用の抽出結果は変えない。
        let source = "Value {count} and {0}";
        assert_eq!(
            extract_placeholders_for_profile(source, PlaceholderProfile::Lang)
                .iter()
                .map(|token| (token.kind, token.value.as_str()))
                .collect::<Vec<_>>(),
            [(PlaceholderKind::Numbered, "{0}")]
        );
        assert_eq!(
            extract_protected_spans(source, PlaceholderProfile::Lang)
                .iter()
                .map(|span| (span.kind, span.raw.as_str()))
                .collect::<Vec<_>>(),
            [
                (PlaceholderKind::BraceName, "{count}"),
                (PlaceholderKind::Numbered, "{0}"),
            ]
        );
    }
}
