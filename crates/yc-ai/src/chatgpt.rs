use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use reqwest::{Method, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
use yc_validation::AiTranslation;

use crate::{
    AiError, CredentialManager, Result, TranslationContext, TranslationEntry, TranslationProvider,
    http::{ProviderHttp, validate_auth_header},
    prompt,
    provider::ProviderCapabilities,
};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_ORIGIN: &str = "https://auth.openai.com";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const RESPONSES_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
// Codex CLI のバージョン。古い値では新モデルが返らない可能性があるため適宜更新する。
const CODEX_CLIENT_VERSION: &str = "0.162.1";
const MAX_MODELS_RESPONSE_BYTES: usize = 1024 * 1024;
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_URI: &str = "http://localhost:1455/auth/callback";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH_SKEW: Duration = Duration::from_secs(60);
const MAX_TOKEN_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CALLBACK_REQUEST_BYTES: usize = 16 * 1024;
const MAX_ERROR_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_STREAM_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChatGptModel {
    pub id: &'static str,
    pub name: &'static str,
}

pub const CHATGPT_MODELS: &[ChatGptModel] = &[
    ChatGptModel {
        id: "gpt-6-sol",
        name: "GPT-6 Sol",
    },
    ChatGptModel {
        id: "gpt-6-luna",
        name: "GPT-6 Luna",
    },
    ChatGptModel {
        id: "gpt-5.6-sol",
        name: "GPT-5.6 Sol",
    },
    ChatGptModel {
        id: "gpt-5.6-terra",
        name: "GPT-5.6 Terra",
    },
    ChatGptModel {
        id: "gpt-5.6-luna",
        name: "GPT-5.6 Luna",
    },
    ChatGptModel {
        id: "gpt-5.5",
        name: "GPT-5.5",
    },
];

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatGptCredential {
    #[serde(rename = "type")]
    credential_type: String,
    access: String,
    refresh: String,
    /// Fumuの資格情報形式と揃え、UNIX epochからのミリ秒で期限を保存する。
    expires: u64,
    account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

impl fmt::Debug for ChatGptCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptCredential")
            .field("credential_type", &self.credential_type)
            .field("access", &"[redacted]")
            .field("refresh", &"[redacted]")
            .field("expires", &self.expires)
            .field("account_id", &"[redacted]")
            .field("email", &self.email.as_deref().map(|_| "[redacted]"))
            .finish()
    }
}

impl ChatGptCredential {
    fn validate(&self) -> Result<()> {
        if self.credential_type != "oauth"
            || self.access.is_empty()
            || self.refresh.is_empty()
            || self.expires == 0
            || self.account_id.is_empty()
        {
            return Err(AiError::InvalidConfiguration);
        }
        validate_auth_header(&format!("Bearer {}", self.access))?;
        Ok(())
    }

    fn serialize(&self) -> Result<String> {
        self.validate()?;
        serde_json::to_string(self).map_err(|_| AiError::InvalidConfiguration)
    }
}

fn parse_credential(serialized: &str) -> Result<ChatGptCredential> {
    if serialized.len() > MAX_TOKEN_RESPONSE_BYTES {
        return Err(AiError::InvalidConfiguration);
    }
    let credential: ChatGptCredential =
        serde_json::from_str(serialized).map_err(|_| AiError::InvalidConfiguration)?;
    credential.validate()?;
    Ok(credential)
}

pub(crate) fn validate_serialized_credential(serialized: &str) -> Result<()> {
    parse_credential(serialized).map(|_| ())
}

pub(crate) fn account_label_from_credential(serialized: &str) -> Option<String> {
    parse_credential(serialized)
        .ok()?
        .email
        .filter(|email| !email.trim().is_empty())
}

pub fn valid_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 128
        && model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_' | b':'))
}

/// ブラウザーを開く処理はTauri側に置き、このcrateはURL生成とcallback処理を分離する。
pub struct ChatGptLogin {
    listener: TcpListener,
    verifier: String,
    state: String,
    cancellation: CancellationToken,
    http: reqwest::Client,
    token_endpoint: Url,
}

