//! `llama-server` 上の PLaMo 3 Translate を呼ぶ Provider。
//!
//! サーバの起動とモデルのダウンロードは別クレート (`yc-llama`) の責務で、ここは
//! 起動済みサーバの base URL と API key だけを受け取る。

use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::{Client, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;
use yc_ai::{
    AiError, ProviderCapabilities, Result, TranslationContext, TranslationEntry,
    TranslationProvider,
};
use yc_validation::AiTranslation;

use crate::{
    languages,
    pfml3::{self, ContextParts, Pfml3Error, PromptRequest},
    protect::{ProtectError, ProtectStrategy, ProtectedSource},
};

/// 生成トークン数の上限。原文が長くても暴走しないための最終的な歯止め。
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 1024;

/// `max_output_tokens` の下限。
///
/// 短い原文でも文が途中で切れないだけの余裕を確保する。これ未満の設定は
/// `n_predict` の上限として意味を成さないため、Provider の構築時に拒否する。
pub const MIN_OUTPUT_TOKENS: u32 = 64;

/// `/completion` の 1 リクエストあたりの待ち時間。CPU 推論の長文を許容する。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// entry がシェーダー設定の項目名かどうかを判定する述語。
///
/// `yc_ai::TranslationEntry` はシェーダー設定かどうかを持たず、`TranslationProvider` の
/// シグネチャも固定なので、Provider 側に判定を注入する。シェーダーパックの解析結果を
/// 知っている呼び出し側だけが設定する。
pub type ShaderOptionDetector = Arc<dyn Fn(&TranslationEntry) -> bool + Send + Sync>;

/// entry 1 件の処理結果。評価ハーネスと進捗表示のための観測用。
///
/// [`TranslationProvider::translate_batch`] は成功した訳文しか返さないため、失敗の内訳や
/// 生成速度は外から分からない。評価で「何がどれだけ落ちたか」を測るには実際の経路の
/// 内側から観測する必要があるので、結果をそのまま渡す口を用意する。
#[derive(Clone, Debug)]
pub struct EntryReport {
    pub entry_id: i64,
    /// 成功した strategy。全滅した場合は最後に試した strategy。
    pub strategy: ProtectStrategy,
    /// 成功時は `None`。失敗時は Provider が付けた理由コード。
    pub failure: Option<&'static str>,
    /// 成功した試行回数（1 始まり）。2 なら代替 strategy で成功した。
    pub attempts: u32,
    pub prompt_tokens: u32,
    pub source_tokens: u32,
    pub n_predict: u32,
    /// サーバが報告した生成トークン数。timings が無ければ 0。
    pub generated_tokens: u32,
    /// サーバが報告した生成フェーズの速度（トークン/秒）。timings が無ければ 0。
    pub tokens_per_second: f32,
    pub stop_type: Option<String>,
    pub truncated: bool,
    /// 最後の試行の所要時間。
    pub elapsed: Duration,
    /// 復元前のモデル出力。代表例の抽出に使う。
    pub raw_output: String,
}

impl EntryReport {
    /// 計測値が何も無い状態の report。早期失敗の組み立てに使う。
    fn default(entry_id: i64) -> Self {
        Self {
            entry_id,
            strategy: ProtectStrategy::default(),
            failure: None,
            attempts: 0,
            prompt_tokens: 0,
            source_tokens: 0,
            n_predict: 0,
            generated_tokens: 0,
            tokens_per_second: 0.0,
            stop_type: None,
            truncated: false,
            elapsed: Duration::ZERO,
            raw_output: String::new(),
        }
    }
}

/// entry ごとの結果を受け取るコールバック。
pub type EntryObserver = Arc<dyn Fn(&EntryReport) + Send + Sync>;

/// Provider の調整項目。
#[derive(Clone)]
pub struct PlamoProviderOptions {
    /// `n_predict` の上限。
    pub max_output_tokens: u32,
    /// スロットあたりのコンテキスト長。`None` なら `/props` から取得してキャッシュする。
    pub context_size: Option<u32>,
    /// 全 profile に共通で使うマスク方式。既定は [`ProtectStrategy::BracketedIndex`]。
    ///
    /// [`Self::protect_strategy_by_profile`] が `true`（既定）の間は使われない。
    /// この値を効かせるには、そちらを `false` にする。
    pub protect_strategy: ProtectStrategy,
    /// profile ごとのマスク方式を選ぶか。既定は `true`。
    ///
    /// `true` のとき [`ProtectStrategy::for_profile`] が entry の `format_profile` から
    /// 方式を決める（Patchouli は `PairedTag`、それ以外は `BracketedIndex`）。
    /// `false` にすると [`Self::protect_strategy`] を全 profile に使う（従来の挙動）。
    ///
    /// 実測: `$(item)heat$()` のような Patchouli マクロは `PairedTag` でないと内側の
    /// `heat` が訳されず残る。`§7(Hidden)` は逆に `BracketedIndex` の方が訳される。
    pub protect_strategy_by_profile: bool,
    /// entry ごとのシェーダー設定判定。`None` なら該当なしとして扱う。
    pub shader_option_detector: Option<ShaderOptionDetector>,
    /// entry ごとの結果を受け取る観測フック。`None` なら何もしない。
    ///
    /// 評価ハーネスと、失敗理由を UI へ出したい呼び出し側のための口。翻訳の判断には
    /// 影響しない（戻り値を持たない）。
    pub entry_observer: Option<EntryObserver>,
    /// 文脈要素をプロンプトへ入れるか。既定は `true`。
    ///
    /// `false` にすると `context` 要素を丸ごと落とす（mod 名・namespace・翻訳キー・
    /// entry 種別のヒントすべて）。`TranslationContext` の mod 名を空にしても翻訳キーと
    /// 種別ヒントは残るため、文脈の効果を測るにはこの設定が要る。
    pub include_context: bool,
    /// 用語集の語を履歴の `input`/`output` 対としても与えるか。既定は `false`。
    ///
    /// 実測（2B GPU、1,200 entry、用語集 60 語、対象 264 件）で、`glossary` 要素だけの
    /// 遵守率は 93.2%、履歴を併用すると 89.4%、履歴だけなら 79.2% だった。
    /// chrF も 0.4245 対 0.4242 対 0.4220 で履歴側が低い。履歴はプロンプト長を
    /// 消費するだけで改善しないため既定では切る。評価で条件を変えるときのために口だけ残す。
    pub glossary_history: bool,
    /// `glossary` 要素をプロンプトへ入れるか。既定は `true`。
    ///
    /// `false` にすると用語集の語を `glossary` 要素として送らない。履歴だけを与えた
    /// 場合（`glossary_history = true` + これが `false`）との比較のために置く。
    pub include_glossary: bool,
    pub timeout: Duration,
}

impl fmt::Debug for PlamoProviderOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 述語は中身を表示できないため、設定されているかだけを示す。
        formatter
            .debug_struct("PlamoProviderOptions")
            .field("max_output_tokens", &self.max_output_tokens)
            .field("context_size", &self.context_size)
            .field("protect_strategy", &self.protect_strategy)
            .field(
                "protect_strategy_by_profile",
                &self.protect_strategy_by_profile,
            )
            .field(
                "shader_option_detector",
                &self.shader_option_detector.is_some(),
            )
            .field("entry_observer", &self.entry_observer.is_some())
            .field("include_context", &self.include_context)
            .field("glossary_history", &self.glossary_history)
            .field("include_glossary", &self.include_glossary)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl Default for PlamoProviderOptions {
    fn default() -> Self {
        Self {
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            context_size: None,
            protect_strategy: ProtectStrategy::default(),
            protect_strategy_by_profile: true,
            shader_option_detector: None,
            entry_observer: None,
            include_context: true,
            glossary_history: false,
            include_glossary: true,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

pub struct PlamoProvider {
    client: Client,
    base_url: Url,
    api_key: Option<String>,
    options: PlamoProviderOptions,
    /// `/props` の結果。サーバの再起動でコンテキスト長が変わるまでは使い回す。
    slot_context: OnceCell<u32>,
}

impl PlamoProvider {
    pub fn new(
        base_url: &str,
        api_key: Option<String>,
        options: PlamoProviderOptions,
    ) -> Result<Self> {
        let base_url = Url::parse(base_url).map_err(|_| AiError::InvalidUrl)?;
        // ローカルの `llama-server` 以外へ API key を送らないよう、既存の URL 方針を再利用する。
        yc_ai::validate_provider_url(&base_url).map_err(|_| AiError::InvalidUrl)?;
        // 下限未満の上限は `output_budget` が守れない。黙って切り上げず設定ミスとして弾く。
        if options.max_output_tokens < MIN_OUTPUT_TOKENS {
            return Err(AiError::InvalidConfiguration);
        }
        let api_key = api_key.filter(|key| !key.trim().is_empty());
        if let Some(api_key) = api_key.as_deref() {
            reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|_| AiError::InvalidConfiguration)?;
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(options.timeout)
            // ループバックのサーバを環境変数の proxy 設定経由で外へ出さない。
            .no_proxy()
            // `llama-server` はリダイレクトを返さない。追従すると別ホストへ key が漏れ得る。
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AiError::Network)?;
        Ok(Self {
            client,
            base_url,
            api_key,
            options,
            slot_context: OnceCell::new(),
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        let mut base = self.base_url.clone();
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        base.join(path).map_err(|_| AiError::InvalidUrl)
    }

    fn request(&self, method: reqwest::Method, url: Url) -> RequestBuilder {
        let request = self.client.request(method, url);
        match self.api_key.as_deref() {
            Some(api_key) => request.bearer_auth(api_key),
            None => request,
        }
    }

    async fn send<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        cancellation: &CancellationToken,
    ) -> Result<T> {
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            response = request.send() => response,
        }
        .map_err(|_| AiError::Network)?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(AiError::ProviderAuth);
        }
        if !status.is_success() {
            return Err(AiError::ProviderResponse {
                status: status.as_u16(),
            });
        }

        tokio::select! {
            _ = cancellation.cancelled() => Err(AiError::Cancelled),
            body = response.json::<T>() => body.map_err(|_| AiError::InvalidProviderResponse),
        }
    }

    /// スロットあたりのコンテキスト長。設定が無ければ `/props` から 1 度だけ取得する。
    async fn slot_context(&self, cancellation: &CancellationToken) -> Result<u32> {
        if let Some(size) = self.options.context_size {
            return Ok(size);
        }
        let size = self
            .slot_context
            .get_or_try_init(|| async {
                let url = self.endpoint("props")?;
                let props: PropsResponse = self
                    .send(self.request(reqwest::Method::GET, url), cancellation)
                    .await?;
                Ok::<u32, AiError>(props.default_generation_settings.n_ctx)
            })
            .await?;
        Ok(*size)
    }

    async fn tokenize(
        &self,
        content: &str,
        add_special: bool,
        cancellation: &CancellationToken,
    ) -> Result<u32> {
        let url = self.endpoint("tokenize")?;
        let body = TokenizeRequest {
            content,
            add_special,
        };
        let response: TokenizeResponse = self
            .send(
                self.request(reqwest::Method::POST, url).json(&body),
                cancellation,
            )
            .await?;
        Ok(response.tokens.len() as u32)
    }

    async fn complete(
        &self,
        prompt: &str,
        n_predict: u32,
        cancellation: &CancellationToken,
    ) -> Result<CompletionResponse> {
        let url = self.endpoint("completion")?;
        let body = CompletionRequest {
            prompt,
            n_predict,
            // 翻訳は再現性が最優先。同じ原文から同じ訳文が出ないとレビューできない。
            temperature: 0.0,
            stop: pfml3::STOP_STRINGS,
            cache_prompt: true,
        };
        self.send(
            self.request(reqwest::Method::POST, url).json(&body),
            cancellation,
        )
        .await
    }

    async fn translate_entry(
        &self,
        entry: &TranslationEntry,
        context: &TranslationContext,
        source_language: Option<&str>,
        target_language: &str,
        cancellation: &CancellationToken,
    ) -> Result<Option<AiTranslation>> {
        // 特殊トークンの検査はマスク前の原文に対して行う。マスクは保護区間を無害な
        // マーカーへ置き換えるため、`$(<|plamo:tag|>)` のような原文はマスク後には
        // 危険に見えなくなり、そのまま送られてしまう。
        if pfml3::is_unsafe_for_prompt(&entry.source) {
            tracing::warn!(
                entry_id = entry.id,
                reason = "unsafe_source_text",
                "plamo entry skipped"
            );
            self.report(EntryReport {
                entry_id: entry.id,
                strategy: self.options.protect_strategy,
                failure: Some("unsafe_source_text"),
                attempts: 0,
                ..EntryReport::default(entry.id)
            });
            return Ok(None);
        }

        // `include_context = false` のときは `context` 要素を丸ごと落とす。mod 名を空に
        // するだけでは翻訳キーと entry 種別のヒントが残り、「文脈なし」にならない。
        let prompt_context = if self.options.include_context {
            pfml3::build_context(&ContextParts {
                mod_name: &context.mod_name,
                namespace: &context.namespace,
                key: &entry.key,
                format_profile: entry.format_profile,
                // シェーダー設定の項目名は UI ラベルと語調が違うため、呼び出し側が注入した
                // 判定があればそれを使う。無い場合はシェーダー由来として扱わない。
                shader_option: self
                    .options
                    .shader_option_detector
                    .as_ref()
                    .is_some_and(|detect| detect(entry)),
            })
        } else {
            String::new()
        };
        let matched = pfml3::select_glossary_terms(&entry.source, &context.glossary_terms);
        // 用語を履歴の対としても与えるか。実測では `glossary` 要素だけの方が遵守率が
        // 高いので既定では切る。履歴の本文はマスク前の原文と同じく信頼できない入力
        // なので、`build_prompt` が特殊トークンを含む対を落とす（ここでは組み立てだけ）。
        let history = if self.options.glossary_history {
            pfml3::glossary_history(&matched)
        } else {
            Vec::new()
        };
        let glossary: &[(String, String)] = if self.options.include_glossary {
            &matched
        } else {
            &[]
        };

        let mut strategy = self.strategy_for(entry);
        for attempt in 0..2 {
            if attempt > 0 {
                strategy = strategy.alternative();
            }
            let started = std::time::Instant::now();
            let outcome = self
                .attempt(
                    entry,
                    source_language,
                    target_language,
                    &prompt_context,
                    glossary,
                    &history,
                    strategy,
                    cancellation,
                )
                .await?;
            let elapsed = started.elapsed();
            match outcome {
                Attempt::Translated { translation, stats } => {
                    self.report(EntryReport {
                        strategy,
                        failure: None,
                        attempts: attempt + 1,
                        elapsed,
                        ..stats.into_report(entry.id)
                    });
                    return Ok(Some(AiTranslation {
                        id: entry.id,
                        translation,
                    }));
                }
                Attempt::Failed { failure, stats } => {
                    let reason = failure.reason();
                    if !failure.is_retryable() || attempt == 1 {
                        if failure.is_retryable() {
                            tracing::warn!(
                                entry_id = entry.id,
                                reason,
                                "plamo entry skipped after both protect strategies"
                            );
                        } else {
                            tracing::warn!(entry_id = entry.id, reason, "plamo entry skipped");
                        }
                        self.report(EntryReport {
                            strategy,
                            failure: Some(reason),
                            attempts: attempt + 1,
                            elapsed,
                            ..stats.into_report(entry.id)
                        });
                        return Ok(None);
                    }
                    tracing::debug!(
                        entry_id = entry.id,
                        reason,
                        "plamo format verification failed; retrying with another strategy"
                    );
                }
            }
        }
        Ok(None)
    }

    /// 観測フックへ 1 件分の結果を渡す。登録が無ければ何もしない。
    fn report(&self, report: EntryReport) {
        if let Some(observer) = self.options.entry_observer.as_ref() {
            observer(&report);
        }
    }

    /// entry に使う最初の保護方式。
    ///
    /// `protect_strategy_by_profile` が真なら profile ごとの既定を使う。実測で
    /// Patchouli のマクロは `PairedTag`、`§` コードは `BracketedIndex` が優れる。
    fn strategy_for(&self, entry: &TranslationEntry) -> ProtectStrategy {
        if self.options.protect_strategy_by_profile {
            ProtectStrategy::for_profile(entry.format_profile)
        } else {
            self.options.protect_strategy
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn attempt(
        &self,
        entry: &TranslationEntry,
        source_language: Option<&str>,
        target_language: &str,
        prompt_context: &str,
        glossary: &[(String, String)],
        history: &[(String, String)],
        strategy: ProtectStrategy,
        cancellation: &CancellationToken,
    ) -> Result<Attempt> {
        let protected = ProtectedSource::new(&entry.source, entry.format_profile, strategy);
        let prompt = match pfml3::build_prompt(&PromptRequest {
            source_text: protected.masked_text(),
            source_language,
            target_language,
            context: Some(prompt_context),
            glossary,
            history,
        }) {
            Ok(prompt) => prompt,
            Err(Pfml3Error::UnsafeSourceText) => {
                return Ok(Attempt::Failed {
                    failure: EntryFailure::Fatal("unsafe_source_text"),
                    stats: AttemptStats::default(),
                });
            }
            Err(Pfml3Error::MissingTargetLanguage) => {
                return Ok(Attempt::Failed {
                    failure: EntryFailure::Fatal("missing_target_language"),
                    stats: AttemptStats::default(),
                });
            }
            Err(Pfml3Error::InvalidLanguage) => {
                return Ok(Attempt::Failed {
                    failure: EntryFailure::Fatal("invalid_language"),
                    stats: AttemptStats::default(),
                });
            }
        };

        let prompt_tokens = self.tokenize(&prompt, true, cancellation).await?;
        let source_tokens = self
            .tokenize(protected.masked_text(), false, cancellation)
            .await?;
        let n_predict = self.output_budget(source_tokens);
        let stats = AttemptStats {
            prompt_tokens,
            source_tokens,
            n_predict,
            ..AttemptStats::default()
        };
        let slot_context = self.slot_context(cancellation).await?;
        if prompt_tokens.saturating_add(n_predict) > slot_context {
            // 途中で切れた訳文は返せない。コンテキストに収まらない entry は失敗させる。
            return Ok(Attempt::Failed {
                failure: EntryFailure::Fatal("prompt_too_long"),
                stats,
            });
        }

        let response = self.complete(&prompt, n_predict, cancellation).await?;
        let stats = AttemptStats {
            generated_tokens: response
                .timings
                .as_ref()
                .map_or(0, |timings| timings.predicted_n),
            tokens_per_second: response
                .timings
                .as_ref()
                .map_or(0.0, |timings| timings.predicted_per_second),
            stop_type: response.stop_type.clone(),
            truncated: response.truncated,
            ..stats
        };
        if response.truncated || response.stop_type.as_deref() == Some("limit") {
            // 生成上限で切れた本文は文の途中で終わっている可能性が高い。
            return Ok(Attempt::Failed {
                failure: EntryFailure::Fatal("output_limit"),
                stats,
            });
        }
        let content = pfml3::strip_trailing_stop_token(&response.content);
        // モデルが本文全体を引用符で包むことがある（実測: 1200 entry 中 3 件）。
        // 原文が引用文でない場合だけ外側 1 組を落とす。
        let content = pfml3::strip_wrapping_quotes(content, &entry.source);
        let stats = AttemptStats {
            raw_output: content.to_owned(),
            ..stats
        };
        if content.trim().is_empty() {
            return Ok(Attempt::Failed {
                failure: EntryFailure::Fatal("empty_output"),
                stats,
            });
        }

        match protected.restore(content) {
            Ok(translation) => Ok(Attempt::Translated { translation, stats }),
            Err(ProtectError::UnknownMarker | ProtectError::SpanMismatch) => Ok(Attempt::Failed {
                failure: EntryFailure::Format("format_mismatch"),
                stats,
            }),
        }
    }

    /// 原文トークン数から生成上限を決める。
    ///
    /// 実測（Create / Farmer's Delight / Mekanism / Supplementaries / Ars Nouveau の
    /// 1200 entry、0.8B・2B・8B の CPU/GPU）で、生成トークン数 / 原文トークン数 は
    /// 平均 1.20、p99 2.00、最大 1.64（下限 64 を超える長文だけを見た場合）。
    /// つまり 2 倍 + 余白で足りる。以前は 3 倍 + 32 を見込んでいたが、その分だけ
    /// `prompt_tokens + n_predict` が膨らみ、コンテキストに収まるはずの長文を
    /// `prompt_too_long` で落としていた（スロット 8192 で原文 232 トークンの entry は
    /// 3 倍だと 728 を要求する）。2 倍 + 24 に詰めても上記サンプルで切り捨ては 0 件だった。
    /// 上限は設定値で頭打ちにする。`max_output_tokens >= MIN_OUTPUT_TOKENS` は構築時に
    /// 検査済みなので、ここでの `clamp` 下限は設定値を超えない。
    pub(crate) fn output_budget(&self, source_tokens: u32) -> u32 {
        let cap = self.options.max_output_tokens;
        source_tokens
            .saturating_mul(2)
            .saturating_add(24)
            .clamp(MIN_OUTPUT_TOKENS.min(cap), cap)
    }
}

/// entry 単位の失敗。再試行で直る見込みがあるかどうかで分ける。
enum EntryFailure {
    /// 再試行しても結果は変わらない。
    Fatal(&'static str),
    /// マスク方式との相性かもしれない。
    Format(&'static str),
}

impl EntryFailure {
    fn reason(&self) -> &'static str {
        match self {
            Self::Fatal(reason) | Self::Format(reason) => reason,
        }
    }

    fn is_retryable(&self) -> bool {
        matches!(self, Self::Format(_))
    }
}

/// 1 回の試行の結果。
enum Attempt {
    Translated {
        translation: String,
        stats: AttemptStats,
    },
    Failed {
        failure: EntryFailure,
        stats: AttemptStats,
    },
}

/// 試行中に分かった計測値。失敗しても分かる範囲は残す。
#[derive(Default)]
struct AttemptStats {
    prompt_tokens: u32,
    source_tokens: u32,
    n_predict: u32,
    generated_tokens: u32,
    tokens_per_second: f32,
    stop_type: Option<String>,
    truncated: bool,
    raw_output: String,
}

impl AttemptStats {
    fn into_report(self, entry_id: i64) -> EntryReport {
        EntryReport {
            entry_id,
            strategy: ProtectStrategy::default(),
            failure: None,
            attempts: 0,
            prompt_tokens: self.prompt_tokens,
            source_tokens: self.source_tokens,
            n_predict: self.n_predict,
            generated_tokens: self.generated_tokens,
            tokens_per_second: self.tokens_per_second,
            stop_type: self.stop_type,
            truncated: self.truncated,
            elapsed: Duration::ZERO,
            raw_output: self.raw_output,
        }
    }
}

#[derive(serde::Serialize)]
struct CompletionRequest<'a> {
    prompt: &'a str,
    n_predict: u32,
    temperature: f32,
    stop: [&'static str; 2],
    cache_prompt: bool,
}

#[derive(Deserialize)]
struct CompletionResponse {
    #[serde(default)]
    content: String,
    #[serde(default)]
    stop_type: Option<String>,
    #[serde(default)]
    truncated: bool,
    /// 生成速度の計測値。llama-server は常に返すが、無くても翻訳自体は成立する。
    #[serde(default)]
    timings: Option<CompletionTimings>,
}

#[derive(Deserialize)]
struct CompletionTimings {
    /// 実際に生成したトークン数。`stop` で止まった場合も含む。
    #[serde(default)]
    predicted_n: u32,
    /// 生成フェーズの実測速度（トークン/秒）。プロンプト処理は含まない。
    #[serde(default)]
    predicted_per_second: f32,
}

#[derive(serde::Serialize)]
struct TokenizeRequest<'a> {
    content: &'a str,
    add_special: bool,
}

#[derive(Deserialize)]
struct TokenizeResponse {
    #[serde(default)]
    tokens: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct PropsResponse {
    default_generation_settings: GenerationSettings,
}

#[derive(Deserialize)]
struct GenerationSettings {
    n_ctx: u32,
}

#[async_trait]
impl TranslationProvider for PlamoProvider {
    async fn test_connection(&self, cancellation: &CancellationToken) -> Result<()> {
        let health = self.endpoint("health")?;
        // `/health` はモデル読み込み中に 503 を返す。ここで落ちれば起動待ちだと分かる。
        let _: serde_json::Value = self
            .send(self.request(reqwest::Method::GET, health), cancellation)
            .await?;

        let prompt = pfml3::build_prompt(&PromptRequest {
            source_text: "Hello",
            source_language: Some("en"),
            target_language: "ja",
            context: None,
            glossary: &[],
            history: &[],
        })
        .map_err(|_| AiError::InvalidConfiguration)?;
        let n_predict = self.output_budget(4);
        let response = self.complete(&prompt, n_predict, cancellation).await?;
        if response.truncated || response.stop_type.as_deref() == Some("limit") {
            return Err(AiError::InvalidProviderResponse);
        }
        if pfml3::strip_trailing_stop_token(&response.content)
            .trim()
            .is_empty()
        {
            return Err(AiError::InvalidProviderResponse);
        }
        Ok(())
    }

    async fn translate_batch(
        &self,
        entries: &[TranslationEntry],
        context: &TranslationContext,
        cancellation: &CancellationToken,
    ) -> Result<Vec<AiTranslation>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        if cancellation.is_cancelled() {
            return Err(AiError::Cancelled);
        }

        // 未対応の翻訳先は呼び出し側が UI で除外できるよう、entry ではなく呼び出し全体で返す。
        for entry in entries {
            if languages::plamo_code_for_locale(&entry.target_locale).is_none() {
                return Err(AiError::UnsupportedTargetLanguage);
            }
        }

        let mut translations = Vec::with_capacity(entries.len());
        // サーバのスロット数は呼び出し側が決める。ここで並列化すると slot を奪い合うだけなので
        // 逐次実行し、同時実行の制御は上位の `translate_entries` に任せる。
        for entry in entries {
            if cancellation.is_cancelled() {
                return Err(AiError::Cancelled);
            }
            let Some(target_language) = languages::plamo_code_for_locale(&entry.target_locale)
            else {
                return Err(AiError::UnsupportedTargetLanguage);
            };
            let source_language = languages::plamo_code_for_locale(&entry.source_locale);
            if source_language.is_none() {
                // 原文の言語が分からないと PFML3 の `input` 属性を埋められない。省略もできるが、
                // 自動判定に委ねると原文言語と訳文言語の組み合わせが不定になる。
                tracing::warn!(
                    entry_id = entry.id,
                    reason = "unsupported_source_language",
                    "plamo entry skipped"
                );
                continue;
            }

            if let Some(translation) = self
                .translate_entry(
                    entry,
                    context,
                    source_language,
                    target_language,
                    cancellation,
                )
                .await?
            {
                translations.push(translation);
            }
        }
        Ok(translations)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            // 1 リクエスト 1 entry。PFML3 に複数 entry をまとめる構文は無い。
            batch_translation: false,
            entry_context: true,
            // 翻訳専用モデルで指示文を解釈しない。`custom_prompt` は無視する。
            custom_prompt: false,
            structured_output: false,
            // ローカルサーバなので key は任意。
            requires_api_key: false,
        }
    }
}
