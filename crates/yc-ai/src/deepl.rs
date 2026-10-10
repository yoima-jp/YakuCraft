use std::collections::BTreeMap;

use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use url::Url;
use yc_validation::AiTranslation;

use crate::{
    AiError, Result, TranslationContext, TranslationEntry, TranslationProvider,
    http::{ProviderHttp, validate_auth_header},
    provider::ProviderCapabilities,
};

const FREE_API_BASE: &str = "https://api-free.deepl.com/v2/";
const PRO_API_BASE: &str = "https://api.deepl.com/v2/";

pub struct DeepLProvider {
    http: ProviderHttp,
    api_key: String,
    translate_url: Url,
    usage_url: Url,
}

impl DeepLProvider {
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(AiError::InvalidConfiguration);
        }
        validate_auth_header(&format!("DeepL-Auth-Key {api_key}"))?;
        let base =
            Url::parse(deepl_base_url(&api_key)).map_err(|_| AiError::InvalidConfiguration)?;
        let http = ProviderHttp::new("deepl")?;
        let translate_url = http.endpoint(&base, "translate")?;
        let usage_url = http.endpoint(&base, "usage")?;
        Ok(Self {
            http,
            api_key,
            translate_url,
            usage_url,
        })
    }
}

pub fn deepl_base_url(api_key: &str) -> &'static str {
    if api_key.ends_with(":fx") {
        FREE_API_BASE
    } else {
        PRO_API_BASE
    }
}

#[async_trait::async_trait]
impl TranslationProvider for DeepLProvider {
    async fn test_connection(&self, cancellation: &CancellationToken) -> Result<()> {
        let response = self
            .http
            .send_with_retry(
                || {
                    self.http
                        .client()
                        .request(Method::GET, self.usage_url.clone())
                        .header("Authorization", format!("DeepL-Auth-Key {}", self.api_key))
                },
                cancellation,
            )
            .await?;
        ensure_success(response.status().as_u16())
    }

    async fn translate_batch(
        &self,
        entries: &[TranslationEntry],
        _context: &TranslationContext,
        cancellation: &CancellationToken,
    ) -> Result<Vec<AiTranslation>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        // DeepL は1リクエストで source_lang を1つだけ受け付けるため、言語別にまとめる。
        let mut groups: BTreeMap<Option<String>, Vec<(usize, &TranslationEntry)>> = BTreeMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let source_language = deepl_source_language(&entry.source_locale);
            let source_language = source_language.filter(|language| language != "JA");
            groups
                .entry(source_language)
                .or_default()
                .push((index, entry));
        }

        let mut translations = vec![None; entries.len()];
        for (source_language, group) in groups {
            if cancellation.is_cancelled() {
                return Err(AiError::Cancelled);
            }
            let texts: Vec<_> = group
                .iter()
                .map(|(_, entry)| entry.source.as_str())
                .collect();
            let mut body = json!({
                "text": texts,
                "target_lang": "JA"
            });
            if let Some(source_language) = source_language {
                body["source_lang"] = json!(source_language);
            }

            let response = self
                .http
                .send_with_retry(
                    || {
                        self.http
                            .client()
                            .post(self.translate_url.clone())
                            .header("Authorization", format!("DeepL-Auth-Key {}", self.api_key))
                            .json(&body)
                    },
                    cancellation,
                )
                .await?;
            ensure_success(response.status().as_u16())?;
            let response: DeepLResponse = tokio::select! {
                _ = cancellation.cancelled() => return Err(AiError::Cancelled),
                response = response.json() => {
                    response.map_err(|_| AiError::InvalidProviderResponse)?
                }
            };
            // DeepL は ID を返さないため余分な行は対応付け不能。欠落行は共通 validator に MissingResponse として判定させる。
            if response.translations.len() > group.len() {
                return Err(AiError::InvalidProviderResponse);
            }
            for ((index, entry), translated) in group.into_iter().zip(response.translations) {
                translations[index] = Some(AiTranslation {
                    id: entry.id,
                    translation: translated.text,
                });
            }
        }

        if cancellation.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        Ok(translations.into_iter().flatten().collect::<Vec<_>>())
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            batch_translation: true,
            entry_context: false,
            custom_prompt: false,
            structured_output: false,
            requires_api_key: true,
        }
    }
}

#[derive(Deserialize)]
struct DeepLResponse {
    translations: Vec<DeepLTranslation>,
}

#[derive(Deserialize)]
struct DeepLTranslation {
    text: String,
}

fn deepl_source_language(locale: &str) -> Option<String> {
    let language = locale.split(['_', '-']).next()?.trim();
    if language.len() == 2 && language.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        Some(language.to_ascii_uppercase())
    } else {
        None
    }
}

fn ensure_success(status: u16) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(AiError::ProviderResponse { status })
    }
}
