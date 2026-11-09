//! 実在の Minecraft mod を使った PLaMo 3 Translate の評価ハーネス。
//!
//! アプリと同じ経路（[`yc_ai::TranslationEntry::new_with_profile`] →
//! [`yc_plamo::PlamoProvider::translate_batch`]）を通すので、ここで測った数字は
//! そのまま製品経路の数字になる。サーバは `yc_llama` で起動する。
//!
//! 実行例:
//!
//! ```text
//! # 全部（0.8B/2B/8B × このマシンで使えるバックエンド）
//! cargo run --release -p yc-plamo --example eval_mods
//! # 一部だけ
//! cargo run --release -p yc-plamo --example eval_mods -- --models 2b --backends vulkan
//! # CUDA で測る（このマシンで CUDA が使えるときだけ）
//! cargo run --release -p yc-plamo --example eval_mods -- --models 2b --backends cuda
//! # strategy の比較
//! cargo run --release -p yc-plamo --example eval_mods -- --models 2b --backends vulkan --strategies all
//! ```
//!
//! 結果は JSON と Markdown 断片で `<cache>/eval` へ書き出す。モデル・JAR は
//! dev キャッシュにだけ置き、リポジトリには入れない。

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

use serde::Serialize;
use tokio_util::sync::CancellationToken;
use yc_ai::{AiError, TranslationContext, TranslationEntry, TranslationProvider};
use yc_llama::{Backend, LlamaServer, ServerConfig, current_target_build, ensure_runtime};
use yc_plamo::{
    EntryReport, PlamoModel, PlamoProvider, PlamoProviderOptions, ProtectStrategy, catalog,
    find_model,
};
use yc_validation::PlaceholderProfile;

// ---------------------------------------------------------------------------
// 設定
// ---------------------------------------------------------------------------

/// 1 mod あたりのサンプル件数。プレースホルダ入りは別枠で全件寄りに集める。
const SAMPLE_PLAIN: usize = 120;
/// プレースホルダ／書式コード入りのサンプル上限。
const SAMPLE_FORMATTED_CAP: usize = 120;
/// 8B を CPU で回すときの縮小サンプル。
const SAMPLE_CPU_8B: usize = 60;
/// 他言語（zh_cn / ko_kr / de_de）の sanity サンプル。
const SAMPLE_OTHER_TARGET: usize = 30;
/// 代表例として書き出す件数（モデルごと）。
const EXAMPLES_PER_MODEL: usize = 30;
/// 決定性の確認に使うサンプル件数。
const DETERMINISM_SAMPLE: usize = 25;
/// 未訳のまま残った本文の代表例として残す件数。
const ENCLOSED_EXAMPLES: usize = 12;
/// 閉じコードを持たない装飾コード（`§7` など）で測る本文の長さ上限（文字数）。
///
/// `§7` は行末まで効くため、上限を外すと本文全体が 1 区間になり「原文のまま残ったか」が
/// 常に偽になって指標が意味を失う。実データの `§7(Hidden Advancement)` のような短い
/// ラベルだけを測る。
const IMPLICIT_REGION_LIMIT: usize = 40;

/// サンプル抽選のシード。固定して再現できるようにする。
const SEED: u64 = 0x5EED_1234_ABCD_0001;

fn cache_root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("YakuCraft-dev-cache")
}

// ---------------------------------------------------------------------------
// 決定的な擬似乱数
// ---------------------------------------------------------------------------

/// xorshift64*。サンプル抽選を再現可能にするためだけに使う。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// 先頭から `count` 件を残したまま Fisher-Yates で混ぜる。
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            let swap = self.below(index + 1);
            items.swap(index, swap);
        }
    }
}

// ---------------------------------------------------------------------------
// mod の読み込み
// ---------------------------------------------------------------------------

/// 評価に使う mod 1 件分の情報。
struct ModSpec {
    /// Modrinth のスラッグ。取得元の記録として残す。
    #[allow(dead_code)]
    slug: &'static str,
    display_name: &'static str,
    version: &'static str,
    jar: &'static str,
    /// `assets/<ns>/lang/en_us.json` の `<ns>`。
    namespace: &'static str,
    /// Patchouli のガイドブックを持つか。
    patchouli: bool,
}

const MODS: [ModSpec; 5] = [
    ModSpec {
        slug: "create",
        display_name: "Create",
        version: "mc1.20.1-6.0.8",
        jar: "create-1.20.1-6.0.8.jar",
        namespace: "create",
        patchouli: false,
    },
    ModSpec {
        slug: "farmers-delight",
        display_name: "Farmer's Delight",
        version: "1.20.1-1.3.4",
        jar: "FarmersDelight-1.20.1-1.3.4.jar",
        namespace: "farmersdelight",
        patchouli: false,
    },
    ModSpec {
        slug: "mekanism",
        display_name: "Mekanism",
        version: "10.4.16.80",
        jar: "Mekanism-1.20.1-10.4.16.80.jar",
        namespace: "mekanism",
        patchouli: false,
    },
    ModSpec {
        slug: "supplementaries",
        display_name: "Supplementaries",
        version: "1.20-3.1.44-forge",
        jar: "supplementaries-1.20-3.1.44-forge.jar",
        namespace: "supplementaries",
        patchouli: false,
    },
    ModSpec {
        slug: "ars-nouveau",
        display_name: "Ars Nouveau",
        version: "4.12.7",
        jar: "ars_nouveau-1.20.1-4.12.7-all.jar",
        namespace: "ars_nouveau",
        patchouli: true,
    },
];

/// 1 件分の評価対象。原文と、あれば公式の日本語訳。
#[derive(Clone, Debug)]
struct Candidate {
    key: String,
    source: String,
    reference: Option<String>,
    profile: PlaceholderProfile,
    /// `%s`・`§`・`$(...)` などを含むか。書式安全性の集計を分けて見るために持つ。
    formatted: bool,
}

/// JAR から 1 mod 分の候補を集める。
fn load_candidates(spec: &ModSpec, mods_dir: &Path) -> Result<Vec<Candidate>, String> {
    let path = mods_dir.join(spec.jar);
    let file =
        std::fs::File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| format!("{}: {error}", spec.jar))?;

    let lang_path = format!("assets/{}/lang/en_us.json", spec.namespace);
    let ja_path = format!("assets/{}/lang/ja_jp.json", spec.namespace);
    let english = read_json_object(&mut archive, &lang_path)?;
    let japanese = read_json_object(&mut archive, &ja_path).unwrap_or_default();

    let mut candidates = Vec::new();
    for (key, source) in &english {
        // 空文字と、翻訳しても意味の無いキーだけの値は落とす。
        if source.trim().is_empty() {
            continue;
        }
        let reference = japanese.get(key).cloned();
        let formatted = has_format_or_placeholder(source);
        candidates.push(Candidate {
            key: key.clone(),
            source: source.clone(),
            reference,
            profile: PlaceholderProfile::Lang,
            formatted,
        });
    }

    // Patchouli のガイドブックは lang 側の本文（`ars_nouveau.page.*`）として入っている。
    // 1.20+ の言語フォルダ形式（`patchouli_books/<book>/en_us/entries/**.json`）から
    // 参照されているキーを集め、Patchouli プロファイルの entry として扱う。
    if spec.patchouli {
        let prefix = format!("assets/{}/patchouli_books/", spec.namespace);
        let referenced = patchouli_referenced_keys(&mut archive, &prefix);
        for (key, source) in &english {
            if !referenced.contains(key) {
                continue;
            }
            candidates.push(Candidate {
                key: key.clone(),
                source: source.clone(),
                reference: japanese.get(key).cloned(),
                profile: PlaceholderProfile::Patchouli,
                formatted: has_format_or_placeholder(source),
            });
        }
    }

    Ok(candidates)
}

/// `patchouli_books/<book>/en_us/entries/**.json` が参照する lang キーを集める。
///
/// ページ本文は `"text": "<lang key>"` の形で lang を指す（1.20+ の形式）。インラインの
/// 散文も一部あるが、評価では lang キー経由の本文だけを対象にする。
fn patchouli_referenced_keys(
    archive: &mut zip::ZipArchive<std::fs::File>,
    prefix: &str,
) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let names: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .and_then(|entry| entry.name().ok().map(|name| name.to_string()))
        })
        .filter(|name| {
            name.starts_with(prefix) && name.contains("/en_us/entries/") && name.ends_with(".json")
        })
        .collect();
    for name in names {
        let Ok(mut entry) = archive.by_name(&name) else {
            continue;
        };
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        collect_patchouli_text_keys(&value, &mut keys);
    }
    keys
}

fn collect_patchouli_text_keys(value: &serde_json::Value, keys: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (field, inner) in map {
                if field == "text"
                    && let serde_json::Value::String(text) = inner
                    // lang キーは `mod.page.foo` の形。インライン散文（空白を含む文）は除く。
                    && !text.contains(' ')
                    && text.contains('.')
                {
                    keys.insert(text.clone());
                }
                collect_patchouli_text_keys(inner, keys);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_patchouli_text_keys(item, keys);
            }
        }
        _ => {}
    }
}

fn read_json_object(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
) -> Result<BTreeMap<String, String>, String> {
    let mut entry = archive
        .by_name(name)
        .map_err(|error| format!("{name}: {error}"))?;
    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .map_err(|error| format!("{name}: {error}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("{name}: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{name}: not a JSON object"))?;
    Ok(object
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|text| (key.clone(), text.to_owned())))
        .collect())
}

/// 書式コードやプレースホルダを含むか。集計の切り分けにだけ使う。
fn has_format_or_placeholder(text: &str) -> bool {
    text.contains('%')
        || text.contains('§')
        || text.contains('&')
        || text.contains('{')
        || text.contains("$(")
        || text.contains('\\')
        || text.contains('\n')
}

// ---------------------------------------------------------------------------
// サンプル抽選
// ---------------------------------------------------------------------------

/// 1 mod 分の固定サンプル。書式入りを優先して集め、残りを通常文で埋める。
fn sample(candidates: &[Candidate], budget: usize) -> Vec<Candidate> {
    let mut rng = Rng::new(SEED ^ budget as u64);
    let mut formatted: Vec<&Candidate> = candidates.iter().filter(|item| item.formatted).collect();
    let mut plain: Vec<&Candidate> = candidates.iter().filter(|item| !item.formatted).collect();
    rng.shuffle(&mut formatted);
    rng.shuffle(&mut plain);

    let formatted_take = formatted.len().min(SAMPLE_FORMATTED_CAP).min(budget);
    let plain_take = plain
        .len()
        .min(budget.saturating_sub(formatted_take).max(SAMPLE_PLAIN));
    let plain_take = plain_take.min(budget.saturating_sub(formatted_take));

    let mut picked: Vec<Candidate> = formatted
        .into_iter()
        .take(formatted_take)
        .chain(plain.into_iter().take(plain_take))
        .cloned()
        .collect();
    rng.shuffle(&mut picked);
    picked
}

/// 1 mod 分の、書式コードに挟まれた本文を持つ entry をすべて集める。
///
/// 通常のサンプル（`sample`）は書式入りを最大 120 件しか採らないため、対になった
/// 書式コードを持つ entry は数件しか残らない（実測 7 件）。未訳率を測るには母数が
/// 足りないので、この用途だけは該当 entry を全件使う。
fn enclosed_sample(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut picked: Vec<Candidate> = candidates
        .iter()
        .filter(|candidate| !enclosed_regions(&candidate.source, candidate.profile).is_empty())
        .cloned()
        .collect();
    let mut rng = Rng::new(SEED ^ 0xE11C_105E_D000_0001);
    rng.shuffle(&mut picked);
    picked
}

