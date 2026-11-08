//! Placeholder・書式コードのマスクと復元。
//!
//! `yc-validation` の比較関数は訳文の品質判定用で、`§` コードや `{0}`・改行・FTB の `&`
//! コードを Lang プロファイルでは比較しない。翻訳モデルへ渡す前にこれらを退避しないと、
//! モデルが記号を落としたり全角化したりしたまま出力し、後段の検証で初めて落ちる。
//! ここでは原文の保護区間をマーカーへ置き換えてから翻訳させ、復元時に原文の生テキストを
//! そのまま戻す。復元後は保護区間の多重集合が原文と一致することを厳密に確認する。

use std::collections::BTreeMap;

use yc_validation::{PlaceholderProfile, ProtectedSpan, extract_protected_spans};

/// 保護区間をどう表現するか。評価タスクが差し替えられるよう 1 箇所にまとめる。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProtectStrategy {
    /// マスクせず原文をそのまま送る。モデルが記号を保てる場合は最短のプロンプトになる。
    Verbatim,
    /// `⟦0⟧` 形式。ASCII でない珍しい括弧はモデルがそのまま複写しやすく、
    /// かつ Minecraft の翻訳キーや書式と衝突しない。トークン数も少ない。
    #[default]
    BracketedIndex,
    /// `__YC_PH_0000__` 形式。ASCII だけで構成され、トークナイザによっては
    /// 記号の連続より安定して 1 語として扱われる。
    AsciiMarker,
}

impl ProtectStrategy {
    /// 区間 `index` を置き換える文字列。`Verbatim` は置き換えない。
    fn marker(self, index: usize) -> Option<String> {
        match self {
            Self::Verbatim => None,
            Self::BracketedIndex => Some(format!("⟦{index}⟧")),
            Self::AsciiMarker => Some(format!("__YC_PH_{index:04}__")),
        }
    }

    fn delimiters(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Verbatim => None,
            Self::BracketedIndex => Some(("⟦", "⟧")),
            Self::AsciiMarker => Some(("__YC_PH_", "__")),
        }
    }

    /// 1 回だけ試す代替手段。マーカーが化けた場合は別の表記へ、
    /// 素通しで失敗した場合はマスクへ切り替える。
    pub fn alternative(self) -> Self {
        match self {
            Self::BracketedIndex => Self::AsciiMarker,
            Self::AsciiMarker => Self::BracketedIndex,
            Self::Verbatim => Self::BracketedIndex,
        }
    }
}

/// 復元時の失敗。呼び出し側は entry を失敗として扱い、代替 strategy で 1 回だけ再試行する。
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProtectError {
    /// マーカーが欠落・重複・改変され、保護区間の多重集合が原文と一致しない。
    #[error("protected spans do not match the source")]
    SpanMismatch,
    /// 原文に無いマーカーが出力に現れた。
    #[error("output contains an unknown placeholder marker")]
    UnknownMarker,
}

/// 原文の保護区間とマスク結果をまとめて保持する。
pub struct ProtectedSource {
    source: String,
    profile: PlaceholderProfile,
    strategy: ProtectStrategy,
    spans: Vec<ProtectedSpan>,
    masked: String,
}

impl ProtectedSource {
    pub fn new(source: &str, profile: PlaceholderProfile, strategy: ProtectStrategy) -> Self {
        let spans = extract_protected_spans(source, profile);
        let masked = mask(source, &spans, strategy);
        Self {
            source: source.to_owned(),
            profile,
            strategy,
            spans,
            masked,
        }
    }

    /// モデルへ渡す本文。
    pub fn masked_text(&self) -> &str {
        &self.masked
    }

    pub fn spans(&self) -> &[ProtectedSpan] {
        &self.spans
    }

    pub fn strategy(&self) -> ProtectStrategy {
        self.strategy
    }

