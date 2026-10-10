use std::net::{IpAddr, ToSocketAddrs};

use thiserror::Error;
use url::{Host, Url};

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("URL scheme or host is not allowed")]
pub enum UrlPolicyError {
    DisallowedScheme,
    DisallowedHost,
    LocalhostResolution,
    CredentialsOrQuery,
}

/// API key を含む URL や、LAN・外部への平文 HTTP を送信先にしない。
pub fn validate_provider_url(url: &Url) -> std::result::Result<(), UrlPolicyError> {
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        return Err(UrlPolicyError::CredentialsOrQuery);
    }
    if url.fragment().is_some() {
        return Err(UrlPolicyError::CredentialsOrQuery);
    }

    match url.scheme() {
        "https" => Ok(()),
        "http" => validate_loopback_http(url),
        _ => Err(UrlPolicyError::DisallowedScheme),
    }
}

fn validate_loopback_http(url: &Url) -> std::result::Result<(), UrlPolicyError> {
    match url.host() {
        Some(Host::Ipv4(address)) if address.is_loopback() => Ok(()),
        Some(Host::Ipv6(address)) if address.is_loopback() => Ok(()),
        Some(Host::Domain(host)) if host.eq_ignore_ascii_case("localhost") => {
            let port = url
                .port_or_known_default()
                .ok_or(UrlPolicyError::DisallowedHost)?;
            let addresses = (host, port)
                .to_socket_addrs()
                .map_err(|_| UrlPolicyError::LocalhostResolution)?
                .map(|address| address.ip())
                .collect::<Vec<IpAddr>>();
            if addresses.is_empty() || addresses.iter().any(|address| !address.is_loopback()) {
                return Err(UrlPolicyError::LocalhostResolution);
            }
            Ok(())
        }
        _ => Err(UrlPolicyError::DisallowedHost),
    }
}
