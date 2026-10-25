use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{header, method, path},
};
use yc_validation::ValidationIssue;

use crate::http::ProviderHttp;
use crate::{
    AiError, CredentialBackend, CredentialManager, CredentialStatus, JobManager,
    OpenAiCompatibleProvider, ProviderId, TranslationContext, TranslationEntry, TranslationOptions,
    TranslationProgress, TranslationProvider, deepl::deepl_base_url, translate_entries,
    url_policy::validate_provider_url,
};

fn entry(id: i64, source: &str) -> TranslationEntry {
    TranslationEntry::new(id, format!("key.{id}"), source, "en_us", "ja_jp")
}

fn mock_url(server: &MockServer, path: &str) -> Url {
    let mut url = Url::parse(&server.uri()).expect("mock server URL should be valid");
    url.set_host(Some("127.0.0.1"))
        .expect("loopback host should be valid");
    url.set_path(path);
    url
}

#[test]
fn batch_splitting_uses_entry_count_and_source_character_limits() {
    assert_eq!(TranslationOptions::default().effective_concurrency(), 2);
    assert_eq!(
        TranslationOptions { concurrency: 0 }.effective_concurrency(),
        1
    );
    assert_eq!(
        TranslationOptions { concurrency: 9 }.effective_concurrency(),
        4
    );

    let by_count: Vec<_> = (0..51).map(|id| entry(id, "x")).collect();
    let batches = crate::batch::split_batches(&by_count);
    assert_eq!(
        batches.iter().map(|range| range.len()).collect::<Vec<_>>(),
        [50, 1]
    );

    let by_characters = [
        entry(1, &"あ".repeat(7_000)),
        entry(2, &"い".repeat(5_001)),
        entry(3, &"う".repeat(12_001)),
    ];
    let batches = crate::batch::split_batches(&by_characters);
    assert_eq!(
        batches.iter().map(|range| range.len()).collect::<Vec<_>>(),
        [1, 1, 1]
    );
    assert_eq!(
        by_characters[batches[0].start].source.chars().count(),
        7_000
    );
}

#[test]
fn provider_urls_allow_https_and_resolved_loopback_http_only() {
    assert!(validate_provider_url(&Url::parse("https://api.example.test/v1").unwrap()).is_ok());
    assert!(validate_provider_url(&Url::parse("http://localhost:11434/v1").unwrap()).is_ok());
    assert!(validate_provider_url(&Url::parse("http://127.23.4.5:11434/v1").unwrap()).is_ok());
    assert!(validate_provider_url(&Url::parse("http://192.168.1.8:11434/v1").unwrap()).is_err());
    assert!(
        validate_provider_url(&Url::parse("https://example.test/v1?key=secret").unwrap()).is_err()
    );
}