    /// モデル出力から保護区間を原文どおりに戻し、厳密に検証する。
    pub fn restore(&self, model_output: &str) -> Result<String, ProtectError> {
        // 空白だけの原文は訳す本文が無く、先頭と末尾の空白が同じ範囲を指す。
        // そのまま返し、前後の空白を二重に付けない。
        if self.source.trim().is_empty() {
            return Ok(self.source.clone());
        }
        let trimmed = model_output.trim();
        let markers = scan_markers(trimmed, self.strategy);
        if markers
            .iter()
            .any(|(_, _, index)| *index >= self.spans.len())
        {
            return Err(ProtectError::UnknownMarker);
        }
        // 区間の多重集合だけを見ると、同じ生テキストを持つ 2 区間（例: `§a` が 2 つ）で
        // `⟦0⟧` の重複と `⟦1⟧` の欠落が相殺されて通ってしまう。どの区間が戻ったかは
        // 番号でしか区別できないので、期待する番号がちょうど 1 回ずつ現れることを先に要求する。
        if !markers_cover_every_span_once(&markers, self.spans.len(), self.strategy) {
            return Err(ProtectError::SpanMismatch);
        }

        let mut restored = String::with_capacity(trimmed.len() + 32);
        let mut cursor = 0;
        for (start, end, index) in markers {
            let span = &self.spans[index];
            restored.push_str(&trimmed[cursor..start]);
            restored.push_str(&span.raw);
            cursor = end;
        }
        restored.push_str(&trimmed[cursor..]);

        // 端の空白はモデル出力を trim した時点で失われているので、必ず原文から補う。
        // ただし端の保護区間（例: 先頭の `\n`）はマーカー復元で既に生テキストが戻って
        // いるため、その分だけ二重に足さない。区間は端の空白領域のどこにあってもよい
        // （` \nStone` の改行は byte 1 から始まる）ので、領域に収まる区間の生テキストを
        // すべて集め、復元済みの先頭・末尾から取り除いてから端の空白を付け直す。
        let lead = self.edge_whitespace(Edge::Leading);
        let trail = self.edge_whitespace(Edge::Trailing);
        let covered_lead = self.covered_edge_whitespace(Edge::Leading, lead.len());
        let covered_trail = self.covered_edge_whitespace(Edge::Trailing, trail.len());

        let body = restored
            .strip_prefix(covered_lead.as_str())
            .unwrap_or(&restored);
        let body = body.strip_suffix(covered_trail.as_str()).unwrap_or(body);

        let mut output = String::with_capacity(lead.len() + body.len() + trail.len());
        output.push_str(lead);
        output.push_str(body);
        output.push_str(trail);

        if span_multiset(&self.spans)
            != span_multiset(&extract_protected_spans(&output, self.profile))
        {
            return Err(ProtectError::SpanMismatch);
        }
        Ok(output)
    }

    /// 原文の先頭または末尾の空白。
    fn edge_whitespace(&self, edge: Edge) -> &str {
        match edge {
            Edge::Leading => {
                let length = self.source.len() - self.source.trim_start().len();
                &self.source[..length]
            }
            Edge::Trailing => {
                let length = self.source.len() - self.source.trim_end().len();
                &self.source[self.source.len() - length..]
            }
        }
    }

    /// 端の空白領域に収まっている保護区間の生テキストを、原文順に連結して返す。
    ///
    /// これが [`ProtectedSource::restore`] の復元結果の先頭（末尾）に現れる分なので、
    /// 端の空白を付け直す前に取り除く。区間が領域をまたぐ場合は、領域内の部分だけを数える。
    fn covered_edge_whitespace(&self, edge: Edge, edge_length: usize) -> String {
        if edge_length == 0 {
            return String::new();
        }
        let region_start = match edge {
            Edge::Leading => 0,
            Edge::Trailing => self.source.len() - edge_length,
        };
        let region_end = region_start + edge_length;
        let mut covered = String::new();
        for span in &self.spans {
            if span.end <= region_start || span.start >= region_end {
                continue;
            }
            let start = span.start.max(region_start);
            let end = span.end.min(region_end);
            covered.push_str(&self.source[start..end]);
        }
        covered
    }
}

/// 原文の端。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Edge {
    Leading,
    Trailing,
}

fn mask(source: &str, spans: &[ProtectedSpan], strategy: ProtectStrategy) -> String {
    let mut masked = String::with_capacity(source.len() + spans.len() * 8);
    let mut cursor = 0;
    for (index, span) in spans.iter().enumerate() {
        masked.push_str(&source[cursor..span.start]);
        match strategy.marker(index) {
            Some(marker) => masked.push_str(&marker),
            None => masked.push_str(&span.raw),
        }
        cursor = span.end;
    }
    masked.push_str(&source[cursor..]);
    masked
}

