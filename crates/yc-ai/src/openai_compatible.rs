use reqwest::{Method, Response, StatusCode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;
use yc_validation::AiTranslation;

use crate::{
    AiError, Result, TranslationContext, TranslationEntry, TranslationProvider,
    http::{ProviderHttp, validate_auth_header},
    prompt,
    provider::ProviderCapabilities,
};

pub struct OpenAiCompatibleProvider {
    http: ProviderHttp,
    base_url: Url,
    api_key: Option<String>,
    model: String,
}

impl OpenAiCompatibleProvider {
    pub fn new(base_url: &str, api_key: Option<String>, model: impl Into<String>) -> Result<Self> {
        let base_url = Url::parse(base_url).map_err(|_| AiError::InvalidUrl)?;
        crate::validate_provider_url(&base_url).map_err(|_| AiError::InvalidUrl)?;
        let model = model.into();
        if model.trim().is_empty() {
            return Err(AiError::InvalidConfiguration);
        }
        let api_key = api_key.filter(|key| !key.trim().is_empty());
        if let Some(api_key) = api_key.as_deref() {
            validate_auth_header(&format!("Bearer {api_key}"))?;
        }
        let http = ProviderHttp::new("openai-compatible")?;
        http.endpoint(&base_url, "chat/completions")?;
        Ok(Self {
            http,
            base_url,
            api_key,
            model,
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.http.endpoint(&self.base_url, path)
    }

    fn request(&self, endpoint: Url, body: Option<Value>) -> reqwest::RequestBuilder {
        let mut request = self.http.client().request(Method::POST, endpoint);
        if let Some(api_key) = self.api_key.as_deref() {
            request = request.bearer_auth(api_key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
    }
}

pub async fn list_models(
    base_url: &str,
    api_key: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<Vec<String>> {
    let base_url = Url::parse(base_url).map_err(|_| AiError::InvalidUrl)?;
    crate::validate_provider_url(&base_url).map_err(|_| AiError::InvalidUrl)?;
    let api_key = api_key.filter(|key| !key.trim().is_empty());
    if let Some(api_key) = api_key {
        validate_auth_header(&format!("Bearer {api_key}"))?;
    }
    let http = ProviderHttp::new("openai-compatible")?;
    let endpoint = http.endpoint(&base_url, "models")?;
    let response = http
        .send_with_retry(
            || {
                let request = http.client().get(endpoint.clone());
                match api_key {
                    Some(api_key) => request.bearer_auth(api_key),
                    None => request,
                }
            },
            cancellation,
        )
        .await?;
    ensure_success(response.status().as_u16())?;
    if cancellation.is_cancelled() {
        return Err(AiError::Cancelled);
    }
    let payload: Value = tokio::select! {
        _ = cancellation.cancelled() => return Err(AiError::Cancelled),
        payload = response.json() => payload.map_err(|_| AiError::InvalidProviderResponse)?,
    };
    let models = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or(AiError::InvalidProviderResponse)?
        .iter()
        .filter_map(|model| model.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    Ok(models)
}

#[async_trait::async_trait]
impl TranslationProvider for OpenAiCompatibleProvider {
    async fn test_connection(&self, cancellation: &CancellationToken) -> Result<()> {
        let endpoint = self.endpoint("models")?;
        let response = self
            .http
            .send_with_retry(
                || {
                    let mut request = self.http.client().request(Method::GET, endpoint.clone());
                    if let Some(api_key) = self.api_key.as_deref() {
                        request = request.bearer_auth(api_key);
                    }
                    request
                },
                cancellation,
            )
            .await?;
        ensure_success(response.status().as_u16())
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
        let endpoint = self.endpoint("chat/completions")?;
        let payload = prompt::user_payload(context, entries)?;
        let structured_body = chat_body(
            &self.model,
            &prompt::system_instruction(context, entries, false),
            &payload,
            true,
        );
        let response = self
            .http
            .send_with_retry(
                || self.request(endpoint.clone(), Some(structured_body.clone())),
                cancellation,
            )
            .await?;

        let response = if matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ) {
            let status = response.status().as_u16();
            if !is_structured_output_rejection(response, cancellation).await? {
                return Err(AiError::ProviderResponse { status });
            }
            let fallback_body = chat_body(
                &self.model,
                &prompt::system_instruction(context, entries, true),
                &payload,
                false,
            );
            self.http
                .send_with_retry(
                    || self.request(endpoint.clone(), Some(fallback_body.clone())),
                    cancellation,
                )
                .await?
        } else {
            response
        };

        ensure_success(response.status().as_u16())?;
        if cancellation.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        let payload: Value = tokio::select! {
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            payload = response.json() => payload.map_err(|_| AiError::InvalidProviderResponse)?,
        };
        parse_chat_completion(payload)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            batch_translation: true,
            entry_context: true,
            custom_prompt: true,
            structured_output: true,
            requires_api_key: false,
        }
    }
}

fn chat_body(model: &str, system: &str, user: &str, structured: bool) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    });
    if structured {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "translation_batch",
                "strict": true,
                "schema": super::openai::translation_schema()
            }
        });
    }
    body
}

async fn is_structured_output_rejection(
    mut response: Response,
    cancellation: &CancellationToken,
) -> Result<bool> {
    // 互換実装ごとにエラー本文の形式が異なるため、本文は判定にだけ使い、外部へ返さない。
    let mut body = Vec::new();
    while body.len() < 16 * 1024 {
        let chunk = tokio::select! {
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            chunk = response.chunk() => chunk,
        };
        match chunk {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(_) => return Ok(false),
        }
    }
    Ok(response_format_unsupported(&String::from_utf8_lossy(&body)))
}

fn response_format_unsupported(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("response_format")
        && [
            "unsupported",
            "not support",
            "not supported",
            "doesn't support",
            "unrecognized",
            "unknown",
            "invalid parameter",
            "not allowed",
            "unexpected",
        ]
        .iter()
        .any(|marker| body.contains(marker))
}

fn parse_chat_completion(payload: Value) -> Result<Vec<AiTranslation>> {
    let content = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| {
            content.as_str().map(str::to_owned).or_else(|| {
                content.as_array().and_then(|parts| {
                    parts
                        .iter()
                        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .next()
                        .map(str::to_owned)
                })
            })
        })
        .ok_or(AiError::InvalidProviderResponse)?;
    crate::models::parse_translation_payload(&content)
}

fn ensure_success(status: u16) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(AiError::ProviderResponse { status })
    }
}