/// chrF（文字 n-gram の F スコア）。
///
/// 実装は標準の chrF（Popović 2015）に合わせる。次数ごとの適合率・再現率を**先に平均**し、
/// その平均から β-F 値を 1 回だけ計算する。次数ごとに F を出して平均すると別の指標に
/// なってしまう（短い n と長い n の重みが変わってしまう）。
///
/// 空白は除いた文字 n-gram を n = 1..=`max_n` で数える。日本語のように単語分割が要らない
/// 比較に向く。`beta` は再現率の重み（chrF2 なら 2.0）。
fn chrf(hypothesis: &str, reference: &str, max_n: usize, beta: f64) -> f64 {
    chrf_statistics(hypothesis, reference, max_n).score(beta)
}

/// chrF の n-gram 統計。コーパス全体で合算してから 1 回だけスコアを出すために持つ。
///
/// 標準の chrF はコーパス単位で数える。entry ごとにスコアを出して平均すると、
/// 短い entry と長い entry の重みが同じになり、指標が変わる。両方を出して区別する。
#[derive(Clone, Debug, Default)]
struct ChrFStatistics {
    /// 次数ごとの `(一致した n-gram 数, 仮説の n-gram 数, 参照の n-gram 数)`。
    per_order: Vec<(usize, usize, usize)>,
}

impl ChrFStatistics {
    fn add(&mut self, hypothesis: &str, reference: &str, max_n: usize) {
        let hypothesis: Vec<char> = hypothesis.chars().filter(|c| !c.is_whitespace()).collect();
        let reference: Vec<char> = reference.chars().filter(|c| !c.is_whitespace()).collect();
        if hypothesis.is_empty() || reference.is_empty() {
            return;
        }
        self.per_order.resize(max_n, (0, 0, 0));
        for n in 1..=max_n {
            let hyp = ngram_counts(&hypothesis, n);
            let refr = ngram_counts(&reference, n);
            let mut overlap = 0usize;
            for (gram, count) in &hyp {
                if let Some(other) = refr.get(gram) {
                    overlap += (*count).min(*other);
                }
            }
            let slot = &mut self.per_order[n - 1];
            slot.0 += overlap;
            slot.1 += hyp.values().sum::<usize>();
            slot.2 += refr.values().sum::<usize>();
        }
    }

    /// 合算した統計から 1 回だけ β-F を計算する。
    fn score(&self, beta: f64) -> f64 {
        let mut precision_sum = 0.0;
        let mut recall_sum = 0.0;
        let mut counted = 0;
        for (overlap, hyp_total, ref_total) in &self.per_order {
            if *hyp_total == 0 || *ref_total == 0 {
                continue;
            }
            precision_sum += *overlap as f64 / *hyp_total as f64;
            recall_sum += *overlap as f64 / *ref_total as f64;
            counted += 1;
        }
        if counted == 0 {
            return 0.0;
        }
        let precision = precision_sum / counted as f64;
        let recall = recall_sum / counted as f64;
        let beta_squared = beta * beta;
        let denominator = beta_squared * precision + recall;
        if denominator == 0.0 {
            0.0
        } else {
            (1.0 + beta_squared) * precision * recall / denominator
        }
    }
}

/// 1 件分の chrF 統計を返す。`chrf` と同じ定義。
fn chrf_statistics(hypothesis: &str, reference: &str, max_n: usize) -> ChrFStatistics {
    let mut statistics = ChrFStatistics::default();
    statistics.add(hypothesis, reference, max_n);
    statistics
}

fn ngram_counts(chars: &[char], n: usize) -> BTreeMap<Vec<char>, usize> {
    let mut counts = BTreeMap::new();
    if chars.len() < n {
        return counts;
    }
    for window in chars.windows(n) {
        *counts.entry(window.to_vec()).or_insert(0) += 1;
    }
    counts
}

// ---------------------------------------------------------------------------
// 書式コードに挟まれた本文の未訳率
// ---------------------------------------------------------------------------

/// 原文の中で「対になった書式コードに挟まれた本文」1 つ分。
#[derive(Clone, Debug)]
struct EnclosedRegion {
    /// 挟まれた本文そのもの（原文の生テキスト）。
    inner: String,
    /// 開き側の書式コード。代表例の表示に使う。
    opener: String,
    /// 閉じ側の書式コード。代表例の表示に使う。
    closer: String,
}

impl EnclosedRegion {
    /// 原文どおりの表記。代表例をレポートへ載せるときに使う。
    fn render(&self) -> String {
        format!("{}{}{}", self.opener, self.inner, self.closer)
    }
}

/// 行末まで効く装飾コード（`§7` / `&a` など）かどうか。
///
/// `is_opening_code` は「閉じ側でない保護区間」すべてを開き側とみなすため、`%1$s` や
/// `{0}` も含んでしまう。暗黙の閉じを行末とみなしてよいのは装飾コードだけなので、
/// ここで絞る。
fn is_decoration_code(raw: &str) -> bool {
    let mut characters = raw.chars();
    matches!(characters.next(), Some('§' | '&'))
        && characters
            .next()
            .is_some_and(|code| code.is_ascii_alphanumeric())
        && characters.next().is_none()
}

/// 原文から、対になった書式コードに挟まれた本文を集める。
///
/// Patchouli の `$(item)heat$()`、Minecraft の `§a…§r`、FTB の `&a…&r` が対象。
/// 開き側／閉じ側の判定は `yc_plamo` の `protect` と同じ規則を使う（プロダクト側の
/// 保護区間の切り方と測定対象を一致させるため、規則は 1 箇所から借りる）。
///
/// `§` / `&` の装飾コードは `§r` で明示的に閉じないことが多い（`§7(Hidden)` のように
/// 行末まで効く）。閉じが無い場合は行末（`\n`）または文字列の終端を閉じとみなす。
/// そうしないと Create / Mekanism の `§` 入り entry が 1 件も測れない。
fn enclosed_regions(source: &str, profile: PlaceholderProfile) -> Vec<EnclosedRegion> {
    let spans = yc_validation::extract_protected_spans(source, profile);
    let mut regions = Vec::new();
    let mut opener: Option<&yc_validation::ProtectedSpan> = None;
    for span in &spans {
        if yc_plamo::protect::is_closing_code(&span.raw) {
            if let Some(open) = opener.take() {
                push_enclosed_region(source, open, span.start, &span.raw, &mut regions);
            }
        } else if is_decoration_code(&span.raw) {
            // 直前の開きが閉じられないまま次の装飾コードが来たら、前の開きはここまで。
            if let Some(open) = opener.replace(span) {
                push_implicit_region(source, open, span.start, &mut regions);
            }
        } else if yc_plamo::protect::is_opening_code(&span.raw) {
            // Patchouli のマクロは `$()` で閉じる。閉じないまま次のマクロが来たら、
            // 前のマクロは範囲を決められないので測らない。
            opener = Some(span);
        }
    }
    // 閉じられないまま終わった装飾コードも、文字列の終端まで効く。
    if let Some(open) = opener {
        push_implicit_region(source, open, source.len(), &mut regions);
    }
    regions
}

/// 明示的な閉じコードを持つ区間を積む。
fn push_enclosed_region(
    source: &str,
    open: &yc_validation::ProtectedSpan,
    close_start: usize,
    closer: &str,
    regions: &mut Vec<EnclosedRegion>,
) {
    let inner = &source[open.end..close_start];
    if inner.trim().is_empty() {
        return;
    }
    regions.push(EnclosedRegion {
        inner: inner.to_owned(),
        opener: open.raw.clone(),
        closer: closer.to_owned(),
    });
}

/// 閉じコードが無い区間を、行末（または次の開き／文字列の終端）までとして積む。
///
/// 暗黙の閉じは `§` / `&` の装飾コードだけに許す。Patchouli の `$(item)` が閉じない
/// 場合は本文の範囲を決められないので、測らない（製品側も対にしていない）。
///
/// 上限 [`IMPLICIT_REGION_LIMIT`] を設けるのは、`§7` が行末まで効くと本文全体が
/// 1 区間になり、「原文がそのまま残ったか」がほぼ常に偽になって指標が意味を失うため。
/// 実データの `§7(Hidden Advancement)` のような短いラベルだけを測る。
fn push_implicit_region(
    source: &str,
    open: &yc_validation::ProtectedSpan,
    limit: usize,
    regions: &mut Vec<EnclosedRegion>,
) {
    if !is_decoration_code(&open.raw) {
        return;
    }
    let rest = &source[open.end..limit];
    let end = rest
        .find('\n')
        .map_or(open.end + rest.len(), |offset| open.end + offset);
    let inner = &source[open.end..end];
    if inner.trim().is_empty() || inner.chars().count() > IMPLICIT_REGION_LIMIT {
        return;
    }
    regions.push(EnclosedRegion {
        inner: inner.to_owned(),
        opener: open.raw.clone(),
        // 暗黙の閉じなので、閉じコードは空。代表例の表示では付けない。
        closer: String::new(),
    });
}

/// 挟まれた本文が訳文でも原文のまま残っているか。
///
/// 「未訳」の判定は、原文の本文が訳文にそのまま現れるかで行う。固有名詞が正当に
/// 英語のまま残る場合と区別できないため、参照訳がある entry では参照訳と比べて
/// 「参照訳でも同じ表記のまま」なら未訳と数えない（`judge` を参照）。
fn enclosed_kept_verbatim(translation: &str, region: &EnclosedRegion) -> bool {
    let inner = region.inner.trim();
    if inner.is_empty() {
        return false;
    }
    // 英字を含まない本文（記号だけ）は訳しようがないので対象外。
    if !inner.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    translation.contains(inner)
}

/// 1 回の実行での、書式コードに挟まれた本文の集計。
#[derive(Clone, Debug, Default, Serialize)]
struct EnclosedStats {
    /// 対になった書式コードを含む entry の数（サンプル全体）。
    ///
    /// 訳文が得られなかった entry も含める。除外すると保護区間の保持率が
    /// 定義上 100% になり、失敗を隠してしまう。
    entries: usize,
    /// 対象になった本文の総数。訳文が無い entry の本文も数える。
    regions: usize,
    /// 訳文が得られた entry の数。
    translated_entries: usize,
    /// 訳文が得られた entry の本文の数。
    translated_regions: usize,
    /// 訳文でも原文のまま残った本文の数。
    kept_verbatim: usize,
    /// 参照訳でも同じ表記のまま残っている本文の数（未訳と数えない分）。
    legitimately_kept: usize,
    /// 保護区間が原文どおりに戻ったことを確認できた entry 数。
    spans_preserved: usize,
    /// 保護区間を確認できた entry の総数（訳文が得られた entry と同じ）。
    ///
    /// 訳文が無い entry は「保持できた」とは数えない。`entries` を分母にすると、
    /// 失敗した entry が保持率を下げる形になり、失敗が表に出る。
    spans_total: usize,
    /// 対象 entry のコーパス chrF。
    corpus_chrf: f64,
    /// 未訳のまま残った本文の代表例。レポートの根拠として残す。
    examples: Vec<String>,
}

/// 書式コードに挟まれた本文の未訳率を集計する。
///
/// 「未訳」は、原文の本文が訳文にそのまま現れるかで判定する。固有名詞が正当に英語の
/// まま残る場合と区別できないため、参照訳がある entry では参照訳の側でも同じ表記が
/// 残っているかを確かめ、残っている分は `legitimately_kept` として未訳から除く。
///
/// 訳文が得られなかった entry も `entries` / `regions` に数える。以前は先に除外して
/// いたため、残った entry がすべて `spans_preserved` に加算され、保護区間の保持率が
/// 定義上 100% になっていた（失敗が表に出ない）。
fn enclosed_stats(outcomes: &[EntryOutcome]) -> EnclosedStats {
    let mut stats = EnclosedStats::default();
    let mut corpus = ChrFStatistics::default();
    for outcome in outcomes {
        let regions = enclosed_regions(&outcome.candidate.source, outcome.candidate.profile);
        if regions.is_empty() {
            continue;
        }
        // 分母は訳文の有無に関わらず数える。
        stats.entries += 1;
        stats.regions += regions.len();
        let Some(translation) = outcome.translation.as_deref() else {
            continue;
        };
        // 訳文が得られた entry だけが「保持を確認できた」対象になる。
        stats.translated_entries += 1;
        stats.translated_regions += regions.len();
        stats.spans_total += 1;
        stats.spans_preserved += 1;
        for region in &regions {
            if !enclosed_kept_verbatim(translation, region) {
                continue;
            }
            // 参照訳でも同じ表記なら、訳さないのが正しい（固有名詞など）。
            let legitimately = outcome
                .candidate
                .reference
                .as_deref()
                .is_some_and(|reference| reference.contains(region.inner.trim()));
            if legitimately {
                stats.legitimately_kept += 1;
            } else {
                stats.kept_verbatim += 1;
                if stats.examples.len() < ENCLOSED_EXAMPLES {
                    stats.examples.push(region.render());
                }
            }
        }
        if let Some(reference) = outcome.candidate.reference.as_deref() {
            corpus.add(translation, reference, 6);
        }
    }
    stats.corpus_chrf = corpus.score(2.0);
    stats
}

