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

/// マスク後の本文に現れるマーカー風の並び 1 つ分の役割。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkerRole {
    /// 保護区間を置き換えた生成マーカー。
    Generated,
    /// 原文に元からあった表記。
    Literal,
}

/// 原文の保護区間とマスク結果をまとめて保持する。
pub struct ProtectedSource {
    source: String,
    profile: PlaceholderProfile,
    strategy: ProtectStrategy,
    spans: Vec<ProtectedSpan>,
    masked: String,
    /// マーカー表記ごとの、マスク後の本文での役割の並び（出現順）。
    ///
    /// 原文が `⟦0⟧` を通常テキストとして含むことがある。生成マーカーと同じ表記になり得る
    /// ので、テキストだけでは区別できない。マスク後の並び順を記録し、復元時は同じ表記の
    /// 何番目の出現かを数えて役割を引く。こうしないと、生成マーカーと原文の表記を
    /// 取り違えて書式コードが別の位置へ移動する。
    roles: BTreeMap<String, Vec<MarkerRole>>,
    /// 原文に現れるマーカー風の不正列の出現数。
    ///
    /// 生成マーカーは常に正規形なので、不正列はすべて原文由来。個数を数えて消費する。
    malformed_literals: BTreeMap<String, usize>,
}

impl ProtectedSource {
    pub fn new(source: &str, profile: PlaceholderProfile, strategy: ProtectStrategy) -> Self {
        let spans = extract_protected_spans(source, profile);
        let (masked, roles, malformed_literals) = mask(source, &spans, strategy);
        Self {
            source: source.to_owned(),
            profile,
            strategy,
            spans,
            masked,
            roles,
            malformed_literals,
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
        let scan = scan_markers(trimmed, self.strategy);

        // マーカー表記ごとに「何番目の出現が生成マーカーか」を引く。同じ表記が原文にも
        // 生成マーカーにも現れ得るため、テキストだけでは区別できない。マスク時に記録した
        // 並び順（原文の並び + 生成マーカーの挿入位置）を使う。
        //
        // 出現順で対応づけるので、`§aUse ⟦0⟧ here` のように生成マーカーが原文の表記より
        // 前に来る場合でも、`§a` が `Use` の後ろへ移動しない。
        //
        // ただし対応づけは「出力が同じ表記を同じ個数だけ保っている」ことが前提になる。
        // 個数が違うと、どちらを落としたのか決められない（生成マーカーの欠落を原文の表記で
        // 埋めてしまい、書式コードが黙って移動する）。その場合は対応づけを諦めて失敗させる。
        let mut role_cursor: BTreeMap<String, usize> = BTreeMap::new();
        let mut markers = Vec::with_capacity(scan.markers.len());

        for (start, end, index) in scan.markers {
            let text = trimmed[start..end].to_owned();
            let ordinal = role_cursor.entry(text.clone()).or_insert(0);
            let roles = self.roles.get(&text);
            let role = roles.and_then(|roles| roles.get(*ordinal)).copied();
            *ordinal += 1;
            // 原文に無い表記・原文の出現数を超えた分は生成マーカーとして扱う。
            match role {
                Some(MarkerRole::Literal) => {}
                Some(MarkerRole::Generated) | None => markers.push((start, end, index)),
            }
        }
        // 表記ごとの出現数がマスク後と食い違っていれば、対応づけは信用できない。
        // ただし原文由来の表記を含む表記だけを見る。生成マーカーだけの表記は、個数の
        // 過不足を `markers_cover_every_span_once` が SpanMismatch として扱うので、
        // ここで先に落とすと失敗理由が変わってしまう。
        // マーカー風だが正規形でない並びは、原文由来のものだけを本文として許す。
        let mut malformed_literals = self.malformed_literals.clone();
        for (start, end) in scan.malformed {
            let text = trimmed[start..end].to_owned();
            match malformed_literals.get_mut(&text) {
                Some(remaining) if *remaining > 0 => *remaining -= 1,
                _ => return Err(ProtectError::UnknownMarker),
            }
        }
        if markers
            .iter()
            .any(|(_, _, index)| *index >= self.spans.len())
        {
            return Err(ProtectError::UnknownMarker);
        }
        for (text, roles) in &self.roles {
            if !roles.contains(&MarkerRole::Literal) {
                continue;
            }
            let found = role_cursor.get(text).copied().unwrap_or(0);
            if found != roles.len() {
                return Err(ProtectError::SpanMismatch);
            }
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
        // 復元済みの本文の先頭・末尾に残っている空白は、(a) 端の保護区間として戻した
        // 生テキスト、(b) モデルが足した余分な空白、のどちらかしかない。原文の端は
        // 定義上すべて空白なので、どちらも捨てて原文の端の空白を付け直すのが正しい。
        //
        // 以前は「端の領域に収まる区間の生テキスト」だけを集めて取り除いていたため、
        // 区間と区間のあいだにある保護されない空白（`\n \nStone` の中央の空白）を
        // 取りこぼし、端の空白を二重に付けていた。
        let lead = self.edge_whitespace(Edge::Leading);
        let trail = self.edge_whitespace(Edge::Trailing);
        let body = restored.trim();

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

    /// 端の空白領域に収まっている保護区間の生テキストを、原文順に連結して返す。
    ///
    /// これが [`ProtectedSource::restore`] の復元結果の先頭（末尾）に現れる分なので、
    /// 端の空白を付け直す前に取り除く。区間が領域をまたぐ場合は、領域内の部分だけを数える。
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
}

/// 原文の端。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Edge {
    Leading,
    Trailing,
}

/// 原文をマスクし、マーカー表記ごとの役割の並びを返す。
///
/// 戻り値は `(マスク後本文, 表記ごとの役割, 不正列の原文出現数)`。原文の区間外の文字列は
/// そのまま残るので、そこに `⟦0⟧` が含まれていれば「原文由来」として記録する。生成
/// マーカーは区間の位置に挿入されるため、両者はマスク後の並び順で区別できる。
fn mask(
    source: &str,
    spans: &[ProtectedSpan],
    strategy: ProtectStrategy,
) -> (
    String,
    BTreeMap<String, Vec<MarkerRole>>,
    BTreeMap<String, usize>,
) {
    let mut masked = String::with_capacity(source.len() + spans.len() * 8);
    let mut roles: BTreeMap<String, Vec<MarkerRole>> = BTreeMap::new();
    let mut cursor = 0;
    for (index, span) in spans.iter().enumerate() {
        // 区間の手前は原文のまま残る。ここに現れるマーカー風の並びは原文由来。
        push_verbatim(
            &mut masked,
            &mut roles,
            &source[cursor..span.start],
            strategy,
        );
        match strategy.marker(index) {
            Some(marker) => {
                roles
                    .entry(marker.clone())
                    .or_default()
                    .push(MarkerRole::Generated);
                masked.push_str(&marker);
            }
            // `Verbatim` は置き換えない。原文の生テキストは本文の一部として残る。
            None => push_verbatim(&mut masked, &mut roles, &span.raw, strategy),
        }
        cursor = span.end;
    }
    push_verbatim(&mut masked, &mut roles, &source[cursor..], strategy);

    // 不正列は生成マーカーになり得ない（生成マーカーは常に正規形）。原文由来として数える。
    let mut malformed_literals: BTreeMap<String, usize> = BTreeMap::new();
    for (start, end) in scan_markers(source, strategy).malformed {
        *malformed_literals
            .entry(source[start..end].to_owned())
            .or_insert(0) += 1;
    }
    (masked, roles, malformed_literals)
}

/// 原文の断片をそのまま積み、含まれるマーカー風の並びを「原文由来」として記録する。
fn push_verbatim(
    masked: &mut String,
    roles: &mut BTreeMap<String, Vec<MarkerRole>>,
    fragment: &str,
    strategy: ProtectStrategy,
) {
    for (start, end, _) in scan_markers(fragment, strategy).markers {
        roles
            .entry(fragment[start..end].to_owned())
            .or_default()
            .push(MarkerRole::Literal);
    }
    masked.push_str(fragment);
}

/// 出力中のマーカー走査結果。
struct MarkerScan {
    /// 正規形のマーカー `(開始, 終了, 番号)`。
    markers: Vec<(usize, usize, usize)>,
    /// マーカー風だが正規形でない並び `(開始, 終了)`。
    ///
    /// これがあると、モデルがマーカーを書き損じたまま本文へ残した状態になる。番号として
    /// 読めない・桁が正規形と違う・閉じ記号が無い、のいずれもここに入れる。呼び出し側は
    /// 原文に同じ並びが無いことを確かめてから失敗させる（原文の本文なら正当な文字列）。
    malformed: Vec<(usize, usize)>,
}

/// 出力中のマーカーを走査する。
///
/// 正規表現を足さずに済むよう、strategy ごとの区切り文字で走査する。数字として読めるだけ
/// では足りず、表記が正規形と完全に一致することを要求する。そうしないと `⟦00⟧` が `⟦0⟧`
/// として、`__YC_PH_0__` が `__YC_PH_0000__` として受理され、改変されたマーカーが
/// 厳密検証をすり抜けてしまう。
///
/// 正規形に一致しない並びは黙って読み飛ばさず [`MarkerScan::malformed`] へ入れる。読み飛ばすと
/// `⟦0⟧石⟦x⟧` のように壊れたマーカーが本文に残ったまま検証を通ってしまう。
fn scan_markers(text: &str, strategy: ProtectStrategy) -> MarkerScan {
    let Some((prefix, suffix)) = strategy.delimiters() else {
        return MarkerScan {
            markers: Vec::new(),
            malformed: Vec::new(),
        };
    };
    let mut scan = MarkerScan {
        markers: Vec::new(),
        malformed: Vec::new(),
    };
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find(prefix) {
        let start = cursor + offset;
        let digits_start = start + prefix.len();
        let digit_length = text[digits_start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digit_length == 0 {
            // 開き記号の直後が数字でない。閉じ記号があればそこまで、無ければ次の開き記号の
            // 手前（それも無ければ末尾）までを壊れたマーカーとして記録する。
            //
            // 閉じ記号が無いときに開き記号だけを記録してはいけない。照合は「原文に同じ
            // 並びがあるか」で行うため、原文に `⟦` が含まれると `⟦x` が素通りし、壊れた
            // マーカーが本文に残ったまま検証を通ってしまう。
            let end = text[digits_start..]
                .find(suffix)
                .map(|found| digits_start + found + suffix.len())
                .or_else(|| {
                    text[digits_start..]
                        .find(prefix)
                        .map(|found| digits_start + found)
                })
                .unwrap_or(text.len());
            scan.malformed.push((start, end));
            cursor = digits_start;
            continue;
        }
        let after_digits = digits_start + digit_length;
        if !text[after_digits..].starts_with(suffix) {
            // 数字の後に閉じ記号が無い。次の開き記号の手前までを壊れたマーカーとする。
            let end = text[after_digits..]
                .find(prefix)
                .map_or(text.len(), |found| after_digits + found);
            scan.malformed.push((start, end));
            cursor = after_digits;
            continue;
        }
        let end = after_digits + suffix.len();
        let index = text[digits_start..after_digits].parse::<usize>().ok();
        let canonical = index
            .and_then(|index| strategy.marker(index).map(|marker| (index, marker)))
            .is_some_and(|(_, canonical)| canonical == text[start..end]);
        match index {
            Some(index) if canonical => scan.markers.push((start, end, index)),
            _ => scan.malformed.push((start, end)),
        }
        cursor = end;
    }
    scan
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
        // 数字として読めば同じ番号でも、表記が正規形と違えば改変とみなす。改変された
        // マーカーは「原文に無いマーカー風の並び」なので UnknownMarker として弾く。
        for altered in ["⟦00⟧石⟦1⟧", "⟦0⟧石⟦01⟧", "⟦+0⟧石⟦1⟧"] {
            assert_eq!(
                protected.restore(altered),
                Err(ProtectError::UnknownMarker),
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
                Err(ProtectError::UnknownMarker),
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
    fn unprotected_whitespace_between_edge_spans_is_restored_exactly_once() {
        // 先頭から `\n`、空白、`\n` の順に並ぶ。中央の空白は保護区間の外なので、
        // 区間の生テキストだけを集めると取りこぼし、端の空白を付け直したときに重複していた。
        let protected = ProtectedSource::new(
            "\n \nStone",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(protected.masked_text(), "⟦0⟧ ⟦1⟧Stone");
        assert_eq!(protected.restore("⟦0⟧ ⟦1⟧石").unwrap(), "\n \n石");
        // モデルが区間のあいだの空白を落としても、原文の端の空白はそのまま戻る。
        assert_eq!(protected.restore("⟦0⟧⟦1⟧石").unwrap(), "\n \n石");

        // 末尾側の対称ケース。
        let trailing = ProtectedSource::new(
            "Stone\n \n",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(trailing.masked_text(), "Stone⟦0⟧ ⟦1⟧");
        assert_eq!(trailing.restore("石⟦0⟧⟦1⟧").unwrap(), "石\n \n");

        // 3 区間が端の空白領域に並ぶ場合も、原文の端がちょうど 1 回ずつ戻る。
        let three = ProtectedSource::new(
            "\n\n \nStone",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(three.restore("⟦0⟧⟦1⟧⟦2⟧石").unwrap(), "\n\n \n石");
    }

    #[test]
    fn malformed_marker_like_sequences_fail_the_restore() {
        let protected = ProtectedSource::new(
            "§aStone",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        // `⟦x⟧` は正規形でない。読み飛ばすと壊れたマーカーが本文に残ったまま通ってしまう。
        for altered in ["⟦0⟧石⟦x⟧", "⟦0⟧石⟦⟧", "⟦0⟧石⟦0", "⟦0⟧石⟦ 0⟧"]
        {
            assert_eq!(
                protected.restore(altered),
                Err(ProtectError::UnknownMarker),
                "{altered}"
            );
        }

        let ascii = ProtectedSource::new(
            "%s Stone",
            PlaceholderProfile::Lang,
            ProtectStrategy::AsciiMarker,
        );
        for altered in [
            "__YC_PH_0000__ 石__YC_PH_x__",
            "__YC_PH_0000__ 石__YC_PH___",
            "__YC_PH_0000__ 石__YC_PH_0000_",
        ] {
            assert_eq!(
                ascii.restore(altered),
                Err(ProtectError::UnknownMarker),
                "{altered}"
            );
        }

        // 原文に同じ並びがある場合は、本文として書かれた正当な文字列なので拒否しない。
        let literal = ProtectedSource::new(
            "Use ⟦x⟧ as a marker",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(
            literal
                .restore("マーカーとして⟦x⟧を使ってください")
                .unwrap(),
            "マーカーとして⟦x⟧を使ってください"
        );

        // 原文に開き記号だけが含まれる場合、閉じ記号の無い壊れたマーカーを「原文にある
        // から正当」と誤認してはいけない。以前は開き記号 1 文字だけを記録していたため、
        // この出力が通って `⟦x` が本文に残っていた。
        let open_bracket = ProtectedSource::new(
            "§aStone ⟦",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(
            open_bracket.restore("⟦0⟧石 ⟦x"),
            Err(ProtectError::UnknownMarker)
        );
        // 閉じ記号の無い壊れたマーカーは、次の開き記号の手前までを 1 つの不正列として扱う。
        assert_eq!(
            open_bracket.restore("⟦0⟧石 ⟦x ⟦"),
            Err(ProtectError::UnknownMarker)
        );
    }

    #[test]
    fn marker_text_that_exists_in_the_source_is_not_treated_as_generated() {
        // 原文が `⟦0⟧` を通常テキストとして説明している場合。保護区間が無いので、
        // 生成マーカーとして数えると `UnknownMarker` で落ちてしまう。
        let literal = ProtectedSource::new(
            "Use ⟦0⟧ as a marker",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(
            literal.restore("⟦0⟧をマーカーとして使う").unwrap(),
            "⟦0⟧をマーカーとして使う"
        );
        // 原文に無いマーカーは従来どおり拒否する。
        assert_eq!(
            literal.restore("⟦1⟧をマーカーとして使う"),
            Err(ProtectError::UnknownMarker)
        );

        // 原文に `⟦0⟧` と保護区間の両方がある場合。マスク後は `⟦0⟧` が 2 つ並ぶが、
        // 1 つは原文由来なので、生成マーカーの重複とみなしてはいけない。
        //
        // 先頭の `⟦0⟧` は生成マーカー（`§a` の位置）、2 つ目が原文の表記。出現順で
        // 対応づけるので、`§a` は `Use` の手前に戻る（後ろへ移動しない）。
        let both = ProtectedSource::new(
            "§aUse ⟦0⟧ here",
            PlaceholderProfile::Lang,
            ProtectStrategy::BracketedIndex,
        );
        assert_eq!(both.masked_text(), "⟦0⟧Use ⟦0⟧ here");
        assert_eq!(
            both.restore("⟦0⟧Use ⟦0⟧ をここで").unwrap(),
            "§aUse ⟦0⟧ をここで"
        );
        // 生成マーカーが欠落した場合は、原文由来の `⟦0⟧` があっても検出する。
        assert_eq!(
            both.restore("Use ⟦0⟧ をここで"),
            Err(ProtectError::SpanMismatch)
        );

        // ASCII マーカーでも同じ。
        let ascii = ProtectedSource::new(
            "Use __YC_PH_0000__ literally",
            PlaceholderProfile::Lang,
            ProtectStrategy::AsciiMarker,
        );
        assert_eq!(
            ascii.restore("__YC_PH_0000__をそのまま使う").unwrap(),
            "__YC_PH_0000__をそのまま使う"
        );
        let ascii_both = ProtectedSource::new(
            "%s uses __YC_PH_0000__",
            PlaceholderProfile::Lang,
            ProtectStrategy::AsciiMarker,
        );
        // 先頭が生成マーカー（`%s` の位置）、2 つ目が原文の表記。`%s` は先頭に戻る。
        assert_eq!(
            ascii_both
                .restore("__YC_PH_0000__ uses __YC_PH_0000__")
                .unwrap(),
            "%s uses __YC_PH_0000__"
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
