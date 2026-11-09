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
    /// `<x0>…</x0>` 形式。対になった書式コードを開き／閉じのタグとして表し、
    /// 挟まれた本文が「訳す対象」であることをモデルへ示す。
    ///
    /// 実測: `$(item)heat$()` のような Patchouli マクロでは、`⟦0⟧heat⟦1⟧` だと
    /// モデルが `heat` を原文のまま複写する（未訳のまま残る）。開き／閉じの形にすると
    /// 内側を本文として訳す率が上がる。
    PairedTag,
}

impl ProtectStrategy {
    /// 区間 `index` を置き換える文字列。`Verbatim` は置き換えない。
    fn marker(self, index: usize) -> Option<String> {
        self.marker_text(index, PairRole::Single)
    }

    /// 区間 `index` を、対の役割 `role` に応じた表記へ置き換える文字列。
    fn marker_text(self, index: usize, role: PairRole) -> Option<String> {
        match self {
            Self::Verbatim => None,
            Self::BracketedIndex => Some(format!("⟦{index}⟧")),
            Self::AsciiMarker => Some(format!("__YC_PH_{index:04}__")),
            Self::PairedTag => Some(match role {
                PairRole::Open => format!("<x{index}>"),
                PairRole::Close => format!("</x{index}>"),
                PairRole::Single => format!("<x{index}/>"),
            }),
        }
    }

    fn delimiters(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Verbatim | Self::PairedTag => None,
            Self::BracketedIndex => Some(("⟦", "⟧")),
            Self::AsciiMarker => Some(("__YC_PH_", "__")),
        }
    }

    /// 保護区間をマーカーへ置き換えるか。`Verbatim` だけが置き換えない。
    fn masks(self) -> bool {
        !matches!(self, Self::Verbatim)
    }

    /// 1 回だけ試す代替手段。マーカーが化けた場合は別の表記へ、
    /// 素通しで失敗した場合はマスクへ切り替える。
    pub fn alternative(self) -> Self {
        match self {
            Self::BracketedIndex => Self::AsciiMarker,
            Self::AsciiMarker => Self::BracketedIndex,
            Self::Verbatim | Self::PairedTag => Self::BracketedIndex,
        }
    }

    /// profile ごとの既定。
    ///
    /// 実測（2B GPU、書式コードに挟まれた本文を持つ 24 entry・38 区間）で、
    /// 未訳のまま残った本文の率は
    /// `BracketedIndex` 2.6% / `PairedTag` 2.6% / `Verbatim` 18.4% / `AsciiMarker` 26.3%。
    /// 内訳が違う:
    ///
    /// - Patchouli（`$(item)heat$()`）は `BracketedIndex` だと `heat` を原文のまま複写する
    ///   （7 件中 1 件が未訳）。`PairedTag` は 0 件で、`$(item)熱$()` と訳す。
    /// - Minecraft の `§7`（`§7(Hidden Advancement)`）は `PairedTag` だと
    ///   `Hidden Advancement` を残す（17 件中 1 件）。`BracketedIndex` は 0 件。
    ///
    /// どちらも 1 件差なので率だけでは決められないが、chrF は `PairedTag` が最も高い
    /// （0.4532 対 0.4426）。Patchouli の本文は長く、訳す語が多いので chrF への寄与が
    /// 大きい。`§` コードは行末まで効く短いラベルが中心で、`PairedTag` の開き／閉じが
    /// 本文の範囲を誤って示すことがある。
    ///
    /// したがって Patchouli は `PairedTag`、それ以外は `BracketedIndex` を既定にする。
    pub fn for_profile(profile: PlaceholderProfile) -> Self {
        match profile {
            PlaceholderProfile::Patchouli => Self::PairedTag,
            PlaceholderProfile::Lang | PlaceholderProfile::FtbQuests => Self::BracketedIndex,
        }
    }
}