// ---------------------------------------------------------------------------
// 結果の型
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
struct SpeedStats {
    load_seconds: f64,
    wall_seconds: f64,
    entries: usize,
    entries_per_second: f64,
    /// サーバが報告した生成速度の平均（トークン/秒）。
    tokens_per_second_mean: f64,
    /// 生成トークン数の合計。entries/second と併せて実効速度を見る。
    generated_tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
struct SafetyStats {
    total: usize,
    /// 保護区間が原文どおりに戻った件数。
    preserved: usize,
    /// entry 単位の失敗理由ごとの件数。バッチ全体の失敗は含めない。
    failures: BTreeMap<String, usize>,
    /// 2 回目の strategy で成功した件数。
    recovered_by_fallback: usize,
    /// 呼び出し全体が失敗して結果を受け取れなかった entry 数。
    ///
    /// `translate_batch` は 1 件でも致命的な失敗をすると `Err` を返し、それまでの成功も
    /// 捨てる。この entry たちは「失敗した」のではなく「試行できなかった」ので、
    /// entry 単位の失敗とは別に数える。
    not_attempted: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
struct StabilityStats {
    empty_outputs: usize,
    output_limits: usize,
    prompt_too_long: usize,
    format_mismatch: usize,
    unsafe_inputs: usize,
    /// 未対応の翻訳先（`AiError::UnsupportedTargetLanguage`）。
    unsupported_language: usize,
    /// 呼び出し全体が落ちた回数（バッチ単位）。
    batch_failures: usize,
    /// そのうちタイムアウトと判定した回数。
    ///
    /// `PlamoProvider` は reqwest のタイムアウトも接続失敗も `AiError::Network` に畳むため、
    /// 外からは区別できない。経過時間が設定タイムアウト以上ならタイムアウトとみなす。
    timeouts: usize,
    /// サーバ側の障害（通信断・HTTP エラー・不正応答）。
    server_errors: usize,
    /// 呼び出し全体の失敗で失われた entry 数。
    entries_lost_to_batch_failure: usize,
    /// サンプル中に `llama-server` が落ちていた回数（クラッシュ/再起動）。
    server_restarts: usize,
    /// 同じサンプルを 2 回流して 1 件でも差が出たか。
    nondeterministic_entries: usize,
    determinism_checked: usize,
}

impl StabilityStats {
    /// 失敗理由の集計から、レポート用の内訳を作る。
    ///
    /// entry 単位の失敗（`EntryReport::failure`）とバッチ全体の失敗を混ぜない。
    /// バッチ側は [`SampleOutcome::batch_failures`] から別途数える。
    fn from_safety(safety: &SafetyStats) -> Self {
        Self {
            empty_outputs: count_failure(safety, "empty_output"),
            output_limits: count_failure(safety, "output_limit"),
            prompt_too_long: count_failure(safety, "prompt_too_long"),
            format_mismatch: count_failure(safety, "format_mismatch"),
            unsafe_inputs: count_failure(safety, "unsafe_source_text"),
            unsupported_language: count_failure(safety, "unsupported_target_language"),
            // バッチ単位の値は `record_batch_failures` で後から入れる。
            batch_failures: 0,
            timeouts: 0,
            server_errors: 0,
            entries_lost_to_batch_failure: safety.not_attempted,
            server_restarts: 0,
            nondeterministic_entries: 0,
            determinism_checked: 0,
        }
    }