#[tokio::test]
async fn redirect_policy_stops_before_following_a_non_loopback_destination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/redirect"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", "http://192.168.1.8:8912/v1"),
        )
        .mount(&server)
        .await;
    let http = ProviderHttp::new("redirect-test").unwrap();
    let endpoint = mock_url(&server, "/redirect");
    let response = http
        .send_with_retry(
            || http.client().request(Method::GET, endpoint.clone()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn retries_rate_limit_and_honors_retry_after() {
    let server = MockServer::start().await;
    let count = Arc::new(AtomicUsize::new(0));
    let responder = StatusSequence {
        count: Arc::clone(&count),
        first_status: 429,
        retry_after: Some("0"),
        success_body: json!({"ok": true}),
    };
    Mock::given(method("POST"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let http = ProviderHttp::new("retry-test").unwrap();
    let endpoint = mock_url(&server, "/translate");
    let started = Instant::now();
    let response = http
        .send_with_retry(
            || {
                http.client()
                    .post(endpoint.clone())
                    .json(&json!({"source": "secret text"}))
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn retries_server_errors_three_times_then_fails_and_auth_fails_immediately() {
    let server = MockServer::start().await;
    let count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .respond_with(AlwaysStatus {
            count: Arc::clone(&count),
            status: 500,
        })
        .mount(&server)
        .await;
    let http = ProviderHttp::new("retry-test").unwrap();
    let endpoint = mock_url(&server, "/server-error");
    let result = http
        .send_with_retry(
            || http.client().post(endpoint.clone()),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(
        result.unwrap_err(),
        AiError::ProviderResponse { status: 500 }
    );
    assert_eq!(count.load(Ordering::SeqCst), 4);

    let auth_server = MockServer::start().await;
    let auth_count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(CountingResponse {
            count: Arc::clone(&auth_count),
            response: ResponseTemplate::new(401),
        })
        .mount(&auth_server)
        .await;
    let auth_url = mock_url(&auth_server, "/auth");
    let result = http
        .send_with_retry(
            || http.client().get(auth_url.clone()),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.unwrap_err(), AiError::ProviderAuth);
    assert_eq!(auth_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn structured_responses_are_matched_by_id_and_only_bad_entries_are_rejected() {
    let server = MockServer::start().await;
    let response_json = json!({
        "choices": [{
            "message": {
                "content": serde_json::to_string(&json!({"translations": [
                    {"id": 2, "translation": "こんにちは"},
                    {"id": 1, "translation": "placeholder lost"},
                    {"id": 1, "translation": "a second %s"},
                    {"id": 4, "translation": 7},
                    {"id": 99, "translation": "unknown id"}
                ]})).unwrap()
            }
        }]
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_json))
        .mount(&server)
        .await;

    let base = mock_url(&server, "/v1").to_string();
    let provider = OpenAiCompatibleProvider::new(&base, None, "mock-model").unwrap();
    let entries = [
        entry(1, "Item %s"),
        entry(2, "Hello"),
        entry(3, "Absent"),
        entry(4, "Malformed response"),
    ];
    let result = translate_entries(
        &provider,
        &entries,
        &TranslationContext::default(),
        &CancellationToken::new(),
        TranslationOptions::default(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.accepted.len(), 1);
    assert_eq!(result.accepted[0].id, 2);
    assert_eq!(result.accepted[0].translation, "こんにちは");
    assert_eq!(
        result.rejected.iter().filter(|item| item.id == 1).count(),
        2
    );
    assert!(result.rejected.iter().any(|item| {
        item.id == 1
            && item
                .reasons
                .iter()
                .any(|reason| matches!(reason, ValidationIssue::PlaceholderMismatch { .. }))
    }));
    assert!(result.rejected.iter().any(|item| item.id == 3));
    assert!(result.rejected.iter().any(|item| {
        item.id == 4
            && item
                .reasons
                .iter()
                .any(|reason| matches!(reason, ValidationIssue::EmptyTranslation))
    }));
    assert!(result.rejected.iter().any(|item| item.id == 99));
    assert!(
        result
            .rejected
            .iter()
            .filter(|item| item.id == 1)
            .all(|item| {
                item.reasons
                    .iter()
                    .any(|reason| matches!(reason, ValidationIssue::DuplicateId))
            })
    );
}

#[test]
fn openai_responses_output_is_parsed_as_structured_json() {
    let output = crate::openai::parse_responses_output(json!({
        "status": "completed",
        "output": [{
            "content": [{
                "type": "output_text",
                "text": "{\"translations\":[{\"id\":8,\"translation\":\"機械\"}]}"
            }]
        }]
    }))
    .unwrap();
    assert_eq!(output[0].id, 8);
    assert_eq!(output[0].translation, "機械");
}

#[tokio::test]
async fn compatible_provider_falls_back_when_response_format_is_rejected() {
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(StructuredFallbackResponder {
            count: Arc::new(AtomicUsize::new(0)),
            requests: Arc::clone(&requests),
        })
        .mount(&server)
        .await;

    let provider =
        OpenAiCompatibleProvider::new(mock_url(&server, "/v1").as_ref(), None, "mock-model")
            .unwrap();
    let result = provider
        .translate_batch(
            &[entry(1, "Hello")],
            &TranslationContext::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result[0].translation, "こんにちは");

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let structured: Value = serde_json::from_slice(&requests[0]).unwrap();
    let fallback: Value = serde_json::from_slice(&requests[1]).unwrap();
    assert!(structured.get("response_format").is_some());
    assert!(fallback.get("response_format").is_none());
    assert!(
        fallback["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("JSON object")
    );
}

#[tokio::test]
async fn compatible_provider_sends_structured_request_to_nested_base_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/openai/chat/completions"))
        .and(header("Authorization", "Bearer mock-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "content": "{\"translations\":[{\"id\":1,\"translation\":\"こんにちは\"}]}"
                }
            }]
        })))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        mock_url(&server, "/v1beta/openai/").as_ref(),
        Some("mock-key".into()),
        "gemini-model",
    )
    .unwrap();
    let result = provider
        .translate_batch(
            &[entry(1, "Hello")],
            &TranslationContext::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result[0].translation, "こんにちは");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "gemini-model");
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
}

#[tokio::test]
async fn cancellation_discards_an_in_flight_batch_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(250))
                .set_body_json(json!({
                    "choices": [{"message": {"content": "{\"translations\":[{\"id\":1,\"translation\":\"ok\"}]}"}}]
                })),
        )
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(mock_url(&server, "/v1").as_ref(), None, "mock-model")
            .unwrap();
    let token = CancellationToken::new();
    let request_token = token.clone();
    let task = tokio::spawn(async move {
        translate_entries(
            &provider,
            &[entry(1, "Hello")],
            &TranslationContext::default(),
            &request_token,
            TranslationOptions::default(),
            None,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    token.cancel();
    assert_eq!(task.await.unwrap().unwrap_err(), AiError::Cancelled);
}

#[test]
fn deepl_free_and_pro_endpoints_are_selected_from_the_key_suffix() {
    assert_eq!(
        deepl_base_url("example:fx"),
        "https://api-free.deepl.com/v2/"
    );
    assert_eq!(deepl_base_url("example"), "https://api.deepl.com/v2/");
}

#[tokio::test]
async fn credential_manager_uses_in_memory_backend_and_returns_only_status_to_ui() {
    let values = Arc::new(Mutex::new(HashMap::new()));
    let manager = CredentialManager::new(MemoryCredentialBackend {
        values: Arc::clone(&values),
    })
    .unwrap();

    let missing = manager.status(ProviderId::OpenAi).await.unwrap();
    assert_eq!(
        missing,
        CredentialStatus {
            configured: false,
            provider: ProviderId::OpenAi,
            account_label: None,
        }
    );
    let saved = manager
        .save_api_key(ProviderId::OpenAi, "not-a-real-key")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(saved).unwrap(),
        json!({"configured": true, "provider": "openai"})
    );
    let secret = manager
        .load_api_key(ProviderId::OpenAi)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(secret.expose_secret(), "not-a-real-key");
    assert_eq!(format!("{secret:?}"), "Secret([redacted])");
    assert_eq!(
        manager.remove_api_key(ProviderId::OpenAi).await.unwrap(),
        CredentialStatus {
            configured: false,
            provider: ProviderId::OpenAi,
            account_label: None,
        }
    );
}

#[tokio::test]
async fn job_manager_allows_one_job_and_cancels_by_uuid() {
    let manager = JobManager::default();
    let job = manager.start_job().unwrap();
    let token = job.cancellation_token();
    assert!(matches!(
        manager.start_job(),
        Err(AiError::JobAlreadyRunning)
    ));
    assert!(manager.cancel_job(job.id()));
    assert!(token.is_cancelled());
    drop(job);
    assert!(manager.start_job().is_ok());
}

#[tokio::test]
async fn progress_reports_completed_batch_counts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "{\"translations\":[{\"id\":1,\"translation\":\"ok\"}]}"}}]
        })))
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(mock_url(&server, "/v1").as_ref(), None, "mock-model")
            .unwrap();
    let progress = Arc::new(Mutex::new(Vec::<TranslationProgress>::new()));
    let capture = Arc::clone(&progress);
    let callback = move |value| capture.lock().unwrap().push(value);
    let result = translate_entries(
        &provider,
        &[entry(1, "Hello")],
        &TranslationContext::default(),
        &CancellationToken::new(),
        TranslationOptions::default(),
        Some(&callback),
    )
    .await
    .unwrap();
    assert_eq!(result.accepted.len(), 1);
    assert_eq!(
        *progress.lock().unwrap(),
        [TranslationProgress {
            completed: 1,
            total: 1
        }]
    );
}