/// 対になった書式コードでの、保護区間 1 つ分の役割。
///
/// [`ProtectStrategy::PairedTag`] だけが使う。開き側と閉じ側を別の表記にすると、
/// 挟まれた本文が「訳す対象」であることがモデルへ伝わる。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairRole {
    /// 開き側。`<x0>` で表す。
    Open,
    /// 閉じ側。`</x0>` で表す。
    Close,
    /// 対になっていない区間。`<x0/>` で表す。
    Single,
}

/// 閉じ側の書式コードかどうか。
///
/// Patchouli のマクロは `$(item)…$()` の形で閉じる。Minecraft / FTB の書式コードは
/// `§r` / `&r`（リセット）が閉じ側。
///
/// 評価ハーネスも同じ規則で「書式コードに挟まれた本文」を切り出す。規則を 2 箇所に
/// 書くと、測っている対象と製品が保護している対象がずれる。
pub fn is_closing_code(raw: &str) -> bool {
    raw == "$()" || raw.eq_ignore_ascii_case("§r") || raw.eq_ignore_ascii_case("&r")
}

/// 閉じ側と組になる開き側かどうか。
///
/// 開き側として扱うのは**実際の書式コードだけ**。`$(…)` マクロと `§` / `&` の装飾
/// コードが対象で、次のものは開き側にしない:
///
/// - `$(br)`（改行）と `$(k:…)`（キーバインド参照）。実データ（Ars Nouveau の
///   Patchouli 本文 1,200 entry）で `$()` と組になるのは装飾マクロだけで、この 2 つは
///   閉じない。開き側として積むと後続の `$()` が別のマクロの閉じ側と組になり、
///   挟まれた本文が誤って「対の中」と判定される。
/// - `#item.name#` のようなテンプレート変数。保護区間ではあるが書式コードではない。
///   開き側にすると `$(item)heat#item.name#$()` で `$(item)` が単独扱いになり、
///   `heat` が対タグの外に残って訳されなくなる。
/// - `{0}` / `%s` / `\n` などの placeholder。書式コードではない。
pub fn is_opening_code(raw: &str) -> bool {
    if raw.starts_with("$(") {
        // 閉じないマクロ。実データに現れるのはこの 2 種類だけ。
        return raw != "$(br)" && !raw.starts_with("$(k:");
    }
    // `§` / `&` の装飾コードだけを開き側にする。`§r` / `&r` は閉じ側なので除く。
    is_decoration_code(raw) && !is_closing_code(raw)
}

/// `§` / `&` の装飾コード（`§a` / `&7` など）かどうか。
///
/// `§x§1§2…` の 16 進カラーは抽出時に 1 区間にまとまるが、先頭が `§` + 英数字なので
/// ここでも装飾コードとして扱う。
fn is_decoration_code(raw: &str) -> bool {
    let mut characters = raw.chars();
    matches!(characters.next(), Some('§' | '&'))
        && characters
            .next()
            .is_some_and(|code| code.is_ascii_alphanumeric())
}

/// 保護区間ごとの対の役割を求める。
///
/// 閉じ側は直前の対になっていない開き側と組にする。組にできなかった区間は
/// [`PairRole::Single`] のままにする。復元時はこの役割と、出力に現れたタグの表記
/// （開き／閉じ／単独）が一致することを要求する。一致を見ないと、モデルが
/// `</x0>…<x1>` のように開きと閉じを入れ替えた出力でも「番号が揃っている」という
/// 理由で通り、`$(item)` と `$()` が入れ替わったまま復元される。
fn pair_roles(spans: &[ProtectedSpan]) -> Vec<PairRole> {
    let mut roles = vec![PairRole::Single; spans.len()];
    let mut open: Vec<usize> = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        if is_closing_code(&span.raw) {
            if let Some(opener) = open.pop() {
                roles[opener] = PairRole::Open;
                roles[index] = PairRole::Close;
            }
        } else if is_opening_code(&span.raw) {
            open.push(index);
        }
    }
    roles
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
    /// 保護区間ごとの、[`ProtectStrategy::PairedTag`] での表記の役割。
    ///
    /// 復元時に「出力に現れたタグの役割」と突き合わせる。番号だけを数えると、
    /// `</x0>…<x1>` のように開きと閉じを入れ替えた出力でも通ってしまい、
    /// `$(item)` と `$()` が入れ替わったまま復元される。
    pair_roles: Vec<PairRole>,
}