    /// バッチ単位の失敗を集計へ反映する。
    fn record_batch_failures(&mut self, failures: &[BatchFailure]) {
        self.batch_failures = failures.len();
        self.timeouts = failures.iter().filter(|failure| failure.timed_out).count();
        self.server_errors = failures
            .iter()
            .filter(|failure| is_server_error(failure.reason))
            .count();
        // 未対応言語は呼び出し全体を落とすので、entry 単位の失敗ではなくここで数える。
        // 失われた entry 数 = その翻訳先を要求した entry 数。
        self.unsupported_language = failures
            .iter()
            .filter(|failure| failure.reason == "unsupported_target_language")
            .map(|failure| failure.lost_entries)
            .sum();
    }
}

#[derive(Clone, Debug, Default, Serialize)]
struct ChrFStats {
    entries: usize,
    /// entry ごとの chrF の単純平均。短い entry と長い entry を同じ重みで扱う。
    mean: f64,
    /// コーパス chrF。全 entry の n-gram 統計を合算してから 1 回だけスコアを出す。
    ///
    /// 標準の chrF はこちら。長い entry の寄与が大きくなるため `mean` とは値が違う。
    /// 表では両方を並べ、比較には `corpus` を使う。
    corpus: f64,
    median: f64,
}

#[derive(Clone, Debug, Serialize)]
struct ModResult {
    mod_name: String,
    version: String,
    candidates: usize,
    sampled: usize,
    with_reference: usize,
    speed: SpeedStats,
    safety: SafetyStats,
    chrf: ChrFStats,
}

#[derive(Clone, Debug, Serialize)]
struct Example {
    mod_name: String,
    key: String,
    source: String,
    translation: String,
    reference: Option<String>,
    chrf: Option<f64>,
}

/// 全 entry の生データ。後処理（引用符・空白・停止トークン）の分析に使う。
#[derive(Clone, Debug, Serialize)]
struct OutcomeDump {
    model: String,
    backend: String,
    strategy: String,
    mod_name: String,
    key: String,
    source: String,
    translation: Option<String>,
    reference: Option<String>,
    failure: Option<String>,
    raw_output: Option<String>,
    n_predict: u32,
    source_tokens: u32,
    generated_tokens: u32,
    chrf: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
struct RunResult {
    model: String,
    backend: String,
    strategy: String,
    sample_size: usize,
    context_enabled: bool,
    glossary_terms: usize,
    speed: SpeedStats,
    safety: SafetyStats,
    stability: StabilityStats,
    chrf: ChrFStats,
    /// 書式コードに挟まれた本文の未訳率。対象 entry が無ければ 0。
    enclosed: EnclosedStats,
    /// 用語集の遵守率の分子・分母。率だけでは分母の小ささが見えないため件数も残す。
    glossary_applicable: usize,
    glossary_adhered: usize,
    /// 用語が原文に現れる entry（affected entries）の数とコーパス chrF。
    ///
    /// 全体の chrF は用語と無関係な entry にも薄まるため、用語集の効果はこちらで見る。
    glossary_affected_entries: usize,
    glossary_affected_chrf: f64,
    /// 使った用語集そのもの。率の根拠を後から検算できるように残す。
    glossary_used: Vec<(String, String)>,
    per_mod: Vec<ModResult>,
    /// 呼び出し全体を落とした失敗の表示名（`UnsupportedTargetLanguage` など）。
    /// entry 単位の失敗しか無かった場合は空。
    batch_errors: Vec<String>,
    examples_good: Vec<Example>,
    examples_bad: Vec<Example>,
}

// ---------------------------------------------------------------------------
// 1 回分の実行
// ---------------------------------------------------------------------------

/// 評価対象 1 件分の生データ。
struct EntryOutcome {
    mod_name: String,
    candidate: Candidate,
    translation: Option<String>,
    failure: Option<String>,
    report: Option<EntryReport>,
}

/// `translate_batch` が呼び出し全体を落としたときの記録。
///
/// 以前は `unwrap_or_default()` でエラーを捨てていたため、サーバエラーやタイムアウト、
/// `UnsupportedTargetLanguage` が「entry ごとの失敗」と区別できず、`server_errors` が
/// 常に 0、`pt_br` の 30 件がすべて `unknown` になっていた。型付きのエラーを分類して
/// 保持する（`AiError` は `Clone` を持たないので、分類結果だけを残す）。
#[derive(Clone, Debug)]
struct BatchFailure {
    /// 集計用の短い理由コード（`AiError` の variant 名を snake_case にしたもの）。
    reason: &'static str,
    /// レポートへ残す表示名（`AiError` の variant 名）。
    label: &'static str,
    /// タイムアウトと判定したか。
    ///
    /// `PlamoProvider` は reqwest のタイムアウトも接続失敗も `AiError::Network` に畳む。
    /// 外から区別する手段が無いので、呼び出しの経過時間が設定タイムアウト以上なら
    /// タイムアウトとみなす（呼び出し側が `elapsed` を渡す）。
    timed_out: bool,
    /// この失敗で失われた entry 数。
    lost_entries: usize,
}

impl BatchFailure {
    fn new(error: &AiError, elapsed: std::time::Duration, timeout: std::time::Duration) -> Self {
        let reason = classify_ai_error(error);
        Self {
            reason,
            label: ai_error_variant(error),
            // 通信系の失敗だけをタイムアウト候補にする。設定時間に達していれば timeout。
            timed_out: reason == "server_unreachable" && elapsed >= timeout,
            lost_entries: 0,
        }
    }
}

/// `AiError` の variant 名。`Display` はユーザー向け文言で variant が分からない。
fn ai_error_variant(error: &AiError) -> &'static str {
    match error {
        AiError::InvalidUrl => "InvalidUrl",
        AiError::InvalidConfiguration => "InvalidConfiguration",
        AiError::Network => "Network",
        AiError::ProviderAuth => "ProviderAuth",
        AiError::ChatGptReauthenticationRequired => "ChatGptReauthenticationRequired",
        AiError::ChatGptCallbackPortInUse => "ChatGptCallbackPortInUse",
        AiError::ChatGptLoginCancelled => "ChatGptLoginCancelled",
        AiError::ChatGptLoginTimedOut => "ChatGptLoginTimedOut",
        AiError::ChatGptLoginFailed => "ChatGptLoginFailed",
        AiError::ProviderRateLimit { .. } => "ProviderRateLimit",
        AiError::ProviderResponse { .. } => "ProviderResponse",
        AiError::InvalidProviderResponse => "InvalidProviderResponse",
        AiError::UnsupportedTargetLanguage => "UnsupportedTargetLanguage",
        AiError::CredentialStore => "CredentialStore",
        AiError::JobAlreadyRunning => "JobAlreadyRunning",
        AiError::Cancelled => "Cancelled",
    }
}

/// `AiError` を集計用の理由コードへ分類する。
fn classify_ai_error(error: &AiError) -> &'static str {
    match error {
        AiError::UnsupportedTargetLanguage => "unsupported_target_language",
        AiError::Network => "server_unreachable",
        AiError::ProviderAuth => "server_auth",
        AiError::ProviderResponse { .. } => "server_error",
        AiError::InvalidProviderResponse => "invalid_server_response",
        AiError::ProviderRateLimit { .. } => "rate_limited",
        AiError::Cancelled => "cancelled",
        AiError::InvalidUrl | AiError::InvalidConfiguration => "invalid_configuration",
        AiError::CredentialStore => "credential_store",
        AiError::JobAlreadyRunning => "job_already_running",
        // ChatGPT 系は PLaMo の経路では起きないが、網羅性のために分類だけしておく。
        AiError::ChatGptReauthenticationRequired
        | AiError::ChatGptCallbackPortInUse
        | AiError::ChatGptLoginCancelled
        | AiError::ChatGptLoginTimedOut
        | AiError::ChatGptLoginFailed => "chatgpt_login",
    }
}

/// サーバ側の障害（通信断・HTTP エラー・不正応答）かどうか。
fn is_server_error(reason: &str) -> bool {
    matches!(
        reason,
        "server_unreachable" | "server_auth" | "server_error" | "invalid_server_response"
    )
}

/// バッチ失敗の表示名を、重複を畳んで返す。
///
/// 同じ理由で複数バッチが落ちる（`pt_br` は mod ごとに 1 回）ので、そのまま並べると
/// レポートの表が同じ名前で埋まる。
fn batch_error_labels(failures: &[BatchFailure]) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    for failure in failures {
        let label = failure.label.to_owned();
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

/// サンプル 1 回分の結果。
struct SampleOutcome {
    entries: Vec<EntryOutcome>,
    /// 呼び出し全体を落とした失敗（バッチ単位）。
    batch_failures: Vec<BatchFailure>,
    /// サンプル中に `llama-server` が落ちていた回数。
    server_restarts: usize,
}

/// サンプルを 1 回流し、entry ごとの結果を返す。
///
/// 観測フックは provider に 1 つだけ付き、共有バッファへ追記する。mod ごとにバッファを
/// 空にするので、どの report がどの entry のものかは `entry_id` で引ける。
///
/// `translate_batch` が呼び出し全体を落とした場合、それまでの成功も失われる。失われた
/// entry は「失敗した」のではなく「試行できなかった」ので `not_attempted` として数え、
/// entry 単位の失敗（`EntryReport::failure`）とは混ぜない。
async fn run_sample(
    provider: &PlamoProvider,
    reports: &Arc<Mutex<Vec<EntryReport>>>,
    mods: &[(String, String, Vec<Candidate>)],
    glossary: &[(String, String)],
    target_locale: &str,
    timeout: std::time::Duration,
    server: &LlamaServer,
) -> SampleOutcome {
    let mut outcomes = Vec::new();
    let mut batch_failures: Vec<BatchFailure> = Vec::new();
    let mut server_restarts = 0_usize;
    let mut next_id = 1_i64;
    for (mod_name, namespace, candidates) in mods {
        let entries: Vec<TranslationEntry> = candidates
            .iter()
            .map(|candidate| {
                let id = next_id;
                next_id += 1;
                TranslationEntry::new_with_profile(
                    id,
                    candidate.key.clone(),
                    candidate.source.clone(),
                    "en_us",
                    target_locale,
                    candidate.profile,
                )
            })
            .collect();

        // 文脈の有無は provider 側の `include_context` で切り替える。ここで mod 名を
        // 空にすると翻訳キーと entry 種別のヒントが残り、「文脈なし」にならない。
        let context = TranslationContext {
            mod_name: mod_name.clone(),
            namespace: namespace.clone(),
            custom_prompt: None,
            glossary_terms: glossary
                .iter()
                .map(|(source, target)| yc_ai::GlossaryTerm {
                    source_term: source.clone(),
                    target_term: target.clone(),
                })
                .collect(),
        };

        reports.lock().expect("report mutex poisoned").clear();
        let started = Instant::now();
        let result = provider
            .translate_batch(&entries, &context, &CancellationToken::new())
            .await;
        let elapsed = started.elapsed();
        // サーバが落ちていないか確認する。落ちていれば再起動の回数として数える。
        if !server.is_running() {
            server_restarts += 1;
        }
        // 呼び出し全体の失敗は捨てずに保持する。`pt_br` のような未対応言語やサーバ障害は
        // ここでしか観測できない。
        let mut batch_failure = result
            .as_ref()
            .err()
            .map(|error| BatchFailure::new(error, elapsed, timeout));
        let translations = result.unwrap_or_default();
        let by_id: BTreeMap<i64, String> = translations
            .into_iter()
            .map(|translation| (translation.id, translation.translation))
            .collect();
        let collected = reports.lock().expect("report mutex poisoned").clone();

        // バッチが落ちた場合、report が無い entry は「試行できなかった」。
        if let Some(failure) = batch_failure.as_mut() {
            failure.lost_entries = entries
                .iter()
                .filter(|entry| {
                    !collected.iter().any(|report| report.entry_id == entry.id)
                        && !by_id.contains_key(&entry.id)
                })
                .count();
            batch_failures.push(failure.clone());
        }

        for (candidate, entry) in candidates.iter().zip(entries.iter()) {
            let report = collected
                .iter()
                .find(|report| report.entry_id == entry.id)
                .cloned();
            // entry 単位の失敗だけを `failure` に入れる。バッチ全体の失敗は
            // `not_attempted` として別に数えるので、ここでは混ぜない。
            let failure = report
                .as_ref()
                .and_then(|report| report.failure.map(str::to_owned));
            outcomes.push(EntryOutcome {
                mod_name: mod_name.clone(),
                candidate: candidate.clone(),
                translation: by_id.get(&entry.id).cloned(),
                failure,
                report,
            });
        }
    }
    SampleOutcome {
        entries: outcomes,
        batch_failures,
        server_restarts,
    }
}

/// 観測フックが書き込む共有バッファ。
type ReportSink = Arc<Mutex<Vec<EntryReport>>>;

/// 観測フック付きの provider を作る。フックは共有バッファへ追記するだけ。
///
/// `strategy` が `None` なら profile ごとの既定（`protect_strategy_by_profile`）を使う。
fn provider_with_reports(
    base_url: &str,
    api_key: &str,
    strategy: Option<ProtectStrategy>,
    context_size: u32,
    include_context: bool,
    glossary_history: bool,
    include_glossary: bool,
) -> Result<(PlamoProvider, ReportSink), String> {
    let reports: ReportSink = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    let provider = PlamoProvider::new(
        base_url,
        Some(api_key.to_owned()),
        PlamoProviderOptions {
            protect_strategy: strategy.unwrap_or_default(),
            protect_strategy_by_profile: strategy.is_none(),
            context_size: Some(context_size),
            include_context,
            glossary_history,
            include_glossary,
            entry_observer: Some(Arc::new(move |report: &EntryReport| {
                sink.lock()
                    .expect("report mutex poisoned")
                    .push(report.clone());
            })),
            ..PlamoProviderOptions::default()
        },
    )
    .map_err(|error| format!("provider: {error}"))?;
    Ok((provider, reports))
}

fn summarize(
    outcomes: &[EntryOutcome],
    load_seconds: f64,
    wall_seconds: f64,
) -> (SpeedStats, SafetyStats, ChrFStats, Vec<ModResult>) {
    let mut safety = SafetyStats {
        total: outcomes.len(),
        ..SafetyStats::default()
    };
    let mut scores = Vec::new();
    let mut corpus = ChrFStatistics::default();
    let mut generated_tokens = 0_u64;
    let mut tps_sum = 0.0_f64;
    let mut tps_count = 0_usize;

    for outcome in outcomes {
        match outcome.translation.as_deref() {
            Some(translation) => {
                safety.preserved += 1;
                if let Some(reference) = outcome.candidate.reference.as_deref() {
                    scores.push(chrf(translation, reference, 6, 2.0));
                    corpus.add(translation, reference, 6);
                }
            }
            None => match outcome.failure.as_deref() {
                // entry 単位の失敗。
                Some(reason) => *safety.failures.entry(reason.to_owned()).or_insert(0) += 1,
                // report も failure も無い = バッチ全体の失敗で試行できなかった。
                None => safety.not_attempted += 1,
            },
        }
        if outcome
            .report
            .as_ref()
            .is_some_and(|report| report.attempts > 1)
        {
            safety.recovered_by_fallback += 1;
        }
        if let Some(report) = outcome.report.as_ref() {
            generated_tokens += u64::from(report.generated_tokens);
            if report.tokens_per_second > 0.0 {
                tps_sum += f64::from(report.tokens_per_second);
                tps_count += 1;
            }
        }
    }

    let entries = outcomes.len();
    // 呼び出し全体が即座に落ちた場合（未対応言語など）、wall time がほぼ 0 になり
    // entries/second が発散する。推論をしていないので速度は 0 として扱う。
    let produced = safety.preserved;
    let speed = SpeedStats {
        load_seconds,
        wall_seconds,
        entries,
        entries_per_second: if wall_seconds > 0.0 && produced > 0 {
            produced as f64 / wall_seconds
        } else {
            0.0
        },
        tokens_per_second_mean: if tps_count > 0 {
            tps_sum / tps_count as f64
        } else {
            0.0
        },
        generated_tokens,
    };

    scores.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let chrf_stats = ChrFStats {
        entries: scores.len(),
        mean: if scores.is_empty() {
            0.0
        } else {
            scores.iter().sum::<f64>() / scores.len() as f64
        },
        corpus: corpus.score(2.0),
        median: if scores.is_empty() {
            0.0
        } else {
            scores[scores.len() / 2]
        },
    };

    // mod ごとの内訳。
    let mut per_mod: BTreeMap<String, Vec<&EntryOutcome>> = BTreeMap::new();
    for outcome in outcomes {
        per_mod
            .entry(outcome.mod_name.clone())
            .or_default()
            .push(outcome);
    }
    let per_mod = per_mod
        .into_iter()
        .map(|(mod_name, outcomes)| {
            let mut mod_scores = Vec::new();
            let mut mod_corpus = ChrFStatistics::default();
            let mut preserved = 0;
            for outcome in &outcomes {
                if let Some(translation) = outcome.translation.as_deref() {
                    preserved += 1;
                    if let Some(reference) = outcome.candidate.reference.as_deref() {
                        mod_scores.push(chrf(translation, reference, 6, 2.0));
                        mod_corpus.add(translation, reference, 6);
                    }
                }
            }
            mod_scores.sort_by(|left, right| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            });
            ModResult {
                mod_name,
                version: String::new(),
                candidates: outcomes.len(),
                sampled: outcomes.len(),
                with_reference: mod_scores.len(),
                speed: SpeedStats::default(),
                safety: SafetyStats {
                    total: outcomes.len(),
                    preserved,
                    ..SafetyStats::default()
                },
                chrf: ChrFStats {
                    entries: mod_scores.len(),
                    mean: if mod_scores.is_empty() {
                        0.0
                    } else {
                        mod_scores.iter().sum::<f64>() / mod_scores.len() as f64
                    },
                    corpus: mod_corpus.score(2.0),
                    median: if mod_scores.is_empty() {
                        0.0
                    } else {
                        mod_scores[mod_scores.len() / 2]
                    },
                },
            }
        })
        .collect();

    (speed, safety, chrf_stats, per_mod)
}

// ---------------------------------------------------------------------------
// サーバ起動
// ---------------------------------------------------------------------------

/// バックエンドの表示名。`Debug` だと `Cuda(Cuda13)` になり表が読みにくい。
fn backend_label(backend: Backend) -> String {
    match backend {
        Backend::Cpu => "CPU".to_owned(),
        Backend::Vulkan => "Vulkan".to_owned(),
        // 実際の Toolkit の版はビルドが持つ（12 系は Windows 12.4 / Linux 12.8 と違う）。
        Backend::Cuda(variant) => {
            match current_target_build(backend).and_then(|build| build.cuda) {
                Some(toolchain) => format!("CUDA {}", toolchain.toolkit_version),
                None => format!("CUDA {}", variant.as_str()),
            }
        }
    }
}

/// このマシンで使えるバックエンド。既定は CPU と Vulkan で、CUDA は検出できたときだけ足す。
fn available_backends() -> Vec<Backend> {
    let mut backends = vec![Backend::Cpu];
    if let Some(vulkan) = yc_llama::vulkan_backend() {
        backends.push(vulkan);
    }
    if let Some(cuda) = yc_llama::cuda_backend() {
        backends.push(cuda);
    }
    backends
}

/// 追加の比較（context / enclosed / glossary / 他言語）で使う GPU バックエンド。
///
/// これらは「GPU で 1 つ」を前提にした比較なので、指定された中で最初の GPU を使う。
/// CUDA を指定すれば CUDA で回る。
fn gpu_backend(args: &Args) -> Backend {
    args.backends
        .iter()
        .copied()
        .find(|backend| backend.is_gpu())
        .unwrap_or(Backend::Cpu)
}

/// ランタイムとモデルを用意し、`llama-server` を起動する。
async fn start_server(
    model: &PlamoModel,
    backend: Backend,
    slots: u8,
    ctx_per_slot: u32,
) -> Result<(LlamaServer, f64), String> {
    let cache = cache_root();
    let build = current_target_build(backend)
        .ok_or_else(|| format!("no pinned runtime for this target ({backend:?})"))?;
    let runtime_root = cache.join("runtimes");
    let server_path = ensure_runtime(build, &runtime_root, |_, _| {}, CancellationToken::new())
        .await
        .map_err(|error| format!("runtime: {error}"))?;

    let model_path = cache.join("models").join(model.file);
    if !model_path.is_file() {
        return Err(format!(
            "model missing at {} (run the download_models example first)",
            model_path.display()
        ));
    }

    let config = ServerConfig::new(server_path, model_path, backend)
        .with_slots(slots)
        .map_err(|error| format!("config: {error}"))?
        .with_ctx_per_slot(ctx_per_slot)
        .map_err(|error| format!("config: {error}"))?;

    let started = Instant::now();
    let server = LlamaServer::start(&config)
        .await
        .map_err(|error| format!("server start: {error}"))?;
    Ok((server, started.elapsed().as_secs_f64()))
}

// ---------------------------------------------------------------------------
// メイン
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StrategyChoice {
    Default,
    Verbatim,
    BracketedIndex,
    AsciiMarker,
    PairedTag,
}

impl StrategyChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Verbatim => "verbatim",
            Self::BracketedIndex => "bracketed_index",
            Self::AsciiMarker => "ascii_marker",
            Self::PairedTag => "paired_tag",
        }
    }

    fn strategy(self) -> ProtectStrategy {
        match self {
            Self::Default => ProtectStrategy::default(),
            Self::Verbatim => ProtectStrategy::Verbatim,
            Self::BracketedIndex => ProtectStrategy::BracketedIndex,
            Self::AsciiMarker => ProtectStrategy::AsciiMarker,
            Self::PairedTag => ProtectStrategy::PairedTag,
        }
    }

    /// provider に渡す方式。`Default` は profile ごとの既定に委ねるので `None`。
    fn provider_strategy(self) -> Option<ProtectStrategy> {
        match self {
            Self::Default => None,
            other => Some(other.strategy()),
        }
    }
}