impl ChatGptLogin {
    pub async fn start(cancellation: CancellationToken) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AddrInUse {
                    AiError::ChatGptCallbackPortInUse
                } else {
                    AiError::Network
                }
            })?;
        let http = reqwest::Client::builder()
            .connect_timeout(TOKEN_TIMEOUT)
            .timeout(TOKEN_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AiError::Network)?;
        let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let state = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let mut token_endpoint =
            Url::parse(AUTH_ORIGIN).map_err(|_| AiError::InvalidConfiguration)?;
        token_endpoint.set_path("/oauth/token");
        Ok(Self {
            listener,
            verifier,
            state,
            cancellation,
            http,
            token_endpoint,
        })
    }

    pub fn authorization_url(&self) -> String {
        authorization_url(&self.state, &self.verifier)
    }

    /// timeoutまたはcancelで戻るとlistenerがdropされ、loopback serverも閉じる。
    pub async fn wait(self) -> Result<ChatGptAccountInfo> {
        let result = timeout(LOGIN_TIMEOUT, self.wait_for_callback()).await;
        match result {
            Err(_) => Err(AiError::ChatGptLoginTimedOut),
            Ok(Err(AiError::Cancelled | AiError::ChatGptLoginCancelled)) => {
                Err(AiError::ChatGptLoginCancelled)
            }
            Ok(Err(error)) => Err(error),
            Ok(Ok(credential)) => Ok(ChatGptAccountInfo {
                serialized_credential: credential.serialize()?,
            }),
        }
    }

    async fn wait_for_callback(&self) -> Result<ChatGptCredential> {
        let mut handled = false;
        loop {
            let accepted = tokio::select! {
                _ = self.cancellation.cancelled() => return Err(AiError::ChatGptLoginCancelled),
                accepted = self.listener.accept() => accepted.map_err(|_| AiError::Network)?,
            };
            let mut socket = accepted.0;
            let request = tokio::select! {
                _ = self.cancellation.cancelled() => return Err(AiError::ChatGptLoginCancelled),
                request = read_callback_request(&mut socket) => request?,
            };
            match callback_result(&request, &self.state, &mut handled) {
                CallbackResult::NotFound => {
                    write_callback_page(&mut socket, 404, false).await?;
                }
                CallbackResult::WrongState => {
                    write_callback_page(&mut socket, 400, false).await?;
                }
                CallbackResult::Duplicate => {
                    write_callback_page(&mut socket, 409, false).await?;
                }
                CallbackResult::Declined => {
                    write_callback_page(&mut socket, 400, false).await?;
                    return Err(AiError::ChatGptLoginFailed);
                }
                CallbackResult::Invalid => {
                    write_callback_page(&mut socket, 400, false).await?;
                }
                CallbackResult::Code(code) => {
                    let exchange = exchange_authorization_code(
                        &self.http,
                        &self.token_endpoint,
                        &code,
                        &self.verifier,
                        &self.cancellation,
                    )
                    .await;
                    write_callback_page(
                        &mut socket,
                        if exchange.is_ok() { 200 } else { 502 },
                        exchange.is_ok(),
                    )
                    .await?;
                    return exchange;
                }
            }
        }
    }
}

pub struct ChatGptAccountInfo {
    serialized_credential: String,
}

impl ChatGptAccountInfo {
    pub fn into_credential(self) -> String {
        self.serialized_credential
    }
}

fn authorization_url(state: &str, verifier: &str) -> String {
    let mut url = Url::parse(AUTH_ORIGIN).expect("constant OAuth origin is valid");
    url.set_path("/oauth/authorize");
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", CALLBACK_URI)
        .append_pair("scope", "openid profile email offline_access")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", "yakucraft");
    url.into()
}

async fn read_callback_request(socket: &mut TcpStream) -> Result<String> {
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while request.len() < MAX_CALLBACK_REQUEST_BYTES {
        let read = socket
            .read(&mut chunk)
            .await
            .map_err(|_| AiError::Network)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if request.len() >= MAX_CALLBACK_REQUEST_BYTES {
        return Err(AiError::InvalidProviderResponse);
    }
    String::from_utf8(request).map_err(|_| AiError::InvalidProviderResponse)
}

enum CallbackResult {
    NotFound,
    WrongState,
    Duplicate,
    Declined,
    Invalid,
    Code(String),
}

fn callback_result(request: &str, expected_state: &str, handled: &mut bool) -> CallbackResult {
    let Some(target) = request.lines().next().and_then(|line| {
        let mut parts = line.split_ascii_whitespace();
        (parts.next() == Some("GET"))
            .then(|| parts.next())
            .flatten()
    }) else {
        return CallbackResult::Invalid;
    };
    let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
        return CallbackResult::Invalid;
    };
    if url.path() != "/auth/callback" {
        return CallbackResult::NotFound;
    }
    if *handled {
        return CallbackResult::Duplicate;
    }
    let received_state = url
        .query_pairs()
        .find(|(name, _)| name == "state")
        .map(|(_, value)| value.into_owned());
    if received_state.as_deref() != Some(expected_state) {
        return CallbackResult::WrongState;
    }
    if url.query_pairs().any(|(name, _)| name == "error") {
        *handled = true;
        return CallbackResult::Declined;
    }
    let code = url
        .query_pairs()
        .find(|(name, _)| name == "code")
        .map(|(_, value)| value.into_owned());
    match code.filter(|code| !code.is_empty() && code.len() <= 8192) {
        Some(code) => {
            *handled = true;
            CallbackResult::Code(code)
        }
        None => CallbackResult::Invalid,
    }
}

async fn write_callback_page(socket: &mut TcpStream, status: u16, success: bool) -> Result<()> {
    let (reason, body) = if success {
        (
            "OK",
            "ChatGPTにログインしました。このページを閉じてアプリに戻ってください。",
        )
    } else {
        (
            "Request Rejected",
            "ログインを完了できませんでした。アプリに戻ってもう一度お試しください。",
        )
    };
    let html = format!(
        "<!doctype html><html lang=\"ja\"><meta charset=\"utf-8\"><title>ChatGPT</title><body>{body}</body></html>"
    );
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        html.len()
    );
    socket
        .write_all(format!("{headers}{html}").as_bytes())
        .await
        .map_err(|_| AiError::Network)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    id_token: Option<String>,
}

impl TokenResponse {
    fn validate(&self) -> Result<()> {
        if self.access_token.is_empty() || self.expires_in == 0 {
            return Err(AiError::InvalidProviderResponse);
        }
        validate_auth_header(&format!("Bearer {}", self.access_token))
    }
}

