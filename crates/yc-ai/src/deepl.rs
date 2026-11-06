use std::collections::BTreeMap;

use reqwest::{Method, Response};
use serde::Deserialize;
use serde_json::{Value, json};
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
    languages_url: Url,
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
        let mut languages_url = http.endpoint(&base, "../v3/languages")?;
        languages_url
            .query_pairs_mut()
            .append_pair("resource", "translate_text");
        let usage_url = http.endpoint(&base, "usage")?;
        Ok(Self {
            http,
            api_key,
            translate_url,
            languages_url,
            usage_url,
        })
    }

    pub async fn target_locale_support(&self, locale: &str) -> Option<bool> {
        let candidates = deepl_target_languages(locale)?;
        let cancellation = CancellationToken::new();
        let available = self
            .fetch_supported_target_languages(&cancellation)
            .await
            .ok()?;
        Some(select_supported_target(&candidates, &available).is_some())
    }

    async fn fetch_supported_target_languages(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>> {
        let response = self
            .http
            .send_with_retry(
                || {
                    self.http
                        .client()
                        .get(self.languages_url.clone())
                        .header("Authorization", format!("DeepL-Auth-Key {}", self.api_key))
                },
                cancellation,
            )
            .await?;
        ensure_success(response.status().as_u16())?;
        let languages: Vec<DeepLLanguage> = tokio::select! {
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            response = response.json() => {
                response.map_err(|_| AiError::InvalidProviderResponse)?
            }
        };
        Ok(languages
            .into_iter()
            .filter(|language| language.usable_as_target)
            .map(|language| language.lang)
            .collect())
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

        let available_targets = match self.fetch_supported_target_languages(cancellation).await {
            Ok(languages) => Some(languages),
            Err(AiError::Cancelled) => return Err(AiError::Cancelled),
            Err(_) => None,
        };

        // DeepL は1リクエストで source_lang と target_lang を各1つだけ受け付ける。
        let mut groups: BTreeMap<(Option<String>, String), Vec<(usize, &TranslationEntry)>> =
            BTreeMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let target_language =
                select_target_language(&entry.target_locale, available_targets.as_deref())?;
            let source_language =
                source_language_for_entry(&entry.source_locale, &entry.target_locale);
            groups
                .entry((source_language, target_language))
                .or_default()
                .push((index, entry));
        }

        let mut translations = vec![None; entries.len()];
        for ((source_language, target_language), group) in groups {
            if cancellation.is_cancelled() {
                return Err(AiError::Cancelled);
            }
            let texts: Vec<_> = group
                .iter()
                .map(|(_, entry)| entry.source.as_str())
                .collect();
            let body = translation_body(&texts, &target_language, source_language.as_deref());

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
            let response = ensure_translation_success(response).await?;
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

#[derive(Deserialize)]
struct DeepLLanguage {
    lang: String,
    usable_as_target: bool,
}

pub fn deepl_target_languages(locale: &str) -> Option<Vec<String>> {
    let normalized = locale.trim().replace('-', "_").to_ascii_lowercase();
    if yc_locale::is_constructed_language(&normalized) {
        return None;
    }
    let mut parts = normalized.split('_');
    let language = parts.next()?;
    if !(2..=3).contains(&language.len())
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    let region = parts.next();
    let candidates = match (language, region) {
        ("en", Some("us" | "ca")) => vec!["EN-US".to_owned()],
        ("en", Some("gb" | "au" | "nz")) => vec!["EN-GB".to_owned()],
        ("pt", Some("br")) => vec!["PT-BR".to_owned()],
        ("pt", Some("pt")) => vec!["PT-PT".to_owned()],
        ("zh", Some("cn")) => vec!["ZH-HANS".to_owned()],
        ("zh", Some("tw" | "hk")) => vec!["ZH-HANT".to_owned()],
        ("es", Some(region)) if region != "es" => {
            vec!["ES-419".to_owned(), "ES".to_owned()]
        }
        ("es", _) => vec!["ES".to_owned()],
        ("no", Some("no")) => vec!["NB".to_owned()],
        _ => vec![language.to_ascii_uppercase()],
    };
    Some(candidates)
}

fn select_supported_target(candidates: &[String], available: &[String]) -> Option<String> {
    candidates.iter().find_map(|candidate| {
        available
            .iter()
            .any(|language| language.eq_ignore_ascii_case(candidate))
            .then(|| candidate.clone())
    })
}

fn select_target_language(locale: &str, available: Option<&[String]>) -> Result<String> {
    let candidates = deepl_target_languages(locale).ok_or(AiError::UnsupportedTargetLanguage)?;
    match available {
        Some(available) => select_supported_target(&candidates, available)
            .ok_or(AiError::UnsupportedTargetLanguage),
        None => candidates
            .into_iter()
            .next()
            .ok_or(AiError::UnsupportedTargetLanguage),
    }
}

fn source_language_for_entry(source_locale: &str, target_locale: &str) -> Option<String> {
    let source_base = locale_base_language(source_locale);
    let target_base = locale_base_language(target_locale);
    if source_base.is_some() && source_base == target_base {
        None
    } else {
        deepl_source_language(source_locale)
    }
}

fn locale_base_language(locale: &str) -> Option<String> {
    let language = locale.trim().split(['_', '-']).next()?;
    if (2..=3).contains(&language.len()) && language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        Some(language.to_ascii_lowercase())
    } else {
        None
    }
}

fn translation_body(texts: &[&str], target_language: &str, source_language: Option<&str>) -> Value {
    let mut body = json!({
        "text": texts,
        "target_lang": target_language
    });
    if let Some(source_language) = source_language {
        body["source_lang"] = json!(source_language);
    }
    body
}

fn deepl_source_language(locale: &str) -> Option<String> {
    Some(locale_base_language(locale)?.to_ascii_uppercase())
}

async fn ensure_translation_success(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status.is_client_error() {
        let body = response
            .text()
            .await
            .unwrap_or_default()
            .to_ascii_lowercase();
        if body.contains("target_lang") || body.contains("target language") {
            return Err(AiError::UnsupportedTargetLanguage);
        }
    }
    Err(AiError::ProviderResponse {
        status: status.as_u16(),
    })
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
        matchers::{body_string_contains, header, method, path, query_param},
    };

    use super::*;

    fn provider_for(server: &MockServer) -> DeepLProvider {
        let mut base = Url::parse(&server.uri()).unwrap();
        base.set_host(Some("127.0.0.1")).unwrap();
        let http = ProviderHttp::new("deepl").unwrap();
        let translate_url = http.endpoint(&base, "v2/translate").unwrap();
        let mut languages_url = http.endpoint(&base, "v3/languages").unwrap();
        languages_url
            .query_pairs_mut()
            .append_pair("resource", "translate_text");
        let usage_url = http.endpoint(&base, "v2/usage").unwrap();
        DeepLProvider {
            http,
            api_key: "mock-key".into(),
            translate_url,
            languages_url,
            usage_url,
        }
    }

    #[test]
    fn target_locale_candidates_follow_deepl_mappings() {
        assert_eq!(deepl_target_languages("en_gb").unwrap(), ["EN-GB"]);
        assert_eq!(deepl_target_languages("pt_br").unwrap(), ["PT-BR"]);
        assert_eq!(deepl_target_languages("zh_tw").unwrap(), ["ZH-HANT"]);
        assert_eq!(deepl_target_languages("es_mx").unwrap(), ["ES-419", "ES"]);
        assert_eq!(deepl_target_languages("de_at").unwrap(), ["DE"]);
        assert!(deepl_target_languages("tlh_aa").is_none());
    }

    #[test]
    fn unsupported_language_lists_are_unknown_and_same_base_sources_are_omitted() {
        assert_eq!(select_target_language("en_gb", None).unwrap(), "EN-GB");
        assert!(matches!(
            select_target_language("de_de", Some(&["FR".to_owned()])),
            Err(AiError::UnsupportedTargetLanguage)
        ));
        assert_eq!(source_language_for_entry("de_at", "de_de"), None);
        assert_eq!(
            source_language_for_entry("en_us", "de_de"),
            Some("EN".into())
        );
        let body = translation_body(&["Example"], "DE", None);
        assert_eq!(body["target_lang"], "DE");
        assert!(body.get("source_lang").is_none());
    }

    #[tokio::test]
    async fn language_api_filters_unusable_targets_and_translation_omits_same_base_source() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/languages"))
            .and(query_param("resource", "translate_text"))
            .and(header("Authorization", "DeepL-Auth-Key mock-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"lang":"DE","name":"German","usable_as_source":true,"usable_as_target":false,"status":"active"},
                {"lang":"de","name":"German","usable_as_source":true,"usable_as_target":true,"status":"active"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/translate"))
            .and(body_string_contains("\"target_lang\":\"DE\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "translations": [{"text": "Beispiel"}]
            })))
            .mount(&server)
            .await;

        let provider = provider_for(&server);
        let available = provider
            .fetch_supported_target_languages(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(available, ["de"]);
        assert_eq!(
            select_target_language("de_de", Some(&available)).unwrap(),
            "DE"
        );
        provider
            .translate_batch(
                &[TranslationEntry::new(
                    1,
                    "item.example",
                    "Example",
                    "de_at",
                    "de_de",
                )],
                &TranslationContext::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
        assert_eq!(body["target_lang"], "DE");
        assert!(body.get("source_lang").is_none());
    }

    #[tokio::test]
    async fn target_language_errors_are_reported_as_unsupported() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/languages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"lang":"ES-419","usable_as_target":true},
                {"lang":"ES","usable_as_target":true}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/translate"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_string("{\"message\":\"Value for 'target_lang' not supported.\"}"),
            )
            .mount(&server)
            .await;

        let error = provider_for(&server)
            .translate_batch(
                &[TranslationEntry::new(
                    1,
                    "item.example",
                    "Example",
                    "en_us",
                    "es_mx",
                )],
                &TranslationContext::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, AiError::UnsupportedTargetLanguage);
    }
}