/// 用語集の与え方。遵守率の比較に使う。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GlossaryMode {
    /// `glossary` 要素だけ（従来の既定）。
    ElementOnly,
    /// `glossary` 要素 + 履歴の `input`/`output` 対（新しい既定）。
    ElementAndHistory,
    /// 履歴の対だけ。
    HistoryOnly,
}

impl GlossaryMode {
    fn label(self) -> &'static str {
        match self {
            Self::ElementOnly => "glossary_element",
            Self::ElementAndHistory => "glossary_element_history",
            Self::HistoryOnly => "glossary_history",
        }
    }

    /// provider の設定へ落とす。`HistoryOnly` だけが `glossary` 要素を落とす。
    fn history_enabled(self) -> bool {
        !matches!(self, Self::ElementOnly)
    }

    /// `glossary` 要素を送るか。
    fn element_enabled(self) -> bool {
        !matches!(self, Self::HistoryOnly)
    }
}

/// 用語集・文脈の比較 1 条件分の設定。
struct Condition {
    include_context: bool,
    label: &'static str,
    mode: GlossaryMode,
    /// profile ごとの保護方式を使うか（`false` なら全 profile で `BracketedIndex`）。
    by_profile: bool,
    terms: Vec<(String, String)>,
}

struct Args {
    models: Vec<String>,
    backends: Vec<Backend>,
    strategies: Vec<StrategyChoice>,
    /// context と glossary の比較も回すか（2B GPU のみ）。
    context_study: bool,
    /// 書式コードに挟まれた本文の比較も回すか（2B GPU のみ）。
    enclosed_study: bool,
    /// 採用した既定での用語集遵守率を複数モデルで確認するか（GPU のみ）。
    ///
    /// `context_study` は 2B GPU で方式を選ぶための比較なので、0.8B / 8B では
    /// 用語集を渡さない。採用した方式が他のモデルでも効くかは別に測る必要がある。
    glossary_models_study: bool,
    /// 他言語の sanity も回すか。
    other_targets: bool,
    /// 出力ファイル名の接尾辞。実行を設定ごとに分けて流すときに付ける。
    tag: Option<String>,
    /// 1 mod あたりのサンプル上限の上書き。CPU で大きいモデルを回すときに縮める。
    sample_override: Option<usize>,
}

