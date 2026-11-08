//! `llama-server` 上の PLaMo 3 Translate を呼ぶ Provider。
//!
//! サーバの起動とモデルのダウンロードは別クレート (`yc-llama`) の責務で、ここは
//! 起動済みサーバの base URL と API key だけを受け取る。

use std::{fmt, result, sync::Arc, time::Duration};

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

/// Provider の調整項目。
#[derive(Clone)]
pub struct PlamoProviderOptions {
    /// `n_predict` の上限。
    pub max_output_tokens: u32,
    /// スロットあたりのコンテキスト長。`None` なら `/props` から取得してキャッシュする。
    pub context_size: Option<u32>,
    /// 保護区間のマスク方式。既定は [`ProtectStrategy::BracketedIndex`]。
    pub protect_strategy: ProtectStrategy,
    /// entry ごとのシェーダー設定判定。`None` なら該当なしとして扱う。
    pub shader_option_detector: Option<ShaderOptionDetector>,
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
                "shader_option_detector",
                &self.shader_option_detector.is_some(),
            )
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
            shader_option_detector: None,
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
            return Ok(None);
        }

        let prompt_context = pfml3::build_context(&ContextParts {
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
        });
        let glossary = pfml3::select_glossary_terms(&entry.source, &context.glossary_terms);

        let mut strategy = self.options.protect_strategy;
        for attempt in 0..2 {
            if attempt > 0 {
                strategy = strategy.alternative();
            }
            match self
                .attempt(
                    entry,
                    source_language,
                    target_language,
                    &prompt_context,
                    &glossary,
                    strategy,
                    cancellation,
                )
                .await?
            {
                Ok(translation) => {
                    return Ok(Some(AiTranslation {
                        id: entry.id,
                        translation,
                    }));
                }
                Err(EntryFailure::Fatal(reason)) => {
                    tracing::warn!(entry_id = entry.id, reason, "plamo entry skipped");
                    return Ok(None);
                }
                Err(EntryFailure::Format(reason)) => {
                    if attempt == 1 {
                        tracing::warn!(
                            entry_id = entry.id,
                            reason,
                            "plamo entry skipped after both protect strategies"
                        );
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

    #[allow(clippy::too_many_arguments)]
    async fn attempt(
        &self,
        entry: &TranslationEntry,
        source_language: Option<&str>,
        target_language: &str,
        prompt_context: &str,
        glossary: &[(String, String)],
        strategy: ProtectStrategy,
        cancellation: &CancellationToken,
    ) -> Result<result::Result<String, EntryFailure>> {
        let protected = ProtectedSource::new(&entry.source, entry.format_profile, strategy);
        let prompt = match pfml3::build_prompt(&PromptRequest {
            source_text: protected.masked_text(),
            source_language,
            target_language,
            context: Some(prompt_context),
            glossary,
        }) {
            Ok(prompt) => prompt,
            Err(Pfml3Error::UnsafeSourceText) => {
                return Ok(Err(EntryFailure::Fatal("unsafe_source_text")));
            }
            Err(Pfml3Error::MissingTargetLanguage) => {
                return Ok(Err(EntryFailure::Fatal("missing_target_language")));
            }
        };

        let prompt_tokens = self.tokenize(&prompt, true, cancellation).await?;
        let source_tokens = self
            .tokenize(protected.masked_text(), false, cancellation)
            .await?;
        let n_predict = self.output_budget(source_tokens);
        let slot_context = self.slot_context(cancellation).await?;
        if prompt_tokens.saturating_add(n_predict) > slot_context {
            // 途中で切れた訳文は返せない。コンテキストに収まらない entry は失敗させる。
            return Ok(Err(EntryFailure::Fatal("prompt_too_long")));
        }

        let response = self.complete(&prompt, n_predict, cancellation).await?;
        if response.truncated || response.stop_type.as_deref() == Some("limit") {
            // 生成上限で切れた本文は文の途中で終わっている可能性が高い。
            return Ok(Err(EntryFailure::Fatal("output_limit")));
        }
        let content = pfml3::strip_trailing_stop_token(&response.content);
        if content.trim().is_empty() {
            return Ok(Err(EntryFailure::Fatal("empty_output")));
        }

        match protected.restore(content) {
            Ok(translation) => Ok(Ok(translation)),
            Err(ProtectError::UnknownMarker | ProtectError::SpanMismatch) => {
                Ok(Err(EntryFailure::Format("format_mismatch")))
            }
        }
    }

    /// 原文トークン数から生成上限を決める。
    ///
    /// 訳文は原文より長くなりやすい（日本語は英語より文字数が増える）ので 3 倍を見込み、
    /// 短い原文でも最低限の余裕を残す。上限は設定値で頭打ちにする。
    /// `max_output_tokens >= MIN_OUTPUT_TOKENS` は構築時に検査済みなので、
    /// ここでの `clamp` 下限は設定値を超えない。
    pub(crate) fn output_budget(&self, source_tokens: u32) -> u32 {
        let cap = self.options.max_output_tokens;
        source_tokens
            .saturating_mul(3)
            .saturating_add(32)
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