async fn token_request(
    http: &reqwest::Client,
    token_endpoint: &Url,
    form: &[(&str, &str)],
    cancellation: &CancellationToken,
) -> Result<TokenResponse> {
    let response = tokio::select! {
        _ = cancellation.cancelled() => return Err(AiError::Cancelled),
        response = http.post(token_endpoint.clone()).form(form).send() => response.map_err(|_| AiError::Network)?,
    };
    if !response.status().is_success() {
        return Err(AiError::ProviderResponse {
            status: response.status().as_u16(),
        });
    }
    let bytes = read_bounded_response(response, MAX_TOKEN_RESPONSE_BYTES, cancellation).await?;
    let token: TokenResponse =
        serde_json::from_slice(&bytes).map_err(|_| AiError::InvalidProviderResponse)?;
    token.validate()?;
    Ok(token)
}

async fn read_bounded_response(
    response: Response,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = tokio::select! {
        _ = cancellation.cancelled() => return Err(AiError::Cancelled),
        chunk = stream.next() => chunk,
    } {
        let chunk = chunk.map_err(|_| AiError::Network)?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(AiError::InvalidProviderResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn exchange_authorization_code(
    http: &reqwest::Client,
    token_endpoint: &Url,
    code: &str,
    verifier: &str,
    cancellation: &CancellationToken,
) -> Result<ChatGptCredential> {
    let token = token_request(
        http,
        token_endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", CALLBACK_URI),
        ],
        cancellation,
    )
    .await?;
    let refresh = token
        .refresh_token
        .filter(|refresh| !refresh.is_empty())
        .ok_or(AiError::InvalidProviderResponse)?;
    let id_claims = token.id_token.as_deref().and_then(jwt_claims);
    let access_claims = jwt_claims(&token.access_token);
    let account_id = account_id(&id_claims)
        .or_else(|| account_id(&access_claims))
        .ok_or(AiError::InvalidProviderResponse)?;
    Ok(ChatGptCredential {
        credential_type: "oauth".to_owned(),
        access: token.access_token,
        refresh,
        expires: expires_at(token.expires_in),
        account_id,
        email: id_claims
            .as_ref()
            .and_then(email_claim)
            .or_else(|| access_claims.as_ref().and_then(email_claim)),
    })
}

fn expires_at(expires_in: u64) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .saturating_add(u128::from(expires_in).saturating_mul(1000))
        .min(u128::from(u64::MAX)) as u64
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    if payload.is_empty() || payload.len() > 87_384 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn account_id(claims: &Option<Value>) -> Option<String> {
    let claims = claims.as_ref()?.as_object()?;
    claims
        .get("chatgpt_account_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(Value::as_object)
                .and_then(|auth| auth.get("chatgpt_account_id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .map(str::to_owned)
}

fn email_claim(claims: &Value) -> Option<String> {
    claims
        .get("email")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub struct ChatGptProvider {
    http: ProviderHttp,
    responses_endpoint: Url,
    token_endpoint: Url,
    model: String,
    credential: tokio::sync::Mutex<ChatGptCredential>,
    credential_generation: AtomicU64,
    credentials: CredentialManager,
    token_http: reqwest::Client,
}

impl ChatGptProvider {
    pub fn new(
        serialized_credential: &str,
        model: impl Into<String>,
        credentials: CredentialManager,
    ) -> Result<Self> {
        let responses_endpoint =
            Url::parse(RESPONSES_ENDPOINT).map_err(|_| AiError::InvalidConfiguration)?;
        let token_endpoint =
            Url::parse(TOKEN_ENDPOINT).map_err(|_| AiError::InvalidConfiguration)?;
        Self::with_endpoints(
            serialized_credential,
            model.into(),
            credentials,
            responses_endpoint,
            token_endpoint,
        )
    }

    fn with_endpoints(
        serialized_credential: &str,
        model: String,
        credentials: CredentialManager,
        responses_endpoint: Url,
        token_endpoint: Url,
    ) -> Result<Self> {
        if !valid_model_id(&model)
            || responses_endpoint.scheme() != "https"
                && !responses_endpoint.host_str().is_some_and(is_loopback_host)
            || token_endpoint.scheme() != "https"
                && !token_endpoint.host_str().is_some_and(is_loopback_host)
        {
            return Err(AiError::InvalidConfiguration);
        }
        let credential = parse_credential(serialized_credential)?;
        let credential_generation = credentials.chatgpt_credential_generation();
        let http = ProviderHttp::new_fixed("chatgpt")?;
        let token_http = reqwest::Client::builder()
            .connect_timeout(TOKEN_TIMEOUT)
            .timeout(TOKEN_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AiError::Network)?;
        Ok(Self {
            http,
            responses_endpoint,
            token_endpoint,
            model,
            credential: tokio::sync::Mutex::new(credential),
            credential_generation: AtomicU64::new(credential_generation),
            credentials,
            token_http,
        })
    }

    async fn credential_for_request(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<RequestCredential> {
        let mut credential = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            credential = self.credential.lock() => credential,
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            result = self.sync_credential_locked(&mut credential) => result?,
        }
        if credential.expires <= now_ms().saturating_add(REFRESH_SKEW.as_millis() as u64) {
            self.refresh_locked(&mut credential, cancellation)
                .await
                .map_err(refresh_error)?;
        }
        Ok(RequestCredential {
            access: credential.access.clone(),
            account_id: credential.account_id.clone(),
        })
    }

    async fn refresh_after_unauthorized(
        &self,
        failed_access: &str,
        cancellation: &CancellationToken,
    ) -> Result<RequestCredential> {
        let mut credential = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            credential = self.credential.lock() => credential,
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            result = self.sync_credential_locked(&mut credential) => result?,
        }
        if credential.access == failed_access {
            self.refresh_locked(&mut credential, cancellation)
                .await
                .map_err(refresh_error)?;
        }
        Ok(RequestCredential {
            access: credential.access.clone(),
            account_id: credential.account_id.clone(),
        })
    }

    async fn sync_credential_locked(&self, credential: &mut ChatGptCredential) -> Result<()> {
        // 有効期限内でも保存値を確認する。古い provider は logout 後や別アカウントの batch を送れない。
        let Some((serialized, generation)) = self.credentials.load_chatgpt_credential().await?
        else {
            return Err(AiError::ChatGptReauthenticationRequired);
        };
        let current = parse_credential(&serialized)?;
        if current.account_id != credential.account_id {
            return Err(AiError::ChatGptReauthenticationRequired);
        }
        *credential = current;
        self.credential_generation
            .store(generation, Ordering::Release);
        Ok(())
    }

    async fn refresh_locked(
        &self,
        credential: &mut ChatGptCredential,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let generation = self.credential_generation.load(Ordering::Acquire);
        let expected_credential = ChatGptCredential::serialize(credential)?;
        let token = token_request(
            &self.token_http,
            &self.token_endpoint,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", &credential.refresh),
            ],
            cancellation,
        )
        .await?;
        let id_claims = token.id_token.as_deref().and_then(jwt_claims);
        let access_claims = jwt_claims(&token.access_token);
        let updated = ChatGptCredential {
            credential_type: "oauth".to_owned(),
            access: token.access_token,
            refresh: token
                .refresh_token
                .filter(|refresh| !refresh.is_empty())
                .unwrap_or_else(|| credential.refresh.clone()),
            expires: expires_at(token.expires_in),
            account_id: account_id(&id_claims)
                .or_else(|| account_id(&access_claims))
                .unwrap_or_else(|| credential.account_id.clone()),
            email: id_claims
                .as_ref()
                .and_then(email_claim)
                .or_else(|| access_claims.as_ref().and_then(email_claim))
                .or_else(|| credential.email.clone()),
        };
        let serialized = updated.serialize()?;
        // 世代と元の資格情報を保存直前に照合し、競合時は勝者の資格情報を採用してログアウト済みなら拒否する。
        let Some((current_serialized, generation)) = self
            .credentials
            .save_chatgpt_credential_if_generation(&serialized, &expected_credential, generation)
            .await?
        else {
            return Err(AiError::ChatGptReauthenticationRequired);
        };
        self.credential_generation
            .store(generation, Ordering::Release);
        let current = parse_credential(&current_serialized)?;
        if current.account_id != credential.account_id {
            return Err(AiError::ChatGptReauthenticationRequired);
        }
        *credential = current;
        Ok(())
    }

    pub async fn list_models(&self, cancellation: &CancellationToken) -> Result<Vec<String>> {
        let mut endpoint = self.responses_endpoint.clone();
        endpoint.set_path("/backend-api/codex/models");
        endpoint
            .query_pairs_mut()
            .append_pair("client_version", CODEX_CLIENT_VERSION);
        let send = |credential: RequestCredential| {
            let endpoint = endpoint.clone();
            async move {
                self.http
                    .send_with_retry(
                        || {
                            self.http
                                .client()
                                .get(endpoint.clone())
                                .header(reqwest::header::ACCEPT, "application/json")
                                .bearer_auth(&credential.access)
                                .header("ChatGPT-Account-Id", &credential.account_id)
                                .header("originator", "yakucraft")
                        },
                        cancellation,
                    )
                    .await
            }
        };
        let current = self.credential_for_request(cancellation).await?;
        let response = match send(RequestCredential {
            access: current.access.clone(),
            account_id: current.account_id,
        })
        .await
        {
            Err(AiError::ProviderAuth) => {
                let renewed = self
                    .refresh_after_unauthorized(&current.access, cancellation)
                    .await?;
                send(renewed).await.map_err(|error| match error {
                    AiError::ProviderAuth => AiError::ChatGptReauthenticationRequired,
                    other => other,
                })?
            }
            result => result?,
        };
        ensure_success(response.status().as_u16())?;
        let bytes =
            read_bounded_response(response, MAX_MODELS_RESPONSE_BYTES, cancellation).await?;
        #[derive(Deserialize)]
        struct Models {
            models: Vec<Model>,
        }
        #[derive(Deserialize)]
        struct Model {
            slug: String,
            visibility: String,
            priority: Option<i64>,
        }
        let mut models: Vec<Model> = serde_json::from_slice::<Models>(&bytes)
            .map_err(|_| AiError::InvalidProviderResponse)?
            .models
            .into_iter()
            .filter(|model| model.visibility == "list")
            .collect();
        if models.is_empty() || models.iter().any(|model| !valid_model_id(&model.slug)) {
            return Err(AiError::InvalidProviderResponse);
        }
        models.sort_by_key(|model| model.priority.unwrap_or(i64::MAX));
        Ok(models.into_iter().map(|model| model.slug).collect())
    }

    async fn send_responses(
        &self,
        body: Value,
        cancellation: &CancellationToken,
    ) -> Result<Response> {
        let current = self.credential_for_request(cancellation).await?;
        match self
            .send_with_credential(&body, &current, cancellation)
            .await
        {
            Err(AiError::ProviderAuth) => {
                let renewed = self
                    .refresh_after_unauthorized(&current.access, cancellation)
                    .await?;
                match self
                    .send_with_credential(&body, &renewed, cancellation)
                    .await
                {
                    Err(AiError::ProviderAuth) => Err(AiError::ChatGptReauthenticationRequired),
                    result => result,
                }
            }
            result => result,
        }
    }

    async fn send_with_credential(
        &self,
        body: &Value,
        credential: &RequestCredential,
        cancellation: &CancellationToken,
    ) -> Result<Response> {
        self.http
            .send_with_retry(
                || {
                    self.http
                        .client()
                        .request(Method::POST, self.responses_endpoint.clone())
                        .header(reqwest::header::ACCEPT, "text/event-stream")
                        .bearer_auth(&credential.access)
                        .header("ChatGPT-Account-Id", &credential.account_id)
                        .header("originator", "yakucraft")
                        .json(body)
                },
                cancellation,
            )
            .await
    }
}

fn refresh_error(error: AiError) -> AiError {
    match error {
        AiError::ProviderResponse { status: 400 | 401 } | AiError::InvalidProviderResponse => {
            AiError::ChatGptReauthenticationRequired
        }
        other => other,
    }
}

struct RequestCredential {
    access: String,
    account_id: String,
}

#[async_trait]
impl TranslationProvider for ChatGptProvider {
    async fn test_connection(&self, cancellation: &CancellationToken) -> Result<()> {
        let body = responses_body(&self.model, "Reply with OK.", "", false);
        let response = self.send_responses(body, cancellation).await?;
        ensure_success(response.status().as_u16())?;
        read_sse_response(response, cancellation).await.map(|_| ())
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
        let user_payload = prompt::user_payload(context, entries)?;
        let structured = responses_body(
            &self.model,
            &prompt::system_instruction(context, entries, false),
            &user_payload,
            true,
        );
        let response = self.send_responses(structured, cancellation).await?;
        let status = response.status().as_u16();
        let response = if matches!(status, 400 | 422) {
            if structured_output_rejected(response, cancellation).await? {
                let fallback = responses_body(
                    &self.model,
                    &prompt::system_instruction(context, entries, true),
                    &user_payload,
                    false,
                );
                self.send_responses(fallback, cancellation).await?
            } else {
                return Err(AiError::ProviderResponse { status });
            }
        } else {
            response
        };
        ensure_success(response.status().as_u16())?;
        let output = read_sse_response(response, cancellation).await?;
        crate::models::parse_translation_payload(&output)
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

fn responses_body(model: &str, instructions: &str, input: &str, structured: bool) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "store": false,
        "instructions": instructions,
        "reasoning": {"summary": "auto"},
        "include": ["reasoning.encrypted_content"],
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": input}]
        }]
    });
    if structured {
        body["text"] = json!({"format": {
            "type": "json_schema",
            "name": "translation_batch",
            "strict": true,
            "schema": crate::openai::translation_schema()
        }});
    }
    body
}

async fn structured_output_rejected(
    response: Response,
    cancellation: &CancellationToken,
) -> Result<bool> {
    let bytes = read_bounded_response(response, MAX_ERROR_RESPONSE_BYTES, cancellation).await?;
    let message = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    Ok(
        (message.contains("json_schema") || message.contains("text.format"))
            && [
                "unsupported",
                "not support",
                "unrecognized",
                "unknown",
                "not allowed",
                "unexpected",
                "invalid parameter",
            ]
            .iter()
            .any(|marker| message.contains(marker)),
    )
}

fn ensure_success(status: u16) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(AiError::ProviderResponse { status })
    }
}