fn parse_args() -> Args {
    let mut args = Args {
        models: vec!["0.8b".to_owned(), "2b".to_owned(), "8b".to_owned()],
        backends: available_backends(),
        strategies: vec![StrategyChoice::Default],
        context_study: true,
        enclosed_study: true,
        glossary_models_study: true,
        other_targets: true,
        tag: None,
        sample_override: None,
    };
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < raw.len() {
        let flag = raw[index].as_str();
        let value = raw.get(index + 1).cloned().unwrap_or_default();
        match flag {
            "--models" => {
                args.models = value.split(',').map(str::to_owned).collect();
                index += 2;
            }
            "--backends" => {
                args.backends = value
                    .split(',')
                    .map(|name| match name.trim() {
                        // `gpu` は旧表記で Vulkan を指す。
                        "gpu" | "vulkan" => Backend::Vulkan,
                        "cuda" => yc_llama::cuda_backend()
                            .unwrap_or_else(|| panic!("CUDA is not available on this machine")),
                        _ => Backend::Cpu,
                    })
                    .collect();
                index += 2;
            }
            "--strategies" => {
                args.strategies = if value == "all" {
                    vec![
                        StrategyChoice::Verbatim,
                        StrategyChoice::BracketedIndex,
                        StrategyChoice::AsciiMarker,
                        StrategyChoice::PairedTag,
                    ]
                } else {
                    value
                        .split(',')
                        .map(|name| match name.trim() {
                            "verbatim" => StrategyChoice::Verbatim,
                            "bracketed" | "bracketed_index" => StrategyChoice::BracketedIndex,
                            "ascii" | "ascii_marker" => StrategyChoice::AsciiMarker,
                            "paired" | "paired_tag" => StrategyChoice::PairedTag,
                            _ => StrategyChoice::Default,
                        })
                        .collect()
                };
                index += 2;
            }
            "--no-context-study" => {
                args.context_study = false;
                index += 1;
            }
            "--no-enclosed-study" => {
                args.enclosed_study = false;
                index += 1;
            }
            "--no-glossary-models-study" => {
                args.glossary_models_study = false;
                index += 1;
            }
            "--tag" => {
                args.tag = Some(value);
                index += 2;
            }
            "--sample" => {
                args.sample_override = value.parse().ok();
                index += 2;
            }
            "--no-other-targets" => {
                args.other_targets = false;
                index += 1;
            }
            _ => index += 1,
        }
    }
    args
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = parse_args();
    let cache = cache_root();
    let mods_dir = cache.join("mods");
    let out_dir = cache.join("eval");
    std::fs::create_dir_all(&out_dir).map_err(|error| format!("{}: {error}", out_dir.display()))?;

    // mod を読み、サンプルを固定する。
    let mut datasets: Vec<(ModSpec, Vec<Candidate>)> = Vec::new();
    for spec in MODS {
        let candidates = load_candidates(&spec, &mods_dir)?;
        eprintln!(
            "{}: {} candidates ({} formatted, {} with ja_jp)",
            spec.display_name,
            candidates.len(),
            candidates.iter().filter(|item| item.formatted).count(),
            candidates
                .iter()
                .filter(|item| item.reference.is_some())
                .count()
        );
        datasets.push((spec, candidates));
    }

    let mut runs: Vec<RunResult> = Vec::new();
    let mut dumps: Vec<OutcomeDump> = Vec::new();

    for model_id in &args.models {
        let model = find_model(&format!("plamo-3-translate-{model_id}"))
            .or_else(|| {
                catalog::models()
                    .iter()
                    .find(|model| model.parameters.eq_ignore_ascii_case(model_id))
            })
            .ok_or_else(|| format!("unknown model: {model_id}"))?;

        for backend in &args.backends {
            // 8B の CPU は現実的な時間で終わらせるためサンプルを縮める。`--sample` は
            // それをさらに上書きする（CPU で 2B を回すときなど）。
            let budget = args.sample_override.unwrap_or(
                if model.parameters == "8B" && *backend == Backend::Cpu {
                    SAMPLE_CPU_8B
                } else {
                    SAMPLE_PLAIN + SAMPLE_FORMATTED_CAP
                },
            );
            let sampled: Vec<(String, String, Vec<Candidate>)> = datasets
                .iter()
                .map(|(spec, candidates)| {
                    (
                        spec.display_name.to_owned(),
                        spec.namespace.to_owned(),
                        sample(candidates, budget),
                    )
                })
                .collect();
            let total: usize = sampled.iter().map(|(_, _, items)| items.len()).sum();

            for strategy in &args.strategies {
                eprintln!(
                    "=== {} / {} / {} ({} entries) ===",
                    model.parameters,
                    backend_label(*backend),
                    strategy.label(),
                    total
                );
                let (server, load_seconds) = match start_server(model, *backend, 1, 8192).await {
                    Ok(started) => started,
                    Err(error) => {
                        eprintln!(
                            "skipping {} {}: {error}",
                            model.parameters,
                            backend_label(*backend)
                        );
                        continue;
                    }
                };
                let (provider, reports) = match provider_with_reports(
                    server.base_url(),
                    server.api_key(),
                    strategy.provider_strategy(),
                    8192,
                    true,
                    false,
                    true,
                ) {
                    Ok(created) => created,
                    Err(error) => {
                        eprintln!(
                            "skipping {} {}: {error}",
                            model.parameters,
                            backend_label(*backend)
                        );
                        server.shutdown().await;
                        continue;
                    }
                };

                let started = Instant::now();
                let sample = run_sample(
                    &provider,
                    &reports,
                    &sampled,
                    &[],
                    "ja_jp",
                    PlamoProviderOptions::default().timeout,
                    &server,
                )
                .await;
                let batch_errors = batch_error_labels(&sample.batch_failures);

                let batch_failures = sample.batch_failures;
                let server_restarts = sample.server_restarts;
                let outcomes = sample.entries;
                dumps.extend(dump_outcomes(
                    model.parameters,
                    &backend_label(*backend),
                    strategy.label(),
                    &outcomes,
                ));
                let wall_seconds = started.elapsed().as_secs_f64();
                let (speed, safety, chrf, per_mod) =
                    summarize(&outcomes, load_seconds, wall_seconds);

                // 決定性: 同じサンプルの先頭だけを 2 回目として流し直す。
                let subset: Vec<(String, String, Vec<Candidate>)> = sampled
                    .iter()
                    .map(|(name, ns, items)| {
                        (
                            name.clone(),
                            ns.clone(),
                            items
                                .iter()
                                .take(DETERMINISM_SAMPLE / sampled.len() + 1)
                                .cloned()
                                .collect(),
                        )
                    })
                    .collect();
                let again = run_sample(
                    &provider,
                    &reports,
                    &subset,
                    &[],
                    "ja_jp",
                    PlamoProviderOptions::default().timeout,
                    &server,
                )
                .await;
                let again = again.entries;
                // 同じ key でも `Lang` と `Patchouli` で別 entry になる（Ars Nouveau の
                // ガイドブック本文は lang にも載っている）。key だけで引くと取り違える。
                // `PlaceholderProfile` は `Ord` を持たないので、プロファイル名を混ぜた文字列にする。
                let identity = |outcome: &EntryOutcome| {
                    format!(
                        "{}\u{1}{}\u{1}{:?}",
                        outcome.mod_name, outcome.candidate.key, outcome.candidate.profile
                    )
                };
                let first: BTreeMap<_, String> = outcomes
                    .iter()
                    .filter_map(|outcome| {
                        outcome
                            .translation
                            .as_ref()
                            .map(|text| (identity(outcome), text.clone()))
                    })
                    .collect();
                let mut nondeterministic = 0;
                let mut checked = 0;
                for outcome in &again {
                    if let (Some(before), Some(after)) =
                        (first.get(&identity(outcome)), outcome.translation.as_ref())
                    {
                        checked += 1;
                        if before != after {
                            nondeterministic += 1;
                        }
                    }
                }

                let mut stability = StabilityStats {
                    nondeterministic_entries: nondeterministic,
                    determinism_checked: checked,
                    ..StabilityStats::from_safety(&safety)
                };
                stability.record_batch_failures(&batch_failures);
                stability.server_restarts = server_restarts;

                let (examples_good, examples_bad) = pick_examples(&outcomes, EXAMPLES_PER_MODEL);
                let mut per_mod = per_mod;
                for entry in &mut per_mod {
                    if let Some((spec, _)) = datasets
                        .iter()
                        .find(|(spec, _)| spec.display_name == entry.mod_name)
                    {
                        entry.version = spec.version.to_owned();
                    }
                }

                runs.push(RunResult {
                    model: model.parameters.to_owned(),
                    backend: backend_label(*backend),
                    strategy: strategy.label().to_owned(),
                    sample_size: outcomes.len(),
                    context_enabled: true,
                    glossary_terms: 0,
                    speed,
                    safety,
                    stability,
                    chrf,
                    enclosed: enclosed_stats(&outcomes),
                    glossary_applicable: 0,
                    glossary_adhered: 0,
                    glossary_affected_entries: 0,
                    glossary_affected_chrf: 0.0,
                    glossary_used: Vec::new(),
                    per_mod,
                    batch_errors,
                    examples_good,
                    examples_bad,
                });

                server.shutdown().await;
            }
        }
    }

    // context / glossary の比較（2B GPU のみ）。指定が無ければ既定で回す。
    if args.context_study
        && args
            .models
            .iter()
            .any(|model| model.eq_ignore_ascii_case("2b"))
        && args.backends.iter().any(|backend| backend.is_gpu())
        && let Some(model) = find_model("plamo-3-translate-2b")
    {
        let budget = SAMPLE_PLAIN + SAMPLE_FORMATTED_CAP;
        let sampled: Vec<(String, String, Vec<Candidate>)> = datasets
            .iter()
            .map(|(spec, candidates)| {
                (
                    spec.display_name.to_owned(),
                    spec.namespace.to_owned(),
                    sample(candidates, budget),
                )
            })
            .collect();

        // 参照対から用語集を作る。公式訳がある item 名だけを使い、サンプルに実際に
        // 現れる語（＝遵守率の分母に入る語）に絞る。20 語では分母が十数件しか無く、
        // 遵守率の差が偶然と区別できないため、40〜60 語まで増やす。
        let glossary = build_realistic_glossary(&sampled, 60);
        eprintln!("glossary: {} terms", glossary.len());

        let (server, load_seconds) = match start_server(model, gpu_backend(&args), 1, 8192).await {
            Ok(started) => started,
            Err(error) => {
                eprintln!("skipping context study: {error}");
                return finish(&runs, &dumps, &out_dir, args.tag.as_deref());
            }
        };
        // 文脈あり / なし / 用語集の与え方。文脈の有無は provider の `include_context` で
        // 切り替える（mod 名を空にするだけでは翻訳キーと種別ヒントが残ってしまう）。
        //
        // `legacy_baseline` は変更前の既定（全 profile で `BracketedIndex`、用語集は
        // `glossary` 要素だけ、履歴なし）。`default_after` が出荷する既定
        // （profile ごとの方式 + `glossary` 要素だけ、履歴なし）。before → after を
        // 同じサンプル・同じ用語集で比べるためにこの 2 つを先頭に置く。
        let conditions = vec![
            Condition {
                include_context: true,
                label: "legacy_baseline",
                mode: GlossaryMode::ElementOnly,
                by_profile: false,
                terms: glossary.clone(),
            },
            Condition {
                include_context: true,
                label: "default_after",
                mode: GlossaryMode::ElementOnly,
                by_profile: true,
                terms: glossary.clone(),
            },
            Condition {
                include_context: true,
                label: "context_on",
                mode: GlossaryMode::ElementOnly,
                by_profile: false,
                terms: Vec::new(),
            },
            Condition {
                include_context: false,
                label: "context_off",
                mode: GlossaryMode::ElementOnly,
                by_profile: false,
                terms: Vec::new(),
            },
            Condition {
                include_context: true,
                label: GlossaryMode::ElementAndHistory.label(),
                mode: GlossaryMode::ElementAndHistory,
                by_profile: false,
                terms: glossary.clone(),
            },
            Condition {
                include_context: true,
                label: GlossaryMode::HistoryOnly.label(),
                mode: GlossaryMode::HistoryOnly,
                by_profile: false,
                terms: glossary.clone(),
            },
        ];
        for Condition {
            include_context,
            label,
            mode,
            by_profile,
            terms,
        } in conditions
        {
            eprintln!(
                "=== 2B / {} / {label} ===",
                backend_label(gpu_backend(&args))
            );
            // provider は文脈の有無・履歴の有無・profile ごとの方式ごとに作り直す
            // （設定は構築時に固定される）。
            let (provider, reports) = provider_with_reports(
                server.base_url(),
                server.api_key(),
                if by_profile {
                    None
                } else {
                    Some(ProtectStrategy::BracketedIndex)
                },
                8192,
                include_context,
                mode.history_enabled(),
                mode.element_enabled(),
            )?;
            let started = Instant::now();
            let sample = run_sample(
                &provider,
                &reports,
                &sampled,
                &terms,
                "ja_jp",
                PlamoProviderOptions::default().timeout,
                &server,
            )
            .await;
            let batch_errors = batch_error_labels(&sample.batch_failures);

            let batch_failures = sample.batch_failures;
            let server_restarts = sample.server_restarts;
            let outcomes = sample.entries;
            dumps.extend(dump_outcomes(
                "2B",
                &backend_label(gpu_backend(&args)),
                label,
                &outcomes,
            ));
            let wall_seconds = started.elapsed().as_secs_f64();
            let (speed, safety, chrf, per_mod) = summarize(&outcomes, load_seconds, wall_seconds);
            let (glossary_applicable, glossary_adhered) = glossary_counts(&outcomes, &terms);
            let (glossary_affected_entries, glossary_affected_chrf) =
                glossary_affected_chrf(&outcomes, &terms);
            let enclosed = enclosed_stats(&outcomes);
            let (examples_good, examples_bad) = pick_examples(&outcomes, EXAMPLES_PER_MODEL);
            let mut stability = StabilityStats::from_safety(&safety);
            stability.record_batch_failures(&batch_failures);
            stability.server_restarts = server_restarts;
            runs.push(RunResult {
                model: "2B".to_owned(),
                backend: backend_label(gpu_backend(&args)),
                strategy: label.to_owned(),
                sample_size: outcomes.len(),
                context_enabled: include_context,
                glossary_terms: terms.len(),
                speed,
                safety,
                stability,
                chrf,
                enclosed,
                glossary_applicable,
                glossary_adhered,
                glossary_affected_entries,
                glossary_affected_chrf,
                glossary_used: terms.clone(),
                per_mod,
                batch_errors,
                examples_good,
                examples_bad,
            });
            eprintln!(
                "  corpus chrF: {:.4}, glossary adherence: {glossary_adhered}/{glossary_applicable}",
                runs.last().expect("just pushed").chrf.corpus
            );
        }
        server.shutdown().await;
    }

    // 書式コードに挟まれた本文の比較（2B GPU のみ）。strategy ごとに未訳率を測る。
    if args.enclosed_study
        && args
            .models
            .iter()
            .any(|model| model.eq_ignore_ascii_case("2b"))
        && args.backends.iter().any(|backend| backend.is_gpu())
        && let Some(model) = find_model("plamo-3-translate-2b")
    {
        // 対になった書式コードを持つ entry を全件使う。通常サンプルでは数件しか
        // 残らず、未訳率の分母が足りない。
        let sampled: Vec<(String, String, Vec<Candidate>)> = datasets
            .iter()
            .map(|(spec, candidates)| {
                (
                    spec.display_name.to_owned(),
                    spec.namespace.to_owned(),
                    enclosed_sample(candidates),
                )
            })
            .collect();
        let total: usize = sampled.iter().map(|(_, _, items)| items.len()).sum();
        eprintln!(
            "=== 2B / {} / enclosed study ({total} entries) ===",
            backend_label(gpu_backend(&args))
        );

        let (server, load_seconds) = match start_server(model, gpu_backend(&args), 1, 8192).await {
            Ok(started) => started,
            Err(error) => {
                eprintln!("skipping enclosed study: {error}");
                return finish(&runs, &dumps, &out_dir, args.tag.as_deref());
            }
        };
        for strategy in [
            StrategyChoice::Verbatim,
            StrategyChoice::BracketedIndex,
            StrategyChoice::AsciiMarker,
            StrategyChoice::PairedTag,
            StrategyChoice::Default,
        ] {
            let label = format!("enclosed_{}", strategy.label());
            eprintln!(
                "=== 2B / {} / {label} ===",
                backend_label(gpu_backend(&args))
            );
            let (provider, reports) = provider_with_reports(
                server.base_url(),
                server.api_key(),
                strategy.provider_strategy(),
                8192,
                true,
                false,
                true,
            )?;
            let started = Instant::now();
            let sample = run_sample(
                &provider,
                &reports,
                &sampled,
                &[],
                "ja_jp",
                PlamoProviderOptions::default().timeout,
                &server,
            )
            .await;
            let batch_errors = batch_error_labels(&sample.batch_failures);

            let batch_failures = sample.batch_failures;
            let server_restarts = sample.server_restarts;
            let outcomes = sample.entries;
            dumps.extend(dump_outcomes(
                "2B",
                &backend_label(gpu_backend(&args)),
                &label,
                &outcomes,
            ));
            let wall_seconds = started.elapsed().as_secs_f64();
            let (speed, safety, chrf, per_mod) = summarize(&outcomes, load_seconds, wall_seconds);
            let enclosed = enclosed_stats(&outcomes);
            let mut stability = StabilityStats::from_safety(&safety);
            stability.record_batch_failures(&batch_failures);
            stability.server_restarts = server_restarts;
            eprintln!(
                "  untranslated: {}/{}, entries {}/{}, chrF: {:.4}",
                enclosed.kept_verbatim,
                enclosed.translated_regions,
                enclosed.translated_entries,
                enclosed.entries,
                chrf.corpus
            );
            runs.push(RunResult {
                model: "2B".to_owned(),
                backend: backend_label(gpu_backend(&args)),
                strategy: label,
                sample_size: outcomes.len(),
                context_enabled: true,
                glossary_terms: 0,
                speed,
                safety,
                stability,
                chrf,
                enclosed,
                glossary_applicable: 0,
                glossary_adhered: 0,
                glossary_affected_entries: 0,
                glossary_affected_chrf: 0.0,
                glossary_used: Vec::new(),
                per_mod,
                batch_errors,
                examples_good: Vec::new(),
                examples_bad: Vec::new(),
            });
        }
        server.shutdown().await;
    }

    // 採用した既定での用語集遵守率を 0.8B / 2B / 8B GPU で確認する。
    //
    // `context_study` は 2B GPU で方式を選ぶための比較で、0.8B / 8B の通常実行は
    // 用語集を渡さない。2B で選んだ方式（`glossary` 要素だけ + profile ごとの保護方式）が
    // 他のモデルでも遵守率と chrF を保つかは、同じ条件で測らないと分からない。
    if args.glossary_models_study && args.backends.iter().any(|backend| backend.is_gpu()) {
        let budget = SAMPLE_PLAIN + SAMPLE_FORMATTED_CAP;
        let sampled: Vec<(String, String, Vec<Candidate>)> = datasets
            .iter()
            .map(|(spec, candidates)| {
                (
                    spec.display_name.to_owned(),
                    spec.namespace.to_owned(),
                    sample(candidates, budget),
                )
            })
            .collect();
        // 用語集は 2B の比較と同じ規則（サンプルに現れる語から 60 語）で作る。
        let glossary = build_realistic_glossary(&sampled, 60);
        eprintln!("=== glossary across models: {} terms ===", glossary.len());

        for model_id in &args.models {
            let Some(model) = find_model(&format!("plamo-3-translate-{model_id}")).or_else(|| {
                catalog::models()
                    .iter()
                    .find(|model| model.parameters.eq_ignore_ascii_case(model_id))
            }) else {
                continue;
            };
            eprintln!(
                "=== {} / {} / glossary_default ===",
                model.parameters,
                backend_label(gpu_backend(&args))
            );
            let (server, load_seconds) =
                match start_server(model, gpu_backend(&args), 1, 8192).await {
                    Ok(started) => started,
                    Err(error) => {
                        eprintln!("skipping {} glossary: {error}", model.parameters);
                        continue;
                    }
                };
            // 出荷する既定（profile ごとの保護方式、`glossary` 要素のみ、履歴なし）。
            let (provider, reports) = provider_with_reports(
                server.base_url(),
                server.api_key(),
                None,
                8192,
                true,
                false,
                true,
            )?;
            let started = Instant::now();
            let sample = run_sample(
                &provider,
                &reports,
                &sampled,
                &glossary,
                "ja_jp",
                PlamoProviderOptions::default().timeout,
                &server,
            )
            .await;
            let batch_errors = batch_error_labels(&sample.batch_failures);

            let batch_failures = sample.batch_failures;
            let server_restarts = sample.server_restarts;
            let outcomes = sample.entries;
            let label = "glossary_default";
            dumps.extend(dump_outcomes(
                model.parameters,
                &backend_label(gpu_backend(&args)),
                label,
                &outcomes,
            ));
            let wall_seconds = started.elapsed().as_secs_f64();
            let (speed, safety, chrf, per_mod) = summarize(&outcomes, load_seconds, wall_seconds);
            let (glossary_applicable, glossary_adhered) = glossary_counts(&outcomes, &glossary);
            let (glossary_affected_entries, glossary_affected_chrf) =
                glossary_affected_chrf(&outcomes, &glossary);
            let enclosed = enclosed_stats(&outcomes);
            let mut stability = StabilityStats::from_safety(&safety);
            stability.record_batch_failures(&batch_failures);
            stability.server_restarts = server_restarts;
            eprintln!(
                "  adherence: {glossary_adhered}/{glossary_applicable}, chrF: {:.4}",
                chrf.corpus
            );
            runs.push(RunResult {
                model: model.parameters.to_owned(),
                backend: backend_label(gpu_backend(&args)),
                strategy: label.to_owned(),
                sample_size: outcomes.len(),
                context_enabled: true,
                glossary_terms: glossary.len(),
                speed,
                safety,
                stability,
                chrf,
                enclosed,
                glossary_applicable,
                glossary_adhered,
                glossary_affected_entries,
                glossary_affected_chrf,
                glossary_used: glossary.clone(),
                per_mod,
                batch_errors,
                examples_good: Vec::new(),
                examples_bad: Vec::new(),
            });
            server.shutdown().await;
        }
    }

    if args.other_targets
        && let Some(model) = find_model("plamo-3-translate-2b")
    {
        let sampled: Vec<(String, String, Vec<Candidate>)> = datasets
            .iter()
            .map(|(spec, candidates)| {
                (
                    spec.display_name.to_owned(),
                    spec.namespace.to_owned(),
                    sample(candidates, SAMPLE_OTHER_TARGET)
                        .into_iter()
                        .take(6)
                        .collect(),
                )
            })
            .collect();
        let (server, load_seconds) = match start_server(model, gpu_backend(&args), 1, 8192).await {
            Ok(started) => started,
            Err(error) => {
                eprintln!("skipping other targets: {error}");
                return finish(&runs, &dumps, &out_dir, args.tag.as_deref());
            }
        };
        let (provider, reports) = provider_with_reports(
            server.base_url(),
            server.api_key(),
            None,
            8192,
            true,
            false,
            true,
        )?;
        for target in ["zh_cn", "ko_kr", "de_de", "pt_br"] {
            eprintln!(
                "=== 2B / {} / target {target} ===",
                backend_label(gpu_backend(&args))
            );
            let started = Instant::now();
            let sample = run_sample(
                &provider,
                &reports,
                &sampled,
                &[],
                target,
                PlamoProviderOptions::default().timeout,
                &server,
            )
            .await;
            let batch_errors = batch_error_labels(&sample.batch_failures);

            let batch_failures = sample.batch_failures;
            let server_restarts = sample.server_restarts;
            let outcomes = sample.entries;
            dumps.extend(dump_outcomes(
                "2B",
                &backend_label(gpu_backend(&args)),
                &format!("target_{target}"),
                &outcomes,
            ));
            let wall_seconds = started.elapsed().as_secs_f64();
            let (speed, safety, chrf, per_mod) = summarize(&outcomes, load_seconds, wall_seconds);
            let mut stability = StabilityStats::from_safety(&safety);
            stability.record_batch_failures(&batch_failures);
            stability.server_restarts = server_restarts;
            runs.push(RunResult {
                model: "2B".to_owned(),
                backend: backend_label(gpu_backend(&args)),
                strategy: format!("target_{target}"),
                sample_size: outcomes.len(),
                context_enabled: true,
                glossary_terms: 0,
                speed,
                safety,
                stability,
                chrf,
                enclosed: enclosed_stats(&outcomes),
                glossary_applicable: 0,
                glossary_adhered: 0,
                glossary_affected_entries: 0,
                glossary_affected_chrf: 0.0,
                glossary_used: Vec::new(),
                per_mod,
                batch_errors,
                examples_good: Vec::new(),
                examples_bad: Vec::new(),
            });
        }
        server.shutdown().await;
    }

    finish(&runs, &dumps, &out_dir, args.tag.as_deref())
}