struct StatusSequence {
    count: Arc<AtomicUsize>,
    first_status: u16,
    retry_after: Option<&'static str>,
    success_body: Value,
}

impl Respond for StatusSequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        if self.count.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut response = ResponseTemplate::new(self.first_status);
            if let Some(retry_after) = self.retry_after {
                response = response.insert_header("Retry-After", retry_after);
            }
            response
        } else {
            ResponseTemplate::new(200).set_body_json(self.success_body.clone())
        }
    }
}

struct CountingResponse {
    count: Arc<AtomicUsize>,
    response: ResponseTemplate,
}

struct AlwaysStatus {
    count: Arc<AtomicUsize>,
    status: u16,
}

impl Respond for AlwaysStatus {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.count.fetch_add(1, Ordering::SeqCst);
        ResponseTemplate::new(self.status)
    }
}

impl Respond for CountingResponse {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.response.clone()
    }
}

struct StructuredFallbackResponder {
    count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Respond for StructuredFallbackResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.requests.lock().unwrap().push(request.body.clone());
        if self.count.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(400).set_body_json(json!({
                "error": {"message": "response_format is not supported"}
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "{\"translations\":[{\"id\":1,\"translation\":\"こんにちは\"}]}"}}]
            }))
        }
    }
}

struct MemoryCredentialBackend {
    values: Arc<Mutex<HashMap<ProviderId, String>>>,
}

impl CredentialBackend for MemoryCredentialBackend {
    fn get(
        &mut self,
        provider: ProviderId,
    ) -> std::result::Result<Option<String>, crate::credentials::CredentialStoreError> {
        Ok(self.values.lock().unwrap().get(&provider).cloned())
    }

    fn set(
        &mut self,
        provider: ProviderId,
        secret: &str,
    ) -> std::result::Result<(), crate::credentials::CredentialStoreError> {
        self.values
            .lock()
            .unwrap()
            .insert(provider, secret.to_owned());
        Ok(())
    }

    fn delete(
        &mut self,
        provider: ProviderId,
    ) -> std::result::Result<(), crate::credentials::CredentialStoreError> {
        self.values.lock().unwrap().remove(&provider);
        Ok(())
    }
}