async fn read_sse_response(response: Response, cancellation: &CancellationToken) -> Result<String> {
    let mut stream = response.bytes_stream();
    let mut pending = Vec::new();
    let mut event_data = Vec::new();
    let mut total_bytes = 0usize;
    let mut output = String::new();
    let mut completed = false;
    loop {
        let chunk = tokio::select! {
            _ = cancellation.cancelled() => return Err(AiError::Cancelled),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|_| AiError::Network)?;
        total_bytes = total_bytes.saturating_add(chunk.len());
        if total_bytes > MAX_STREAM_RESPONSE_BYTES {
            return Err(AiError::InvalidProviderResponse);
        }
        pending.extend_from_slice(&chunk);
        while let Some(line_end) = pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<_> = pending.drain(..=line_end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if dispatch_sse_event(&event_data, &mut output, &mut completed)? {
                    return if completed {
                        Ok(output)
                    } else {
                        Err(AiError::InvalidProviderResponse)
                    };
                }
                event_data.clear();
            } else if line.starts_with(b"data:") {
                let data = &line[5..];
                let data = data.strip_prefix(b" ").unwrap_or(data);
                event_data.push(
                    String::from_utf8(data.to_vec())
                        .map_err(|_| AiError::InvalidProviderResponse)?,
                );
            }
        }
    }
    if !pending.is_empty() {
        if pending.last() == Some(&b'\r') {
            pending.pop();
        }
        if pending.starts_with(b"data:") {
            let data = &pending[5..];
            let data = data.strip_prefix(b" ").unwrap_or(data);
            event_data.push(
                String::from_utf8(data.to_vec()).map_err(|_| AiError::InvalidProviderResponse)?,
            );
        }
    }
    dispatch_sse_event(&event_data, &mut output, &mut completed)?;
    if completed {
        Ok(output)
    } else {
        Err(AiError::InvalidProviderResponse)
    }
}