fn count_failure(safety: &SafetyStats, reason: &str) -> usize {
    safety.failures.get(reason).copied().unwrap_or(0)
}

/// 参照訳から小さな用語集を作る。item 名らしい短い原文だけを選ぶ。
///
/// 実データに即した用語集が要る場合は [`build_realistic_glossary`] を使う。こちらは
/// 先頭から拾うだけなので、サンプルに現れない語が混ざりやすい。
#[allow(dead_code)]
fn build_glossary(datasets: &[(ModSpec, Vec<Candidate>)], limit: usize) -> Vec<(String, String)> {
    let mut terms = Vec::new();
    let mut seen = BTreeSet::new();
    for (_, candidates) in datasets {
        for candidate in candidates {
            if terms.len() >= limit {
                return terms;
            }
            let Some(reference) = candidate.reference.as_deref() else {
                continue;
            };
            // 1〜3 語の短い名前だけ。文や書式入りは用語集に向かない。
            let words = candidate.source.split_whitespace().count();
            if !(1..=3).contains(&words)
                || candidate.source.len() > 24
                || candidate.formatted
                || reference.len() > 24
                || candidate.source.contains('.')
            {
                continue;
            }
            if seen.insert(candidate.source.to_lowercase()) {
                terms.push((candidate.source.clone(), reference.to_owned()));
            }
        }
    }
    terms
}

