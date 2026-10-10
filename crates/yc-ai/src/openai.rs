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

const API_BASE: &str = "https://api.openai.com/v1/";

pub struct OpenAiProvider {
    http: ProviderHttp,
    api_key: String,
    model: String,
    responses_url: Url,
    model_url: Url,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let base = Url::parse(API_BASE).map_err(|_| AiError::InvalidConfiguration)?;
        Self::with_base_url(api_key.into(), model.into(), base)
    }

    fn with_base_url(api_key: String, model: String, base: Url) -> Result<Self> {
        if api_key.trim().is_empty() || model.trim().is_empty() {
            return Err(AiError::InvalidConfiguration);
        }
        validate_auth_header(&format!("Bearer {api_key}"))?;

        let http = ProviderHttp::new("openai")?;
        let responses_url = http.endpoint(&base, "responses")?;
        let mut model_url = http.endpoint(&base, "models")?;
        model_url
            .path_segments_mut()
            .map_err(|_| AiError::InvalidConfiguration)?
            .push(&model);

        Ok(Self {
            http,
            api_key,
            model,
            responses_url,
            model_url,
        })
    }
}

#[async_trait::async_trait]
impl TranslationProvider for OpenAiProvider {
    async fn test_connection(&self, cancellation: &CancellationToken) -> Result<()> {
        let response = self
            .http
            .send_with_retry(
                || {
                    self.http
                        .client()
                        .request(Method::GET, self.model_url.clone())
                        .bearer_auth(&self.api_key)
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
        let user_input = prompt::user_payload(context, entries)?;
        let body = json!({
            "model": self.model,
            "input": [
                {
                    "role": "system",
                    "content": [{"type": "input_text", "text": prompt::system_instruction(context, false)}]
                },
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": user_input}]
                }
            ],
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "translation_batch",
                    "strict": true,
                    "schema": translation_schema()
                }
            }
        });
        let response = self
            .http
            .send_with_retry(
                || {
                    self.http
                        .client()
                        .post(self.responses_url.clone())
                        .bearer_auth(&self.api_key)
                        .json(&body)
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
        parse_responses_output(payload)
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

pub(crate) fn translation_schema() -> Value {
    // Responses API の strict schema は object を root にし、翻訳配列内の ID で結果を対応付ける。
    json!({
        "type": "object",
        "properties": {
            "translations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "integer"},
                        "translation": {"type": "string"}
                    },
                    "required": ["id", "translation"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["translations"],
        "additionalProperties": false
    })
}

pub(crate) fn parse_responses_output(payload: Value) -> Result<Vec<AiTranslation>> {
    if payload.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(AiError::InvalidProviderResponse);
    }

    let mut output_text = payload
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if output_text.is_none() {
        output_text = payload
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|item| {
                item.get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
            .filter_map(|content| content.get("text").and_then(Value::as_str))
            .map(str::to_owned)
            .next();
    }
    let output_text = output_text.ok_or(AiError::InvalidProviderResponse)?;
    crate::models::parse_translation_payload(&output_text)
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

    use super::OpenAiProvider;
    use crate::{TranslationContext, TranslationEntry, TranslationProvider};

    #[tokio::test]
    async fn sends_structured_outputs_request_to_responses_api() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("Authorization", "Bearer mock-key"))
            .and(body_string_contains("json_schema"))
            .and(body_string_contains("mock-model"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "completed",
                "output": [{
                    "content": [{
                        "type": "output_text",
                        "text": "{\"translations\":[{\"id\":10,\"translation\":\"こんにちは\"}]}"
                    }]
                }]
            })))
            .mount(&server)
            .await;

        let mut base = Url::parse(&server.uri()).unwrap();
        base.set_host(Some("127.0.0.1")).unwrap();
        base.set_path("/v1/");
        let provider =
            OpenAiProvider::with_base_url("mock-key".into(), "mock-model".into(), base).unwrap();
        let translations = provider
            .translate_batch(
                &[TranslationEntry::new(
                    10,
                    "item.machine",
                    "Machine",
                    "en_us",
                    "ja_jp",
                )],
                &TranslationContext {
                    mod_name: "Example Mod".into(),
                    namespace: "example".into(),
                    custom_prompt: Some("Use concise game terminology.".into()),
                    glossary_terms: Vec::new(),
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(translations[0].id, 10);
        assert_eq!(translations[0].translation, "こんにちは");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