impl ProtectedSource {
    pub fn new(source: &str, profile: PlaceholderProfile, strategy: ProtectStrategy) -> Self {
        let spans = extract_protected_spans(source, profile);
        let pair_roles = pair_roles(&spans);
        let (masked, roles, malformed_literals) = mask(source, &spans, &pair_roles, strategy);
        Self {
            source: source.to_owned(),
            profile,
            strategy,
            spans,
            masked,
            roles,
            malformed_literals,
            pair_roles,
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

        for marker in scan.markers {
            let text = trimmed[marker.start..marker.end].to_owned();
            let ordinal = role_cursor.entry(text.clone()).or_insert(0);
            let roles = self.roles.get(&text);
            let role = roles.and_then(|roles| roles.get(*ordinal)).copied();
            *ordinal += 1;
            // 原文に無い表記・原文の出現数を超えた分は生成マーカーとして扱う。
            match role {
                Some(MarkerRole::Literal) => {}
                Some(MarkerRole::Generated) | None => markers.push(marker),
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
            .any(|marker| marker.index >= self.spans.len())
        {
            return Err(ProtectError::UnknownMarker);
        }
        // 対のタグは、出力に現れた表記の役割がマスク側の記録と一致することを要求する。
        // 番号だけを数えると `</x0>…<x1>`（開きと閉じの入れ替え）でも「番号が 1 回ずつ
        // 揃っている」として通り、`$(item)` と `$()` が入れ替わったまま復元される。
        if self.strategy == ProtectStrategy::PairedTag
            && markers.iter().any(|marker| {
                marker
                    .role
                    .is_none_or(|role| self.pair_roles.get(marker.index) != Some(&role))
            })
        {
            return Err(ProtectError::SpanMismatch);
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
        for marker in markers {
            let span = &self.spans[marker.index];
            restored.push_str(&trimmed[cursor..marker.start]);
            restored.push_str(&span.raw);
            cursor = marker.end;
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
    pair_roles: &[PairRole],
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
        match strategy.marker_text(index, pair_roles[index]) {
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
    for marker in scan_markers(fragment, strategy).markers {
        roles
            .entry(fragment[marker.start..marker.end].to_owned())
            .or_default()
            .push(MarkerRole::Literal);
    }
    masked.push_str(fragment);
}

/// 出力から読み取った正規形のマーカー 1 つ分。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScannedMarker {
    start: usize,
    end: usize,
    /// 保護区間の番号。
    index: usize,
    /// [`ProtectStrategy::PairedTag`] のときの表記の役割（開き／閉じ／単独）。
    ///
    /// 他の strategy は `None`。復元時にマスク側が記録した役割と一致することを要求する。
    role: Option<PairRole>,
}

/// 出力中のマーカー走査結果。
struct MarkerScan {
    /// 正規形のマーカー。
    markers: Vec<ScannedMarker>,
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
    if strategy == ProtectStrategy::PairedTag {
        return scan_paired_tags(text);
    }
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
            Some(index) if canonical => scan.markers.push(ScannedMarker {
                start,
                end,
                index,
                role: None,
            }),
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

/// [`ProtectStrategy::PairedTag`] のマーカーを走査する。
///
/// `<x0>` / `</x0>` / `<x0/>` の 3 表記を読み、区間番号と表記の役割の両方を記録する。
/// 役割を捨てて番号だけを数えると、モデルが `</x0>…<x1>` のように開きと閉じを
/// 入れ替えた出力でも「番号が 1 回ずつ揃っている」として通り、`$(item)` と `$()` が
/// 入れ替わったまま復元される。役割は [`ProtectedSource::restore`] がマスク側の記録と
/// 突き合わせる。
fn scan_paired_tags(text: &str) -> MarkerScan {
    let mut scan = MarkerScan {
        markers: Vec::new(),
        malformed: Vec::new(),
    };
    let bytes = text.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'<' {
            cursor += 1;
            continue;
        }
        let Some((end, number, role)) = parse_paired_tag(text, cursor) else {
            cursor += 1;
            continue;
        };
        match (number, role) {
            (Some(number), Some(role)) => scan.markers.push(ScannedMarker {
                start: cursor,
                end,
                index: number,
                role: Some(role),
            }),
            _ => scan.malformed.push((cursor, end)),
        }
        cursor = end;
    }
    scan
}

/// `text[start..]` の先頭が `<x…>` 形式なら `(終了位置, 区間番号, 表記の役割)` を返す。
///
/// 正規形は `<x0>` / `</x0>` / `<x0/>` の 3 つだけ。`<x00>` や `<x>` は番号として読めても
/// 受理しない（改変されたマーカーが厳密検証をすり抜けるため）。`</x0/>` のように
/// 開きと閉じを兼ねた矛盾した表記も正規形ではない。
fn parse_paired_tag(text: &str, start: usize) -> Option<(usize, Option<usize>, Option<PairRole>)> {
    let rest = &text[start..];
    let after_open = rest.strip_prefix('<')?;
    let (body, closing_form) = match after_open.strip_prefix('/') {
        Some(body) => (body, true),
        None => (after_open, false),
    };
    let body = body.strip_prefix('x')?;
    let digit_length = body.bytes().take_while(u8::is_ascii_digit).count();
    if digit_length == 0 {
        return None;
    }
    let digits = &body[..digit_length];
    let tail = &body[digit_length..];
    let (tail, self_closing) = match tail.strip_prefix('/') {
        Some(tail) => (tail, true),
        None => (tail, false),
    };
    let tail = tail.strip_prefix('>')?;
    let end = text.len() - tail.len();
    let number = digits.parse::<usize>().ok();
    let role = match (closing_form, self_closing) {
        (false, false) => Some(PairRole::Open),
        (true, false) => Some(PairRole::Close),
        (false, true) => Some(PairRole::Single),
        // 開きと閉じを兼ねた表記は正規形ではない。
        (true, true) => None,
    };
    // 番号と役割が揃い、かつ表記が正規形と完全に一致するときだけ受理する。
    let canonical = match (number, role) {
        (Some(number), Some(role)) => {
            ProtectStrategy::PairedTag
                .marker_text(number, role)
                .as_deref()
                == Some(&text[start..end])
        }
        _ => false,
    };
    Some((end, number, role.filter(|_| canonical)))
}

/// マーカー番号が保護区間をちょうど 1 回ずつ覆っているか。
///
/// マスクしない [`ProtectStrategy::Verbatim`] にはマーカーが無いため、この検査は行わない
/// （生テキストの多重集合比較だけで検証する）。
fn markers_cover_every_span_once(
    markers: &[ScannedMarker],
    span_count: usize,
    strategy: ProtectStrategy,
) -> bool {
    if !strategy.masks() {
        return true;
    }
    if markers.len() != span_count {
        return false;
    }
    let mut seen = vec![false; span_count];
    for marker in markers {
        match seen.get_mut(marker.index) {
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
            ProtectStrategy::PairedTag,
        ] {
            assert_ne!(strategy, strategy.alternative());
        }
    }

    #[test]
    fn per_profile_defaults_follow_the_measured_winners() {
        // 実測（2B GPU、書式コードに挟まれた本文 24 entry・38 区間）:
        // Patchouli の `$(item)heat$()` は PairedTag で 0/7 件が未訳、
        // BracketedIndex で 1/7 件。`§7(Hidden)` は逆に BracketedIndex が 0/17 件、
        // PairedTag が 1/17 件。chrF は PairedTag が最も高い（0.4532 対 0.4426）。
        assert_eq!(
            ProtectStrategy::for_profile(PlaceholderProfile::Patchouli),
            ProtectStrategy::PairedTag
        );
        assert_eq!(
            ProtectStrategy::for_profile(PlaceholderProfile::Lang),
            ProtectStrategy::BracketedIndex
        );
        assert_eq!(
            ProtectStrategy::for_profile(PlaceholderProfile::FtbQuests),
            ProtectStrategy::BracketedIndex
        );
    }

    #[test]
    fn paired_tags_use_open_and_close_forms_around_the_enclosed_text() {
        // `$(item)heat$()` の内側は保護区間ではない。開き／閉じをタグで表すと、
        // 挟まれた `heat` が訳す対象であることがモデルへ伝わる。
        let protected = ProtectedSource::new(
            "produces $(item)heat$(), used",
            PlaceholderProfile::Patchouli,
            ProtectStrategy::PairedTag,
        );
        assert_eq!(protected.masked_text(), "produces <x0>heat</x1>, used");
        assert_eq!(
            protected.restore("produces <x0>熱</x1>, used").unwrap(),
            "produces $(item)熱$(), used"
        );
        // 開きと閉じを入れ替えた出力は受け付けない。番号だけを見ると「1 回ずつ揃って
        // いる」ので通ってしまい、`$(item)` と `$()` が入れ替わったまま復元される。
        assert_eq!(
            protected.restore("produces <x1>熱</x0>, used").err(),
            Some(ProtectError::SpanMismatch)
        );
        assert_eq!(
            protected.restore("produces </x0>熱</x1>, used").err(),
            Some(ProtectError::SpanMismatch)
        );
        // 開き側を単独の表記にしても、役割が違うので受け付けない。
        assert_eq!(
            protected.restore("produces <x0/>熱</x1>, used").err(),
            Some(ProtectError::SpanMismatch)
        );
    }

    #[test]
    fn paired_tags_keep_unpaired_codes_as_single_tags() {
        // `$(br)` は閉じない。対にできないので単独のタグとして表す。
        let protected = ProtectedSource::new(
            "$(br)Stone",
            PlaceholderProfile::Patchouli,
            ProtectStrategy::PairedTag,
        );
        assert_eq!(protected.masked_text(), "<x0/>Stone");
        assert_eq!(protected.restore("<x0/>石").unwrap(), "$(br)石");
    }

    #[test]
    fn paired_tags_reject_missing_duplicated_or_malformed_tags() {
        let protected = ProtectedSource::new(
            "$(item)heat$()",
            PlaceholderProfile::Patchouli,
            ProtectStrategy::PairedTag,
        );
        assert_eq!(protected.masked_text(), "<x0>heat</x1>");
        // 欠落
        assert_eq!(
            protected.restore("<x0>熱").err(),
            Some(ProtectError::SpanMismatch)
        );
        // 重複
        assert_eq!(
            protected.restore("<x0>熱</x0></x1>").err(),
            Some(ProtectError::SpanMismatch)
        );
        // 原文に無い番号
        assert_eq!(
            protected.restore("<x0>熱</x1><x7/>").err(),
            Some(ProtectError::UnknownMarker)
        );
        // 正規形でない表記（`<x00>` は `<x0>` として受理しない）
        assert_eq!(
            protected.restore("<x00>熱</x1>").err(),
            Some(ProtectError::UnknownMarker)
        );
        // 開き側を単独の表記に置き換えると、番号は揃っていても役割が違う。
        assert_eq!(
            protected.restore("<x0/>熱</x1>").err(),
            Some(ProtectError::SpanMismatch)
        );
    }

    #[test]
    fn paired_tags_do_not_treat_non_closing_macros_as_openers() {
        // `$(br)` と `$(k:…)` は閉じない。これらを開き側として積むと、後続の `$()` が
        // 別のマクロの閉じ側と組になり、挟まれた本文が誤って「対の中」と判定される。
        let protected = ProtectedSource::new(
            "$(br)$(item)heat$()",
            PlaceholderProfile::Patchouli,
            ProtectStrategy::PairedTag,
        );
        assert_eq!(protected.masked_text(), "<x0/><x1>heat</x2>");
        assert_eq!(
            protected.restore("<x0/><x1>熱</x2>").unwrap(),
            "$(br)$(item)熱$()"
        );
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
