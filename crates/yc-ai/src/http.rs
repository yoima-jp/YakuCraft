use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, SystemTime},
};

use reqwest::{Client, RequestBuilder, Response, StatusCode, header};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{AiError, Result, url_policy::validate_provider_url};

const MAX_RETRIES: usize = 3;

pub(crate) fn validate_auth_header(value: &str) -> Result<()> {
    reqwest::header::HeaderValue::from_str(value)
        .map(|_| ())
        .map_err(|_| AiError::InvalidConfiguration)
}

pub(crate) struct ProviderHttp {
    client: Client,
    provider: &'static str,
    fixed_endpoint: bool,
}

impl ProviderHttp {
    pub(crate) fn new(provider: &'static str) -> Result<Self> {
        Self::build(provider, false)
    }

    /// 固定endpointのProviderはredirect先を許可せず、記載されたHTTPS hostだけへ接続する。
    pub(crate) fn new_fixed(provider: &'static str) -> Result<Self> {
        Self::build(provider, true)
    }

    fn build(provider: &'static str, fixed_endpoint: bool) -> Result<Self> {
        let builder = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            // Loopback HTTP は proxy 経由で外へ出さず、localhost の再解決も loopback に固定する。
            .no_proxy()
            .resolve_to_addrs(
                "localhost",
                &[
                    SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
                    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0),
                ],
            )
            .redirect(if fixed_endpoint {
                reqwest::redirect::Policy::none()
            } else {
                reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= 10 {
                        return attempt.error("redirect limit exceeded");
                    }
                    if validate_provider_url(attempt.url()).is_ok() {
                        attempt.follow()
                    } else {
                        attempt.stop()
                    }
                })
            });
        let client = builder.build().map_err(|_| AiError::Network)?;
        Ok(Self {
            client,
            provider,
            fixed_endpoint,
        })
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) async fn send_with_retry<F>(
        &self,
        mut make_request: F,
        cancellation: &CancellationToken,
    ) -> Result<Response>
    where
        F: FnMut() -> RequestBuilder,
    {
        for attempt in 0..=MAX_RETRIES {
            if cancellation.is_cancelled() {
                return Err(AiError::Cancelled);
            }

            let response = tokio::select! {
                _ = cancellation.cancelled() => return Err(AiError::Cancelled),
                response = make_request().send() => response,
            };

            let response = match response {
                Ok(response) => response,
                Err(_) if attempt < MAX_RETRIES => {
                    // reqwest のエラー表示には URL が含まれ得るため、内容は捨てて Provider と試行回数だけ記録する。
                    tracing::debug!(
                        provider = self.provider,
                        attempt,
                        "provider request failed; retrying"
                    );
                    wait_for_retry(backoff(attempt), cancellation).await?;
                    continue;
                }
                Err(_) => {
                    tracing::debug!(
                        provider = self.provider,
                        "provider request failed after retries"
                    );
                    return Err(AiError::Network);
                }
            };

            let status = response.status();
            if status == StatusCode::UNAUTHORIZED
                || status == StatusCode::FORBIDDEN && !self.fixed_endpoint
            {
                tracing::debug!(
                    provider = self.provider,
                    status = status.as_u16(),
                    "provider authentication failed"
                );
                return Err(AiError::ProviderAuth);
            }
            if status == StatusCode::FORBIDDEN && self.fixed_endpoint {
                // ChatGPTでは401だけtoken refreshの対象とし、403は権限エラーとして返す。
                return Ok(response);
            }

            if is_retryable_status(status) {
                let retry_after = response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| parse_retry_after(value.as_bytes()));
                if attempt < MAX_RETRIES {
                    tracing::debug!(
                        provider = self.provider,
                        attempt,
                        status = status.as_u16(),
                        "provider requested retry"
                    );
                    wait_for_retry(
                        retry_after.unwrap_or_else(|| backoff(attempt)),
                        cancellation,
                    )
                    .await?;
                    continue;
                }
                if status == StatusCode::TOO_MANY_REQUESTS {
                    return Err(AiError::ProviderRateLimit { retry_after });
                }
                return Err(AiError::ProviderResponse {
                    status: status.as_u16(),
                });
            }

            return Ok(response);
        }

        Err(AiError::Network)
    }

    pub(crate) fn endpoint(&self, base: &Url, path: &str) -> Result<Url> {
        validate_provider_url(base).map_err(|_| AiError::InvalidUrl)?;
        let mut base = base.clone();
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let endpoint = base.join(path).map_err(|_| AiError::InvalidUrl)?;
        validate_provider_url(&endpoint).map_err(|_| AiError::InvalidUrl)?;
        Ok(endpoint)
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || matches!(status.as_u16(), 500 | 502 | 503 | 504)
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_secs(1_u64 << attempt)
}

fn parse_retry_after(value: &[u8]) -> Option<Duration> {
    let value = std::str::from_utf8(value).ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    retry_at.duration_since(SystemTime::now()).ok()
}

async fn wait_for_retry(delay: Duration, cancellation: &CancellationToken) -> Result<()> {
    tokio::select! {
        _ = cancellation.cancelled() => Err(AiError::Cancelled),
        _ = tokio::time::sleep(delay) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{backoff, parse_retry_after};
    use std::time::{Duration, SystemTime};

    #[test]
    fn retry_after_accepts_delta_seconds_and_http_dates() {
        assert_eq!(parse_retry_after(b"3"), Some(Duration::from_secs(3)));
        let retry_at = SystemTime::now() + Duration::from_secs(60);
        let retry_after = httpdate::fmt_http_date(retry_at);
        let parsed = parse_retry_after(retry_after.as_bytes()).unwrap();
        assert!((59..=60).contains(&parsed.as_secs()));
        assert!(parse_retry_after(b"not a date").is_none());
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(4));
    }
}