fn dispatch_sse_event(data: &[String], output: &mut String, completed: &mut bool) -> Result<bool> {
    if data.is_empty() {
        return Ok(false);
    }
    let data = data.join("\n");
    if data == "[DONE]" {
        return Ok(true);
    }
    let event: Value = serde_json::from_str(&data).map_err(|_| AiError::InvalidProviderResponse)?;
    match event.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            let delta = event
                .get("delta")
                .and_then(Value::as_str)
                .ok_or(AiError::InvalidProviderResponse)?;
            output.push_str(delta);
        }
        Some("response.completed") => {
            *completed = true;
            if output.is_empty()
                && let Some(text) = event.get("response").and_then(response_output_text)
            {
                output.push_str(text);
            }
        }
        Some("response.failed" | "response.incomplete" | "error") => {
            return Err(AiError::InvalidProviderResponse);
        }
        Some(_) => {}
        None => return Err(AiError::InvalidProviderResponse),
    }
    Ok(false)
}

fn response_output_text(response: &Value) -> Option<&str> {
    response
        .get("output_text")
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .get("output")
                .and_then(Value::as_array)?
                .iter()
                .flat_map(|item| {
                    item.get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .find(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
        })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn is_loopback_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "[::1]" || host == "localhost"
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    use super::*;
    use crate::{CredentialBackend, CredentialStoreError, ProviderId};

    #[derive(Default, Clone)]
    struct MemoryCredentials(Arc<Mutex<Option<String>>>);

    impl CredentialBackend for MemoryCredentials {
        fn get(
            &mut self,
            provider: ProviderId,
        ) -> std::result::Result<Option<String>, CredentialStoreError> {
            if provider == ProviderId::ChatGpt {
                Ok(self.0.lock().unwrap().clone())
            } else {
                Ok(None)
            }
        }

        fn set(
            &mut self,
            provider: ProviderId,
            secret: &str,
        ) -> std::result::Result<(), CredentialStoreError> {
            if provider == ProviderId::ChatGpt {
                *self.0.lock().unwrap() = Some(secret.to_owned());
            }
            Ok(())
        }

        fn delete(
            &mut self,
            provider: ProviderId,
        ) -> std::result::Result<(), CredentialStoreError> {
            if provider == ProviderId::ChatGpt {
                *self.0.lock().unwrap() = None;
            }
            Ok(())
        }
    }

    fn test_credential(access: &str, refresh: &str, expires: u64) -> ChatGptCredential {
        ChatGptCredential {
            credential_type: "oauth".into(),
            access: access.into(),
            refresh: refresh.into(),
            expires,
            account_id: "account-123".into(),
            email: Some("user@example.com".into()),
        }
    }

    fn jwt(claims: Value) -> String {
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }

    async fn provider_for(
        server: &MockServer,
        credential: ChatGptCredential,
    ) -> (ChatGptProvider, MemoryCredentials, CredentialManager) {
        let memory = MemoryCredentials::default();
        let store = CredentialManager::new(memory.clone()).unwrap();
        let serialized = credential.serialize().unwrap();
        store.save_chatgpt_credential(&serialized).await.unwrap();
        let responses =
            Url::parse(&format!("{}/backend-api/codex/responses", server.uri())).unwrap();
        let token = Url::parse(&format!("{}/oauth/token", server.uri())).unwrap();
        (
            ChatGptProvider::with_endpoints(
                &serialized,
                "gpt-6-luna".into(),
                store.clone(),
                responses,
                token,
            )
            .unwrap(),
            memory,
            store,
        )
    }

    #[test]
    fn authorization_url_contains_pkce_state_and_fixed_oauth_parameters() {
        let verifier = "a".repeat(64);
        let url = Url::parse(&authorization_url("state-value", &verifier)).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            url.origin().ascii_serialization(),
            "https://auth.openai.com"
        );
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(query["redirect_uri"], CALLBACK_URI);
        assert_eq!(query["scope"], "openid profile email offline_access");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["state"], "state-value");
        assert_eq!(
            query["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
        );
    }

    #[test]
    fn callback_rejects_wrong_state_oauth_error_and_duplicate_code() {
        let mut handled = false;
        assert!(matches!(
            callback_result(
                "GET /auth/callback?code=x&state=bad HTTP/1.1",
                "good",
                &mut handled
            ),
            CallbackResult::WrongState
        ));
        assert!(!handled);
        assert!(matches!(
            callback_result(
                "GET /auth/callback?error=access_denied&state=good HTTP/1.1",
                "good",
                &mut handled
            ),
            CallbackResult::Declined
        ));
        assert!(handled);
        assert!(matches!(
            callback_result(
                "GET /auth/callback?code=x&state=good HTTP/1.1",
                "good",
                &mut handled
            ),
            CallbackResult::Duplicate
        ));
    }

    #[test]
    fn callback_accepts_a_single_matching_code() {
        let mut handled = false;
        assert!(
            matches!(callback_result("GET /auth/callback?code=abc&state=good HTTP/1.1", "good", &mut handled), CallbackResult::Code(code) if code == "abc")
        );
        assert!(handled);
    }

    #[test]
    fn account_id_is_read_from_the_openai_auth_claims() {
        let claims =
            Some(json!({"https://api.openai.com/auth":{"chatgpt_account_id":"account-123"}}));
        assert_eq!(account_id(&claims).as_deref(), Some("account-123"));
        assert_eq!(
            email_claim(&json!({"email":"user@example.com"})).as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn debug_representation_redacts_tokens_account_and_email() {
        let credential = test_credential("access-secret", "refresh-secret", now_ms());
        let debug = format!("{credential:?}");
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("refresh-secret"));
        assert!(!debug.contains("account-123"));
        assert!(!debug.contains("user@example.com"));
    }

    #[tokio::test]
    async fn credential_store_keeps_oauth_values_and_returns_only_account_status() {
        let memory = MemoryCredentials::default();
        let manager = CredentialManager::new(memory.clone()).unwrap();
        let serialized = test_credential("access-secret", "refresh-secret", now_ms() + 3_600_000)
            .serialize()
            .unwrap();
        let status = manager.save_chatgpt_credential(&serialized).await.unwrap();
        let visible = serde_json::to_string(&status).unwrap();

        assert!(visible.contains("user@example.com"));
        assert!(!visible.contains("access-secret"));
        assert!(!visible.contains("refresh-secret"));
        assert_eq!(
            memory.0.lock().unwrap().as_deref(),
            Some(serialized.as_str())
        );
    }

    #[tokio::test]
    async fn authorization_code_exchange_requires_refresh_token_and_extracts_account() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": jwt(json!({"https://api.openai.com/auth":{"chatgpt_account_id":"account-123"}})),
                "refresh_token": "refresh-secret",
                "expires_in": 3600,
                "id_token": jwt(json!({"email":"user@example.com"}))
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let credential = exchange_authorization_code(
            &http,
            &Url::parse(&format!("{}/oauth/token", server.uri())).unwrap(),
            "authorization-code",
            &"v".repeat(64),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(credential.account_id, "account-123");
        assert_eq!(credential.email.as_deref(), Some("user@example.com"));

        Mock::given(method("POST"))
            .and(path("/oauth/no-refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": jwt(json!({"chatgpt_account_id":"id"})),
                "expires_in": 3600
            })))
            .mount(&server)
            .await;
        assert!(
            exchange_authorization_code(
                &http,
                &Url::parse(&format!("{}/oauth/no-refresh", server.uri())).unwrap(),
                "code",
                &"v".repeat(64),
                &CancellationToken::new(),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn expired_credential_refreshes_and_preserves_missing_rotated_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": jwt(json!({"chatgpt_account_id":"account-123"})),
                "expires_in": 3600
            })))
            .mount(&server)
            .await;
        let (provider, memory, _) = provider_for(
            &server,
            test_credential(
                "old-access",
                "old-refresh",
                now_ms() + REFRESH_SKEW.as_millis() as u64 - 1,
            ),
        )
        .await;
        let refreshed = provider
            .credential_for_request(&CancellationToken::new())
            .await
            .unwrap();
        assert_ne!(refreshed.access, "old-access");
        let saved = memory.0.lock().unwrap().clone().unwrap();
        let saved = parse_credential(&saved).unwrap();
        assert_eq!(saved.refresh, "old-refresh");
        assert_eq!(saved.account_id, "account-123");
    }

    #[tokio::test]
    async fn logout_during_refresh_does_not_restore_chatgpt_credential() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(500))
                    .set_body_json(json!({
                        "access_token": "new-access",
                        "refresh_token": "new-refresh",
                        "expires_in": 3600
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let (provider, memory, credentials) = provider_for(
            &server,
            test_credential("old-access", "old-refresh", now_ms()),
        )
        .await;
        let refresh = tokio::spawn(async move {
            provider
                .credential_for_request(&CancellationToken::new())
                .await
        });

        timeout(Duration::from_secs(2), async {
            loop {
                if server.received_requests().await.unwrap().len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("refresh request should reach the mock server");

        credentials
            .remove_api_key(ProviderId::ChatGpt)
            .await
            .unwrap();
        assert!(refresh.await.unwrap().is_err());
        assert!(memory.0.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn provider_with_credential_loaded_before_logout_cannot_restore_it_afterward() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "expires_in": 3600
            })))
            .expect(0)
            .mount(&server)
            .await;

        let memory = MemoryCredentials::default();
        let credentials = CredentialManager::new(memory.clone()).unwrap();
        let serialized = test_credential("old-access", "old-refresh", now_ms())
            .serialize()
            .unwrap();
        credentials
            .save_chatgpt_credential(&serialized)
            .await
            .unwrap();
        credentials
            .remove_api_key(ProviderId::ChatGpt)
            .await
            .unwrap();

        let responses =
            Url::parse(&format!("{}/backend-api/codex/responses", server.uri())).unwrap();
        let token = Url::parse(&format!("{}/oauth/token", server.uri())).unwrap();
        let provider = ChatGptProvider::with_endpoints(
            &serialized,
            "gpt-6-luna".into(),
            credentials,
            responses,
            token,
        )
        .unwrap();
        let result = provider
            .credential_for_request(&CancellationToken::new())
            .await;

        assert!(matches!(
            result,
            Err(AiError::ChatGptReauthenticationRequired)
        ));
        assert!(memory.0.lock().unwrap().is_none());
        server.verify().await;
    }

    #[tokio::test]
    async fn account_replacement_during_refresh_preserves_winner_and_rejects_old_provider() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(200))
                    .set_body_json(
                        json!({"access_token":"refreshed-old-access", "expires_in":3600}),
                    ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (provider, memory, credentials) = provider_for(
            &server,
            test_credential("old-access", "old-refresh", now_ms()),
        )
        .await;
        let refresh = tokio::spawn(async move {
            provider
                .credential_for_request(&CancellationToken::new())
                .await
        });
        timeout(Duration::from_secs(2), async {
            while server.received_requests().await.unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("refresh should reach localhost");
        let mut replacement = test_credential("other-access", "other-refresh", now_ms() + 600_000);
        replacement.account_id = "other-account".into();
        let serialized = replacement.serialize().unwrap();
        credentials
            .save_chatgpt_credential(&serialized)
            .await
            .unwrap();
        assert!(matches!(
            refresh.await.unwrap(),
            Err(AiError::ChatGptReauthenticationRequired)
        ));
        assert_eq!(
            memory.0.lock().unwrap().as_deref(),
            Some(serialized.as_str())
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn valid_cached_token_cannot_send_a_later_batch_after_logout() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(header("Authorization", "Bearer old-access"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let (provider, _, credentials) = provider_for(
            &server,
            test_credential("old-access", "refresh", now_ms() + 600_000),
        )
        .await;
        provider
            .send_responses(json!({"input": []}), &CancellationToken::new())
            .await
            .unwrap();
        credentials
            .remove_api_key(ProviderId::ChatGpt)
            .await
            .unwrap();
        let result = provider
            .send_responses(json!({"input": []}), &CancellationToken::new())
            .await;
        assert!(matches!(
            result,
            Err(AiError::ChatGptReauthenticationRequired)
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        server.verify().await;
    }

    #[tokio::test]
    async fn replaced_account_cannot_send_with_valid_cached_token_or_refresh_after_401() {
        let server = MockServer::start().await;
        let (provider, _, credentials) = provider_for(
            &server,
            test_credential("old-access", "refresh", now_ms() + 600_000),
        )
        .await;
        let mut replacement = test_credential("other-access", "other-refresh", now_ms() + 600_000);
        replacement.account_id = "other-account".into();
        credentials
            .save_chatgpt_credential(&replacement.serialize().unwrap())
            .await
            .unwrap();
        assert!(matches!(
            provider
                .send_responses(json!({}), &CancellationToken::new())
                .await,
            Err(AiError::ChatGptReauthenticationRequired)
        ));
        assert!(matches!(
            provider
                .refresh_after_unauthorized("old-access", &CancellationToken::new())
                .await,
            Err(AiError::ChatGptReauthenticationRequired)
        ));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn same_account_updated_token_is_used_by_later_batch() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(header("Authorization", "Bearer new-access"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let (provider, _, credentials) = provider_for(
            &server,
            test_credential("old-access", "refresh", now_ms() + 600_000),
        )
        .await;
        credentials
            .save_chatgpt_credential(
                &test_credential("new-access", "new-refresh", now_ms() + 600_000)
                    .serialize()
                    .unwrap(),
            )
            .await
            .unwrap();
        provider
            .send_responses(json!({}), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        server.verify().await;
    }

    #[tokio::test]
    async fn concurrent_expired_requests_refresh_only_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(50))
                    .set_body_json(json!({"access_token":"new-access", "expires_in":3600})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (provider, _, _) =
            provider_for(&server, test_credential("old-access", "refresh", now_ms())).await;
        let cancellation = CancellationToken::new();
        let (first, second) = tokio::join!(
            provider.credential_for_request(&cancellation),
            provider.credential_for_request(&cancellation)
        );
        assert_eq!(first.unwrap().access, "new-access");
        assert_eq!(second.unwrap().access, "new-access");
        server.verify().await;
    }

    #[tokio::test]
    async fn cancelled_request_does_not_wait_for_credential_lock_or_send() {
        let server = MockServer::start().await;
        let (provider, _, _) = provider_for(
            &server,
            test_credential("access", "refresh", now_ms() + 600_000),
        )
        .await;
        let _lock = provider.credential.lock().await;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = timeout(
            Duration::from_millis(100),
            provider.send_responses(json!({}), &cancellation),
        )
        .await
        .expect("cancellation must interrupt lock acquisition");
        assert!(matches!(result, Err(AiError::Cancelled)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unauthorized_responses_refresh_once_and_retry_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(header("Authorization", "Bearer old-access"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(header("Authorization", "Bearer new-access"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let (provider, _, _) = provider_for(
            &server,
            test_credential("old-access", "old-refresh", now_ms() + 600_000),
        )
        .await;
        let result = provider
            .send_responses(json!({"input": []}), &CancellationToken::new())
            .await;
        assert!(matches!(
            result,
            Err(AiError::ChatGptReauthenticationRequired)
        ));
        server.verify().await;
    }

    #[tokio::test]
    async fn translates_sse_by_entry_id_and_falls_back_when_schema_is_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(body_string_contains("json_schema"))
            .and(body_string_contains("\"stream\":true"))
            .and(body_string_contains("\"store\":false"))
            .and(body_string_contains("reasoning.encrypted_content"))
            .and(header("ChatGPT-Account-Id", "account-123"))
            .and(header("originator", "yakucraft"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({"error":{"message":"json_schema is unsupported"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(body_string_contains("Return exactly one JSON object"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"{\\\"translations\\\":[{\\\"id\\\":19,\\\"translation\\\":\\\"こんにちは\\\"}]}\"}\n\ndata: {\"type\":\"response.completed\"}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let (provider, _, _) = provider_for(
            &server,
            test_credential("access", "refresh", now_ms() + 600_000),
        )
        .await;
        assert!(!provider.capabilities().requires_api_key);
        let translations = provider
            .translate_batch(
                &[TranslationEntry::new(
                    19,
                    "item.example",
                    "Example",
                    "en_us",
                    "ja_jp",
                )],
                &TranslationContext {
                    mod_name: "Example".into(),
                    namespace: "example".into(),
                    custom_prompt: None,
                    glossary_terms: Vec::new(),
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            translations,
            [AiTranslation {
                id: 19,
                translation: "こんにちは".into()
            }]
        );
        server.verify().await;
    }
}
