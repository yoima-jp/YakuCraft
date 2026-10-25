use reqwest::Method;
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

const API_BASE: &str = "https://api.anthropic.com/";
const API_VERSION: &str = "2023-06-01";
const TRANSLATION_TOOL: &str = "submit_translations";

pub struct AnthropicProvider {
    http: ProviderHttp,
    base_url: Url,
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let base_url = Url::parse(API_BASE).map_err(|_| AiError::InvalidConfiguration)?;
        Self::with_base_url(api_key.into(), model.into(), base_url)
    }

    fn with_base_url(api_key: String, model: String, base_url: Url) -> Result<Self> {
        if api_key.trim().is_empty() || model.trim().is_empty() {
            return Err(AiError::InvalidConfiguration);
        }
        validate_auth_header(&api_key)?;
        crate::validate_provider_url(&base_url).map_err(|_| AiError::InvalidUrl)?;
        let http = ProviderHttp::new_fixed("anthropic")?;
        http.endpoint(&base_url, "v1/messages")?;
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

    fn request(
        &self,
        method: Method,
        endpoint: Url,
        body: Option<Value>,
    ) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .client()
            .request(method, endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION);
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
    }

    pub async fn list_models(
        api_key: &str,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>> {
        let base_url = Url::parse(API_BASE).map_err(|_| AiError::InvalidConfiguration)?;
        Self::fetch_models(base_url, api_key, cancellation).await
    }

    async fn fetch_models(
        base_url: Url,
        api_key: &str,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>> {
        crate::validate_provider_url(&base_url).map_err(|_| AiError::InvalidUrl)?;
        if api_key.trim().is_empty() {
            return Err(AiError::InvalidConfiguration);
        }
        validate_auth_header(api_key)?;

        let http = ProviderHttp::new_fixed("anthropic")?;
        let models_endpoint = http.endpoint(&base_url, "v1/models")?;
        let mut after_id: Option<String> = None;
        let mut models = Vec::new();
        loop {
            let mut endpoint = models_endpoint.clone();
            {
                let mut query = endpoint.query_pairs_mut();
                query.append_pair("limit", "1000");
                if let Some(cursor) = &after_id {
                    query.append_pair("after_id", cursor);
                }
            }
            let response = http
                .send_with_retry(
                    || {
                        http.client()
                            .get(endpoint.clone())
                            .header("x-api-key", api_key)
                            .header("anthropic-version", API_VERSION)
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
            let page = payload
                .get("data")
                .and_then(Value::as_array)
                .ok_or(AiError::InvalidProviderResponse)?;
            models.extend(
                page.iter()
                    .filter_map(|model| model.get("id").and_then(Value::as_str))
                    .map(str::to_owned),
            );
            if payload.get("has_more").and_then(Value::as_bool) != Some(true) {
                break;
            }
            after_id = Some(
                payload
                    .get("last_id")
                    .and_then(Value::as_str)
                    .filter(|last_id| !last_id.is_empty())
                    .ok_or(AiError::InvalidProviderResponse)?
                    .to_owned(),
            );
        }
        Ok(models)
    }
}

#[async_trait::async_trait]
impl TranslationProvider for AnthropicProvider {
    async fn test_connection(&self, cancellation: &CancellationToken) -> Result<()> {
        let endpoint = self.endpoint("v1/models")?;
        let response = self
            .http
            .send_with_retry(
                || self.request(Method::GET, endpoint.clone(), None),
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
        let endpoint = self.endpoint("v1/messages")?;
        let mut pending = vec![entries];
        let mut translations = Vec::with_capacity(entries.len());
        while let Some(batch) = pending.pop() {
            let user = prompt::user_payload(context, batch)?;
            let body = json!({
                "model": self.model,
                "max_tokens": 4096,
                "system": prompt::system_instruction(context, batch, false),
                "messages": [{"role": "user", "content": user}],
                "tools": [{
                    "name": TRANSLATION_TOOL,
                    "description": "Return the requested translations.",
                    "input_schema": super::openai::translation_schema()
                }],
                "tool_choice": {"type": "tool", "name": TRANSLATION_TOOL}
            });
            let response = self
                .http
                .send_with_retry(
                    || self.request(Method::POST, endpoint.clone(), Some(body.clone())),
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
            if payload.get("stop_reason").and_then(Value::as_str) == Some("max_tokens")
                && batch.len() > 1
            {
                let (left, right) = batch.split_at(batch.len() / 2);
                pending.push(right);
                pending.push(left);
            } else {
                translations.extend(parse_message(payload)?);
            }
        }
        Ok(translations)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            batch_translation: true,
            entry_context: true,
            custom_prompt: true,
            structured_output: true,
            requires_api_key: true,
        }
    }
}

fn parse_message(payload: Value) -> Result<Vec<AiTranslation>> {
    if payload.get("stop_reason").and_then(Value::as_str) != Some("tool_use") {
        return Err(AiError::InvalidProviderResponse);
    }
    let input = payload
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks.iter().find(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && block.get("name").and_then(Value::as_str) == Some(TRANSLATION_TOOL)
            })
        })
        .and_then(|block| block.get("input"))
        .ok_or(AiError::InvalidProviderResponse)?;
    let input = serde_json::to_string(input).map_err(|_| AiError::InvalidProviderResponse)?;
    crate::models::parse_translation_payload(&input)
}

fn ensure_success(status: u16) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(AiError::ProviderResponse { status })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    use super::AnthropicProvider;
    use crate::{TranslationContext, TranslationEntry, TranslationProvider};

    #[tokio::test]
    async fn messages_api_uses_required_tool_and_anthropic_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "mock-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(body_string_contains("submit_translations"))
            .and(body_string_contains("\"type\":\"tool\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "stop_reason": "tool_use",
                "content": [{
                    "type": "tool_use",
                    "name": "submit_translations",
                    "input": {"translations": [{"id": 10, "translation": "こんにちは"}]}
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "mock-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": "claude-model"}],
                "has_more": false
            })))
            .mount(&server)
            .await;

        let mut base = Url::parse(&server.uri()).unwrap();
        base.set_host(Some("127.0.0.1")).unwrap();
        let provider =
            AnthropicProvider::with_base_url("mock-key".into(), "mock-model".into(), base).unwrap();
        provider
            .test_connection(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            AnthropicProvider::fetch_models(
                Url::parse(&server.uri()).unwrap(),
                "mock-key",
                &CancellationToken::new(),
            )
            .await
            .unwrap(),
            ["claude-model"]
        );
        let translations = provider
            .translate_batch(
                &[TranslationEntry::new(
                    10,
                    "item.machine",
                    "Machine",
                    "en_us",
                    "ja_jp",
                )],
                &TranslationContext::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(translations[0].id, 10);
        assert_eq!(translations[0].translation, "こんにちは");
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }
}
