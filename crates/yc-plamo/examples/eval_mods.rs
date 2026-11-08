//! 実在の Minecraft mod を使った PLaMo 3 Translate の評価ハーネス。
//!
//! アプリと同じ経路（[`yc_ai::TranslationEntry::new_with_profile`] →
//! [`yc_plamo::PlamoProvider::translate_batch`]）を通すので、ここで測った数字は
//! そのまま製品経路の数字になる。サーバは `yc_llama` で起動する。
//!
//! 実行例:
//!
//! ```text
//! # 全部（0.8B/2B/8B × CPU/GPU）
//! cargo run --release -p yc-plamo --example eval_mods
//! # 一部だけ
//! cargo run --release -p yc-plamo --example eval_mods -- --models 2b --backends gpu
//! # strategy の比較
//! cargo run --release -p yc-plamo --example eval_mods -- --models 2b --backends gpu --strategies all
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

// ---------------------------------------------------------------------------
// chrF
// ---------------------------------------------------------------------------

/// chrF（文字 n-gram の F スコア）。
///
/// 実装は標準の chrF（Popović 2015）に合わせる。次数ごとの適合率・再現率を**先に平均**し、
/// その平均から β-F 値を 1 回だけ計算する。次数ごとに F を出して平均すると別の指標に
/// なってしまう（短い n と長い n の重みが変わってしまう）。
///
/// 空白は除いた文字 n-gram を n = 1..=`max_n` で数える。日本語のように単語分割が要らない
/// 比較に向く。`beta` は再現率の重み（chrF2 なら 2.0）。
fn chrf(hypothesis: &str, reference: &str, max_n: usize, beta: f64) -> f64 {
    let hypothesis: Vec<char> = hypothesis.chars().filter(|c| !c.is_whitespace()).collect();
    let reference: Vec<char> = reference.chars().filter(|c| !c.is_whitespace()).collect();
    if hypothesis.is_empty() || reference.is_empty() {
        return 0.0;
    }
    let mut precision_sum = 0.0;
    let mut recall_sum = 0.0;
    let mut counted = 0;
    for n in 1..=max_n {
        let hyp = ngram_counts(&hypothesis, n);
        let refr = ngram_counts(&reference, n);
        let mut overlap = 0usize;
        for (gram, count) in &hyp {
            if let Some(other) = refr.get(gram) {
                overlap += (*count).min(*other);
            }
        }
        let hyp_total: usize = hyp.values().sum();
        let ref_total: usize = refr.values().sum();
        if hyp_total == 0 || ref_total == 0 {
            continue;
        }
        precision_sum += overlap as f64 / hyp_total as f64;
        recall_sum += overlap as f64 / ref_total as f64;
        counted += 1;
    }
    if counted == 0 {
        return 0.0;
    }
    // 次数の平均を取ってから β-F を 1 回計算する。
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
    mean: f64,
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
fn provider_with_reports(
    base_url: &str,
    api_key: &str,
    strategy: ProtectStrategy,
    context_size: u32,
    include_context: bool,
) -> Result<(PlamoProvider, ReportSink), String> {
    let reports: ReportSink = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    let provider = PlamoProvider::new(
        base_url,
        Some(api_key.to_owned()),
        PlamoProviderOptions {
            protect_strategy: strategy,
            context_size: Some(context_size),
            include_context,
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
    let mut generated_tokens = 0_u64;
    let mut tps_sum = 0.0_f64;
    let mut tps_count = 0_usize;

    for outcome in outcomes {
        match outcome.translation.as_deref() {
            Some(translation) => {
                safety.preserved += 1;
                if let Some(reference) = outcome.candidate.reference.as_deref() {
                    scores.push(chrf(translation, reference, 6, 2.0));
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
            let mut preserved = 0;
            for outcome in &outcomes {
                if let Some(translation) = outcome.translation.as_deref() {
                    preserved += 1;
                    if let Some(reference) = outcome.candidate.reference.as_deref() {
                        mod_scores.push(chrf(translation, reference, 6, 2.0));
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
}

impl StrategyChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Verbatim => "verbatim",
            Self::BracketedIndex => "bracketed_index",
            Self::AsciiMarker => "ascii_marker",
        }
    }

    fn strategy(self) -> ProtectStrategy {
        match self {
            Self::Default => ProtectStrategy::default(),
            Self::Verbatim => ProtectStrategy::Verbatim,
            Self::BracketedIndex => ProtectStrategy::BracketedIndex,
            Self::AsciiMarker => ProtectStrategy::AsciiMarker,
        }
    }
}

struct Args {
    models: Vec<String>,
    backends: Vec<Backend>,
    strategies: Vec<StrategyChoice>,
    /// context と glossary の比較も回すか（2B GPU のみ）。
    context_study: bool,
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
        backends: vec![Backend::Cpu, Backend::Gpu],
        strategies: vec![StrategyChoice::Default],
        context_study: true,
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
                        "gpu" | "vulkan" => Backend::Gpu,
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
                    ]
                } else {
                    value
                        .split(',')
                        .map(|name| match name.trim() {
                            "verbatim" => StrategyChoice::Verbatim,
                            "bracketed" | "bracketed_index" => StrategyChoice::BracketedIndex,
                            "ascii" | "ascii_marker" => StrategyChoice::AsciiMarker,
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
                    "=== {} / {:?} / {} ({} entries) ===",
                    model.parameters,
                    backend,
                    strategy.label(),
                    total
                );
                let (server, load_seconds) = match start_server(model, *backend, 1, 8192).await {
                    Ok(started) => started,
                    Err(error) => {
                        eprintln!("skipping {} {:?}: {error}", model.parameters, backend);
                        continue;
                    }
                };
                let (provider, reports) = match provider_with_reports(
                    server.base_url(),
                    server.api_key(),
                    strategy.strategy(),
                    8192,
                    true,
                ) {
                    Ok(created) => created,
                    Err(error) => {
                        eprintln!("skipping {} {:?}: {error}", model.parameters, backend);
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
                    &format!("{backend:?}"),
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
                    backend: format!("{backend:?}"),
                    strategy: strategy.label().to_owned(),
                    sample_size: outcomes.len(),
                    context_enabled: true,
                    glossary_terms: 0,
                    speed,
                    safety,
                    stability,
                    chrf,
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
        && args.backends.contains(&Backend::Gpu)
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

        // 参照対から小さな用語集を作る。公式訳がある item 名だけを使う。
        let glossary = build_glossary(&datasets, 20);
        eprintln!("glossary: {} terms", glossary.len());

        let (server, load_seconds) = match start_server(model, Backend::Gpu, 1, 8192).await {
            Ok(started) => started,
            Err(error) => {
                eprintln!("skipping context study: {error}");
                return finish(&runs, &dumps, &out_dir, args.tag.as_deref());
            }
        };
        // 文脈あり / なし / あり+用語集。文脈の有無は provider の `include_context` で
        // 切り替える（mod 名を空にするだけでは翻訳キーと種別ヒントが残ってしまう）。
        for (include_context, terms) in [
            (true, Vec::new()),
            (false, Vec::new()),
            (true, glossary.clone()),
        ] {
            let label = match (include_context, terms.is_empty()) {
                (true, true) => "context_on",
                (false, _) => "context_off",
                (true, false) => "context_on_glossary",
            };
            eprintln!("=== 2B / Gpu / {label} ===");
            // provider は文脈の有無ごとに作り直す（設定は構築時に固定される）。
            let (provider, reports) = provider_with_reports(
                server.base_url(),
                server.api_key(),
                ProtectStrategy::default(),
                8192,
                include_context,
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
            dumps.extend(dump_outcomes("2B", "Gpu", label, &outcomes));
            let wall_seconds = started.elapsed().as_secs_f64();
            let (speed, safety, chrf, per_mod) = summarize(&outcomes, load_seconds, wall_seconds);
            let adherence = glossary_adherence(&outcomes, &terms);
            let (examples_good, examples_bad) = pick_examples(&outcomes, EXAMPLES_PER_MODEL);
            let mut stability = StabilityStats::from_safety(&safety);
            stability.record_batch_failures(&batch_failures);
            stability.server_restarts = server_restarts;
            runs.push(RunResult {
                model: "2B".to_owned(),
                backend: "Gpu".to_owned(),
                strategy: label.to_owned(),
                sample_size: outcomes.len(),
                context_enabled: include_context,
                glossary_terms: terms.len(),
                speed,
                safety,
                stability,
                chrf,
                per_mod,
                batch_errors,
                examples_good,
                examples_bad,
            });
            eprintln!("  glossary adherence: {adherence:.3}");
        }
        server.shutdown().await;
    }

    // 他言語の sanity。
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
        let (server, load_seconds) = match start_server(model, Backend::Gpu, 1, 8192).await {
            Ok(started) => started,
            Err(error) => {
                eprintln!("skipping other targets: {error}");
                return finish(&runs, &dumps, &out_dir, args.tag.as_deref());
            }
        };
        let (provider, reports) = provider_with_reports(
            server.base_url(),
            server.api_key(),
            ProtectStrategy::default(),
            8192,
            true,
        )?;
        for target in ["zh_cn", "ko_kr", "de_de", "pt_br"] {
            eprintln!("=== 2B / Gpu / target {target} ===");
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
                "Gpu",
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
                backend: "Gpu".to_owned(),
                strategy: format!("target_{target}"),
                sample_size: outcomes.len(),
                context_enabled: true,
                glossary_terms: 0,
                speed,
                safety,
                stability,
                chrf,
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

/// 用語集の遵守率。原文に用語が現れた entry のうち、訳文に訳語が入った割合。
fn glossary_adherence(outcomes: &[EntryOutcome], terms: &[(String, String)]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let mut applicable = 0;
    let mut adhered = 0;
    for outcome in outcomes {
        let Some(translation) = outcome.translation.as_deref() else {
            continue;
        };
        let source = outcome.candidate.source.to_lowercase();
        for (source_term, target_term) in terms {
            if !source.contains(&source_term.to_lowercase()) {
                continue;
            }
            applicable += 1;
            if translation.contains(target_term.as_str()) {
                adhered += 1;
            }
        }
    }
    if applicable == 0 {
        0.0
    } else {
        adhered as f64 / applicable as f64
    }
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
    out.push_str("| model | backend | strategy | n | entries/s | tok/s | load s | wall s | preserved | chrF |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    for run in runs {
        let preserved = if run.safety.total == 0 {
            0.0
        } else {
            run.safety.preserved as f64 / run.safety.total as f64 * 100.0
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.2} | {:.1} | {:.1} | {:.1} | {:.1}% | {:.4} |\n",
            run.model,
            run.backend,
            run.strategy,
            run.sample_size,
            run.speed.entries_per_second,
            run.speed.tokens_per_second_mean,
            run.speed.load_seconds,
            run.speed.wall_seconds,
            preserved,
            run.chrf.mean,
        ));
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
    out.push_str("| model | backend | strategy | mod | n | chrF |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for run in runs {
        for entry in &run.per_mod {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:.4} |\n",
                run.model,
                run.backend,
                run.strategy,
                entry.mod_name,
                entry.chrf.entries,
                entry.chrf.mean,
            ));
        }
    }
    out
}