/// 実データに即した用語集を作る。
///
/// 以前は 20 語を先頭から拾っていたため、サンプルに現れない語が混ざり、遵守率の
/// 分母が 36 件しか無かった（差が偶然と区別できない）。ここでは
/// 1. 公式訳がある短い名前だけを使い、
/// 2. **サンプルに実際に現れる語**（＝遵守率の分母に入る語）に絞り、
/// 3. 長い語を優先して 40〜60 語まで集める。
///
/// 2 が重要。全候補から選ぶと、サンプルが引かなかった語が大半を占め、分母が十数件に
/// なって差が読めない。サンプルから選べば、用語を渡した意味のある比較になる。
///
/// 3 の長い語優先は、`Iron` と `Iron Ingot` のような重なりで短い方が先に当たるのを
/// 避けるため（本番の `select_glossary_terms` も長い順に並べる）。
fn build_realistic_glossary(
    sampled: &[(String, String, Vec<Candidate>)],
    limit: usize,
) -> Vec<(String, String)> {
    // サンプル内の原文をまとめる。用語が実際に現れるかをここで判定する。
    let sources: Vec<String> = sampled
        .iter()
        .flat_map(|(_, _, entries)| entries.iter().map(|entry| entry.source.clone()))
        .collect();

    let mut candidates: Vec<(String, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    for (_, _, entries) in sampled {
        for candidate in entries {
            let Some(reference) = candidate.reference.as_deref() else {
                continue;
            };
            let words = candidate.source.split_whitespace().count();
            if !(1..=3).contains(&words)
                || candidate.source.len() > 24
                || candidate.formatted
                || reference.len() > 24
                || candidate.source.contains('.')
            {
                continue;
            }
            // 訳語が原文と同じ（固有名詞をそのまま残す）なら、遵守率の判定に使えない。
            if reference.eq_ignore_ascii_case(&candidate.source) {
                continue;
            }
            if seen.insert(candidate.source.to_lowercase()) {
                candidates.push((candidate.source.clone(), reference.to_owned()));
            }
        }
    }

    // サンプル内で 2 件以上の原文に現れる語だけを残す。1 件にしか出ない語は、その
    // entry が落ちると分母から消え、遵守率が数件で揺れる。
    let glossary: Vec<yc_ai::GlossaryTerm> = candidates
        .iter()
        .map(|(source, target)| yc_ai::GlossaryTerm {
            source_term: source.clone(),
            target_term: target.clone(),
        })
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for source in &sources {
        for (term, _) in &candidates {
            if yc_plamo::pfml3::select_glossary_terms(source, &glossary)
                .iter()
                .any(|(matched, _)| matched == term)
            {
                *counts.entry(term.clone()).or_insert(0) += 1;
            }
        }
    }
    candidates.retain(|(term, _)| counts.get(term).copied().unwrap_or(0) >= 2);
    // 長い語を先に。同じ長さなら元の並びを保つ（`sort_by_key` は安定）。
    candidates.sort_by_key(|entry| std::cmp::Reverse(entry.0.chars().count()));
    candidates.truncate(limit);
    candidates
}

/// 用語集の遵守率。原文に用語が現れた entry のうち、訳文に訳語が入った割合。
///
/// 照合はプロダクト側（`yc_plamo::pfml3::select_glossary_terms`）と同じ規則を使う。
/// ここだけ部分一致にすると、製品が送っていない語を「遵守すべき語」として数えてしまう。
#[allow(dead_code)]
fn glossary_adherence(outcomes: &[EntryOutcome], terms: &[(String, String)]) -> f64 {
    let (applicable, adhered) = glossary_counts(outcomes, terms);
    if applicable == 0 {
        0.0
    } else {
        adhered as f64 / applicable as f64
    }
}
/// 遵守率の分子・分母。表に件数も出すために分けて返す。
///
/// 原文に用語が現れる entry を**すべて**対象にする（原文が用語そのものの entry も、
/// 訳文が得られなかった entry も含む）。以前は両方を分母から除いていたが、依頼された
/// 定義は「原文に用語が現れる entry」なので除外しない。訳文が無い entry は
/// **未遵守**として数える（訳語を出せなかったのだから、遵守していない）。
fn glossary_counts(outcomes: &[EntryOutcome], terms: &[(String, String)]) -> (usize, usize) {
    if terms.is_empty() {
        return (0, 0);
    }
    let glossary: Vec<yc_ai::GlossaryTerm> = terms
        .iter()
        .map(|(source, target)| yc_ai::GlossaryTerm {
            source_term: source.clone(),
            target_term: target.clone(),
        })
        .collect();
    let mut applicable = 0;
    let mut adhered = 0;
    for outcome in outcomes {
        let translation = outcome.translation.as_deref();
        for (_, target_term) in
            yc_plamo::pfml3::select_glossary_terms(&outcome.candidate.source, &glossary)
        {
            applicable += 1;
            // 訳文が無ければ未遵守。分母から外すと失敗が遵守率に出ない。
            if translation.is_some_and(|text| text.contains(target_term.as_str())) {
                adhered += 1;
            }
        }
    }
    (applicable, adhered)
}

/// 用語が原文に現れる entry（affected entries）のコーパス chrF。
///
/// 全体の chrF は用語と無関係な entry にも薄まるため、用語集の効果を見るには
/// 該当 entry だけの chrF が要る。複数の用語が当たる entry も 1 回だけ数える。
/// 参照訳があり、かつ訳文が得られた entry だけを対象にする（chrF の定義上）。
fn glossary_affected_chrf(outcomes: &[EntryOutcome], terms: &[(String, String)]) -> (usize, f64) {
    if terms.is_empty() {
        return (0, 0.0);
    }
    let glossary: Vec<yc_ai::GlossaryTerm> = terms
        .iter()
        .map(|(source, target)| yc_ai::GlossaryTerm {
            source_term: source.clone(),
            target_term: target.clone(),
        })
        .collect();
    let mut corpus = ChrFStatistics::default();
    let mut entries = 0;
    for outcome in outcomes {
        if yc_plamo::pfml3::select_glossary_terms(&outcome.candidate.source, &glossary).is_empty() {
            continue;
        }
        let (Some(translation), Some(reference)) = (
            outcome.translation.as_deref(),
            outcome.candidate.reference.as_deref(),
        ) else {
            continue;
        };
        corpus.add(translation, reference, 6);
        entries += 1;
    }
    (entries, corpus.score(2.0))
}

/// 代表例を chrF の上下から拾う。
fn pick_examples(outcomes: &[EntryOutcome], limit: usize) -> (Vec<Example>, Vec<Example>) {
    let mut scored: Vec<(f64, Example)> = outcomes
        .iter()
        .filter_map(|outcome| {
            let translation = outcome.translation.as_deref()?;
            let reference = outcome.candidate.reference.as_deref()?;
            let score = chrf(translation, reference, 6, 2.0);
            Some((
                score,
                Example {
                    mod_name: outcome.mod_name.clone(),
                    key: outcome.candidate.key.clone(),
                    source: outcome.candidate.source.clone(),
                    translation: translation.to_owned(),
                    reference: Some(reference.to_owned()),
                    chrf: Some(score),
                },
            ))
        })
        .collect();
    scored.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let half = limit / 2;
    let bad: Vec<Example> = scored.iter().take(half).map(|(_, ex)| ex.clone()).collect();
    let good: Vec<Example> = scored
        .iter()
        .rev()
        .take(half)
        .map(|(_, ex)| ex.clone())
        .collect();
    (good, bad)
}

/// 結果を書き出す。
///
/// 実行を設定ごとに分けて流せるよう、`--tag` で名前を付けたファイルにも書く。全体の
/// 集計は `eval_results.json` / `eval_outcomes.json` に上書きされるため、複数回に分けて
/// 回したときは `eval_results_<tag>.json` を後でまとめる。
fn finish(
    runs: &[RunResult],
    dumps: &[OutcomeDump],
    out_dir: &Path,
    tag: Option<&str>,
) -> Result<(), String> {
    let json = serde_json::to_string_pretty(runs).map_err(|error| error.to_string())?;
    let json_path = out_dir.join(match tag {
        Some(tag) => format!("eval_results_{tag}.json"),
        None => "eval_results.json".to_owned(),
    });
    std::fs::write(&json_path, &json)
        .map_err(|error| format!("{}: {error}", json_path.display()))?;
    // 全 entry の生データ。後処理の検討に使うので、集計とは別ファイルにする。
    let dump = serde_json::to_string(&dumps).map_err(|error| error.to_string())?;
    let dump_path = out_dir.join(match tag {
        Some(tag) => format!("eval_outcomes_{tag}.json"),
        None => "eval_outcomes.json".to_owned(),
    });
    std::fs::write(&dump_path, &dump)
        .map_err(|error| format!("{}: {error}", dump_path.display()))?;
    let markdown = render_markdown(runs);
    let md_path = out_dir.join(match tag {
        Some(tag) => format!("eval_results_{tag}.md"),
        None => "eval_results.md".to_owned(),
    });
    std::fs::write(&md_path, &markdown)
        .map_err(|error| format!("{}: {error}", md_path.display()))?;
    println!("{markdown}");
    eprintln!(
        "wrote {}, {} and {}",
        json_path.display(),
        dump_path.display(),
        md_path.display()
    );
    Ok(())
}

/// entry ごとの生データを集計用に平らにする。
fn dump_outcomes(
    model: &str,
    backend: &str,
    strategy: &str,
    outcomes: &[EntryOutcome],
) -> Vec<OutcomeDump> {
    outcomes
        .iter()
        .map(|outcome| {
            let chrf_score = outcome.translation.as_deref().and_then(|translation| {
                outcome
                    .candidate
                    .reference
                    .as_deref()
                    .map(|reference| chrf(translation, reference, 6, 2.0))
            });
            OutcomeDump {
                model: model.to_owned(),
                backend: backend.to_owned(),
                strategy: strategy.to_owned(),
                mod_name: outcome.mod_name.clone(),
                key: outcome.candidate.key.clone(),
                source: outcome.candidate.source.clone(),
                translation: outcome.translation.clone(),
                reference: outcome.candidate.reference.clone(),
                failure: outcome.failure.clone(),
                raw_output: outcome
                    .report
                    .as_ref()
                    .map(|report| report.raw_output.clone()),
                n_predict: outcome.report.as_ref().map_or(0, |report| report.n_predict),
                source_tokens: outcome
                    .report
                    .as_ref()
                    .map_or(0, |report| report.source_tokens),
                generated_tokens: outcome
                    .report
                    .as_ref()
                    .map_or(0, |report| report.generated_tokens),
                chrf: chrf_score,
            }
        })
        .collect()
}

fn render_markdown(runs: &[RunResult]) -> String {
    let mut out = String::new();
    out.push_str("## 実行結果\n\n");
    out.push_str("| model | backend | strategy | n | entries/s | tok/s | load s | wall s | preserved | chrF (corpus) | chrF (mean) |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");
    for run in runs {
        let preserved = if run.safety.total == 0 {
            0.0
        } else {
            run.safety.preserved as f64 / run.safety.total as f64 * 100.0
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.2} | {:.1} | {:.1} | {:.1} | {:.1}% | {:.4} | {:.4} |\n",
            run.model,
            run.backend,
            run.strategy,
            run.sample_size,
            run.speed.entries_per_second,
            run.speed.tokens_per_second_mean,
            run.speed.load_seconds,
            run.speed.wall_seconds,
            preserved,
            run.chrf.corpus,
            run.chrf.mean,
        ));
    }

    // 書式コードに挟まれた本文の未訳率。対象 entry が 1 件も無い実行は出さない。
    let enclosed_runs: Vec<&RunResult> =
        runs.iter().filter(|run| run.enclosed.regions > 0).collect();
    if !enclosed_runs.is_empty() {
        out.push_str("\n### 書式コードに挟まれた本文の未訳率\n\n");
        out.push_str(
            "| model | backend | strategy | entries | 訳文あり | regions | 未訳 | 未訳率 | 参照訳も未訳 | 保護区間の保持 | chrF (corpus) |\n",
        );
        out.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");
        for run in &enclosed_runs {
            let untranslated = run.enclosed.kept_verbatim;
            // 未訳率は「訳文が得られた本文」を分母にする。訳文が無い entry は
            // 未訳ではなく失敗として `訳文あり` 列と保持率に出る。
            let rate = if run.enclosed.translated_regions == 0 {
                0.0
            } else {
                untranslated as f64 / run.enclosed.translated_regions as f64 * 100.0
            };
            // 保持率の分母は「訳文が得られた entry」。訳文が無い entry は保持を
            // 確認できていないので、分子にも分母にも入れない（`entries` との差が失敗数）。
            let preserved = if run.enclosed.spans_total == 0 {
                0.0
            } else {
                run.enclosed.spans_preserved as f64 / run.enclosed.spans_total as f64 * 100.0
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {:.1}% | {} | {:.1}% | {:.4} |\n",
                run.model,
                run.backend,
                run.strategy,
                run.enclosed.entries,
                run.enclosed.translated_entries,
                run.enclosed.translated_regions,
                untranslated,
                rate,
                run.enclosed.legitimately_kept,
                preserved,
                run.enclosed.corpus_chrf,
            ));
        }
    }

    // 用語集の遵守率。用語を渡した実行だけを出す。
    let glossary_runs: Vec<&RunResult> = runs.iter().filter(|run| run.glossary_terms > 0).collect();
    if !glossary_runs.is_empty() {
        out.push_str("\n### 用語集の遵守率\n\n");
        out.push_str(
            "| model | backend | strategy | terms | 遵守 | 対象 | 遵守率 | chrF (corpus, 全体) | chrF (corpus, 用語該当) | 該当 entry |\n",
        );
        out.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
        for run in &glossary_runs {
            let rate = if run.glossary_applicable == 0 {
                0.0
            } else {
                run.glossary_adhered as f64 / run.glossary_applicable as f64 * 100.0
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {:.1}% | {:.4} | {:.4} | {} |\n",
                run.model,
                run.backend,
                run.strategy,
                run.glossary_terms,
                run.glossary_adhered,
                run.glossary_applicable,
                rate,
                run.chrf.corpus,
                run.glossary_affected_chrf,
                run.glossary_affected_entries,
            ));
        }
    }

    out.push_str("\n### 失敗理由の内訳\n\n");
    out.push_str(
        "| model | backend | strategy | empty | limit | too_long | format | unsafe | lang | server | batch | timeout | restarts | lost |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for run in runs {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            run.model,
            run.backend,
            run.strategy,
            run.stability.empty_outputs,
            run.stability.output_limits,
            run.stability.prompt_too_long,
            run.stability.format_mismatch,
            run.stability.unsafe_inputs,
            run.stability.unsupported_language,
            run.stability.server_errors,
            run.stability.batch_failures,
            run.stability.timeouts,
            run.stability.server_restarts,
            run.stability.entries_lost_to_batch_failure,
        ));
    }

    // 呼び出し全体を落としたエラーは、entry ごとの失敗とは別に名前で残す。
    let failed_batches: Vec<&RunResult> = runs
        .iter()
        .filter(|run| !run.batch_errors.is_empty())
        .collect();
    if !failed_batches.is_empty() {
        out.push_str("\n### 呼び出し全体の失敗\n\n");
        out.push_str("| model | backend | strategy | error |\n");
        out.push_str("|---|---|---|---|\n");
        for run in failed_batches {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                run.model,
                run.backend,
                run.strategy,
                run.batch_errors.join(", "),
            ));
        }
    }

    out.push_str("\n### mod 別 chrF\n\n");
    out.push_str("| model | backend | strategy | mod | n | chrF (corpus) | chrF (mean) |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    for run in runs {
        for entry in &run.per_mod {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:.4} | {:.4} |\n",
                run.model,
                run.backend,
                run.strategy,
                entry.mod_name,
                entry.chrf.entries,
                entry.chrf.corpus,
                entry.chrf.mean,
            ));
        }
    }
    out
}