/// 出力中のマーカーを `(開始, 終了, 番号)` で返す。
///
/// 正規表現を足さずに済むよう、strategy ごとの区切り文字で走査する。数字として読めるだけ
/// では足りず、表記が正規形と完全に一致することを要求する。そうしないと `⟦00⟧` が `⟦0⟧`
/// として、`__YC_PH_0__` が `__YC_PH_0000__` として受理され、改変されたマーカーが
/// 厳密検証をすり抜けてしまう。
fn scan_markers(text: &str, strategy: ProtectStrategy) -> Vec<(usize, usize, usize)> {
    let Some((prefix, suffix)) = strategy.delimiters() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find(prefix) {
        let start = cursor + offset;
        let digits_start = start + prefix.len();
        let digit_length = text[digits_start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digit_length == 0 {
            cursor = digits_start;
            continue;
        }
        let after_digits = digits_start + digit_length;
        if !text[after_digits..].starts_with(suffix) {
            cursor = digits_start;
            continue;
        }
        let end = after_digits + suffix.len();
        let Ok(index) = text[digits_start..after_digits].parse::<usize>() else {
            cursor = end;
            continue;
        };
        if strategy
            .marker(index)
            .is_some_and(|canonical| canonical == text[start..end])
        {
            found.push((start, end, index));
        }
        cursor = end;
    }
    found
}

fn span_multiset(spans: &[ProtectedSpan]) -> BTreeMap<&str, usize> {
    let mut counts = BTreeMap::new();
    for span in spans {
        *counts.entry(span.raw.as_str()).or_insert(0) += 1;
    }
    counts
}

/// マーカー番号が保護区間をちょうど 1 回ずつ覆っているか。
///
/// マスクしない [`ProtectStrategy::Verbatim`] にはマーカーが無いため、この検査は行わない
/// （生テキストの多重集合比較だけで検証する）。
fn markers_cover_every_span_once(
    markers: &[(usize, usize, usize)],
    span_count: usize,
    strategy: ProtectStrategy,
) -> bool {
    if strategy.delimiters().is_none() {
        return true;
    }
    if markers.len() != span_count {
        return false;
    }
    let mut seen = vec![false; span_count];
    for (_, _, index) in markers {
        match seen.get_mut(*index) {
            Some(slot) if !*slot => *slot = true,
            // 範囲外、または同じ番号の 2 回目。
            _ => return false,
        }
    }
    seen.iter().all(|covered| *covered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore_ok(source: &str, profile: PlaceholderProfile, output: &str) -> String {
        ProtectedSource::new(source, profile, ProtectStrategy::BracketedIndex)
            .restore(output)
            .expect("restore should succeed")
    }

    #[test]
    fn minecraft_formatting_codes_are_masked_and_restored_verbatim() {
        let protected = ProtectedSource::new(
            "§aGreen §rStone",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(protected.masked_text(), "⟦0⟧Green ⟦1⟧Stone");
        assert_eq!(protected.spans().len(), 2);
        // モデルがコードを落としても、マーカーさえ残っていれば原文のコードが戻る。
        assert_eq!(protected.restore("⟦0⟧緑の⟦1⟧石").unwrap(), "§a緑の§r石");
    }

    #[test]
    fn printf_numbered_named_and_escape_spans_are_protected() {
        let source = "Deals %1$s damage to {0} and ${name}\\nnext line";
        let protected = ProtectedSource::new(
            source,
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        let spans: Vec<_> = protected
            .spans()
            .iter()
            .map(|span| span.raw.as_str())
            .collect();
        assert_eq!(spans, ["%1$s", "{0}", "${name}", "\\n"]);
        assert_eq!(
            protected
                .restore("⟦0⟧ダメージを⟦1⟧と⟦2⟧に与える⟦3⟧次の行")
                .unwrap(),
            "%1$sダメージを{0}と${name}に与える\\n次の行"
        );
    }

    #[test]
    fn patchouli_macros_and_template_variables_are_protected() {
        let source = "$(l)Stone$(reset) #item.name#";
        let protected = ProtectedSource::new(
            source,
            PlaceholderProfile::Patchouli,
            ProtectStrategy::BracketedIndex,
        );
        let spans: Vec<_> = protected
            .spans()
            .iter()
            .map(|span| span.raw.as_str())
            .collect();
        assert_eq!(spans, ["$(l)", "$(reset)", "#item.name#"]);
        assert_eq!(
            protected.restore("⟦0⟧石⟦1⟧ ⟦2⟧").unwrap(),
            "$(l)石$(reset) #item.name#"
        );
    }

    #[test]
    fn ftbquests_codes_tags_and_json_are_protected() {
        let source = r#"&aTitle {@pagebreak} {"text":"Link"}"#;
        let protected = ProtectedSource::new(
            source,
            PlaceholderProfile::FtbQuests,
            ProtectStrategy::BracketedIndex,
        );
        let spans: Vec<_> = protected
            .spans()
            .iter()
            .map(|span| span.raw.as_str())
            .collect();
        assert_eq!(spans, ["&a", "{@pagebreak}", r#"{"text":"Link"}"#]);
        assert_eq!(
            protected.restore("⟦0⟧タイトル ⟦1⟧ ⟦2⟧").unwrap(),
            r#"&aタイトル {@pagebreak} {"text":"Link"}"#
        );
    }

    #[test]
    fn duplicated_and_missing_markers_are_rejected_even_when_raw_spans_match() {
        // `§a` が 2 つある原文。生テキストの多重集合は同じなので、番号を見ないと
        // `⟦0⟧` の重複と `⟦1⟧` の欠落が相殺されて通ってしまう。
        let protected = ProtectedSource::new(
            "§aStone§a",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(protected.masked_text(), "⟦0⟧Stone⟦1⟧");
        assert_eq!(protected.restore("⟦0⟧石⟦1⟧").unwrap(), "§a石§a");
        assert_eq!(
            protected.restore("⟦0⟧石⟦0⟧"),
            Err(ProtectError::SpanMismatch)
        );
        assert_eq!(protected.restore("⟦0⟧石"), Err(ProtectError::SpanMismatch));
        assert_eq!(
            protected.restore("⟦1⟧石⟦0⟧"),
            Ok("§a石§a".to_owned()),
            "order may change but every index must appear exactly once"
        );
    }

    #[test]
    fn real_newlines_are_protected_and_leading_whitespace_is_not_duplicated() {
        let source = "\nStone\n";
        let protected = ProtectedSource::new(
            source,
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        // 端の改行も保護区間なので、原文の生テキストが復元で戻る。
        assert_eq!(protected.masked_text(), "⟦0⟧Stone⟦1⟧");
        assert_eq!(protected.restore("⟦0⟧石⟦1⟧").unwrap(), "\n石\n");
    }

    #[test]
    fn edge_whitespace_after_an_edge_span_is_restored() {
        // 先頭の改行が保護区間 (byte 0) でも、その後に続く空白は区間の外なので
        // 原文から補わなければならない。以前は区間の開始位置で打ち切って失っていた。
        let protected = ProtectedSource::new(
            "\n  Stone",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(protected.masked_text(), "⟦0⟧  Stone");
        assert_eq!(protected.restore("⟦0⟧石").unwrap(), "\n  石");

        // 末尾側も同じ。末尾の改行が区間で、その前の空白は区間の外。
        let trailing = ProtectedSource::new(
            "Stone  \n",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(trailing.masked_text(), "Stone  ⟦0⟧");
        assert_eq!(trailing.restore("石⟦0⟧").unwrap(), "石  \n");
    }

    #[test]
    fn protected_span_anywhere_in_the_edge_whitespace_is_not_duplicated() {
        // 改行が byte 1 から始まる（先頭は通常の空白）。区間は端に接していないが
        // 端の空白領域の内側にあるので、付け直す前に取り除かないと重複する。
        let protected = ProtectedSource::new(
            " \nStone",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(protected.masked_text(), " ⟦0⟧Stone");
        assert_eq!(protected.restore(" ⟦0⟧石").unwrap(), " \n石");

        // 末尾側の対称ケース。
        let trailing = ProtectedSource::new(
            "Stone\n ",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(trailing.masked_text(), "Stone⟦0⟧ ");
        assert_eq!(trailing.restore("石⟦0⟧ ").unwrap(), "石\n ");

        // 両端が混在する場合も、前後の空白がちょうど 1 回ずつ戻る。
        let both = ProtectedSource::new(
            " \nStone\n ",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(both.restore("⟦0⟧石⟦1⟧").unwrap(), " \n石\n ");
    }

    #[test]
    fn noncanonical_marker_spellings_are_rejected() {
        let protected = ProtectedSource::new(
            "§aStone §r",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        // 数字として読めば同じ番号でも、表記が正規形と違えば改変とみなす。
        for altered in ["⟦00⟧石⟦1⟧", "⟦0⟧石⟦01⟧", "⟦+0⟧石⟦1⟧"] {
            assert_eq!(
                protected.restore(altered),
                Err(ProtectError::SpanMismatch),
                "{altered}"
            );
        }

        let ascii = ProtectedSource::new(
            "%s Stone",
            PlaceholderProfile::Lang,
            ProtectStrategy::AsciiMarker,
        );
        for altered in ["__YC_PH_0__ 石", "__YC_PH_00000__ 石"] {
            assert_eq!(
                ascii.restore(altered),
                Err(ProtectError::SpanMismatch),
                "{altered}"
            );
        }
        assert_eq!(ascii.restore("__YC_PH_0000__ 石").unwrap(), "%s 石");
    }

    #[test]
    fn unprotected_leading_and_trailing_whitespace_is_reapplied() {
        let protected = ProtectedSource::new(
            " Stone ",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(protected.masked_text(), " Stone ");
        assert_eq!(protected.restore("  石  ").unwrap(), " 石 ");
    }

    #[test]
    fn missing_duplicated_or_unknown_markers_fail_strict_verification() {
        let protected = ProtectedSource::new(
            "§aStone §r",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        // 欠落
        assert_eq!(protected.restore("⟦0⟧石"), Err(ProtectError::SpanMismatch));
        // 重複
        assert_eq!(
            protected.restore("⟦0⟧石⟦0⟧⟦1⟧"),
            Err(ProtectError::SpanMismatch)
        );
        // 原文に無い番号
        assert_eq!(
            protected.restore("⟦0⟧石⟦1⟧⟦7⟧"),
            Err(ProtectError::UnknownMarker)
        );
    }

    #[test]
    fn verbatim_strategy_still_verifies_the_raw_codes() {
        let protected = ProtectedSource::new(
            "§aStone",
            PlaceholderProfile::Lang,
            ProtectStrategy::Verbatim,
        );
        assert_eq!(protected.masked_text(), "§aStone");
        assert_eq!(protected.restore("§a石").unwrap(), "§a石");
        // モデルがコードを落とした場合は失敗し、マスク戦略へ切り替える判断材料になる。
        assert_eq!(protected.restore("石"), Err(ProtectError::SpanMismatch));
    }

    #[test]
    fn ascii_marker_round_trips_and_rejects_foreign_numbers() {
        let protected = ProtectedSource::new(
            "%s Stone",
            PlaceholderProfile::Lang,
            ProtectStrategy::AsciiMarker,
        );
        assert_eq!(protected.masked_text(), "__YC_PH_0000__ Stone");
        assert_eq!(protected.restore("__YC_PH_0000__ 石").unwrap(), "%s 石");
        assert_eq!(
            protected.restore("__YC_PH_0009__ 石"),
            Err(ProtectError::UnknownMarker)
        );
    }

    #[test]
    fn alternative_strategies_never_repeat_themselves() {
        for strategy in [
            ProtectStrategy::Verbatim,
            ProtectStrategy::BracketedIndex,
            ProtectStrategy::AsciiMarker,
        ] {
            assert_ne!(strategy, strategy.alternative());
        }
    }

    #[test]
    fn text_without_protected_spans_is_only_trimmed() {
        assert_eq!(
            restore_ok("Stone", PlaceholderProfile::Lang, "  石  "),
            "石"
        );
        // 原文に無い書式をモデルが足したら失敗させる。
        let protected = ProtectedSource::new(
            "Stone",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(protected.restore("石 %s"), Err(ProtectError::SpanMismatch));
    }

    #[test]
    fn whitespace_only_source_is_returned_unchanged() {
        // 前後の空白が本文そのものなので、原文をそのまま返す（二重にしない）。
        let protected = ProtectedSource::new(
            "  ",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(protected.restore("  ").unwrap(), "  ");
        assert_eq!(protected.restore("").unwrap(), "  ");
    }
}
