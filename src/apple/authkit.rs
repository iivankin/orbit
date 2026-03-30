use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use p256::SecretKey;
use p256::elliptic_curve::rand_core::OsRng;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use reqwest::blocking::{Client, ClientBuilder, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;

use crate::apple::grand_slam::XcodeNotaryAuth;

pub(crate) const APP_STORE_CONNECT_AUTHKIT_URL: &str =
    "https://appstoreconnect.apple.com/ci/auth/auth/authkit";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum AuthKitIdentity {
    Xcode,
    Orbit,
}

struct AuthKitBootstrapRequest {
    headers: HeaderMap,
    body: JsonValue,
}

pub(crate) fn build_cookie_client(context_label: &str) -> Result<Client> {
    ClientBuilder::new()
        .brotli(true)
        .cookie_store(true)
        .deflate(true)
        .gzip(true)
        .build()
        .with_context(|| format!("failed to build {context_label} HTTP client"))
}

pub(crate) fn header_map(headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(map)
}

pub(crate) fn bootstrap_authkit<T>(
    client: &Client,
    auth: &XcodeNotaryAuth,
    identity: AuthKitIdentity,
    context_label: &str,
) -> Result<T>
where
    T: DeserializeOwned,
{
    let response = send_authkit_bootstrap_request(client, auth, identity, context_label)?;
    parse_json_response(response, &format!("{context_label} authkit bootstrap"))
}

pub(crate) fn send_authkit_bootstrap_request(
    client: &Client,
    auth: &XcodeNotaryAuth,
    identity: AuthKitIdentity,
    context_label: &str,
) -> Result<Response> {
    let authkit_url = authkit_bootstrap_url();
    let request = authkit_bootstrap_request(auth, identity)?;
    let mut last_retryable_error = None;
    for attempt in 0..4 {
        match client
            .post(&authkit_url)
            .headers(request.headers.clone())
            .json(&request.body)
            .send()
        {
            Ok(response) => return Ok(response),
            Err(error) if should_retry_transport_error(&error) && attempt < 3 => {
                last_retryable_error = Some(error);
                thread::sleep(Duration::from_millis(250 * (attempt + 1) as u64));
            }
            Err(error) if should_retry_transport_error(&error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to execute {context_label} authkit bootstrap request after retry"
                    )
                });
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to execute {context_label} authkit bootstrap request")
                });
            }
        }
    }
    Err(last_retryable_error.expect("retry loop should capture error")).with_context(|| {
        format!("failed to execute {context_label} authkit bootstrap request after retry")
    })
}

fn authkit_bootstrap_request(
    auth: &XcodeNotaryAuth,
    identity: AuthKitIdentity,
) -> Result<AuthKitBootstrapRequest> {
    let mut headers = auth.authkit_headers();
    let mut body = auth.authkit_request_body(&generate_client_public_key()?);
    apply_identity_overrides(&mut headers, &mut body, identity);
    Ok(AuthKitBootstrapRequest {
        headers: header_map(&headers)?,
        body,
    })
}

pub(crate) fn parse_json_response<T>(response: Response, context_label: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let body = response
        .bytes()
        .with_context(|| format!("failed to read `{context_label}` response body"))?;
    if !status.is_success() {
        bail!(
            "{context_label} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body)
        .with_context(|| format!("failed to parse `{context_label}` response body"))
}

fn generate_client_public_key() -> Result<String> {
    let secret_key = SecretKey::random(&mut OsRng);
    let public_key = secret_key.public_key();
    let encoded = public_key.to_encoded_point(false);
    let coordinates = encoded
        .as_bytes()
        .get(1..)
        .context("P-256 public key is missing uncompressed coordinates")?;
    Ok(STANDARD.encode(coordinates))
}

fn apply_identity_overrides(
    headers: &mut BTreeMap<String, String>,
    body: &mut JsonValue,
    identity: AuthKitIdentity,
) {
    if identity != AuthKitIdentity::Orbit {
        return;
    }

    let orbit_user_agent = format!("Orbit/{}", env!("CARGO_PKG_VERSION"));
    let orbit_app_info = format!("dev.orbit.cli/{}", env!("CARGO_PKG_VERSION"));
    let orbit_client_info = headers
        .get("x-mme-client-info")
        .map(|value| orbit_client_info(value))
        .unwrap_or_else(default_orbit_client_info);

    headers.insert("user-agent".to_owned(), orbit_user_agent);
    headers.insert("x-apple-app-info".to_owned(), orbit_app_info.clone());
    headers.insert("x-mme-client-info".to_owned(), orbit_client_info.clone());

    if let Some(object) = body.as_object_mut() {
        object.insert(
            "x_mme_client_Info".to_owned(),
            JsonValue::String(orbit_client_info),
        );
    }
}

fn should_retry_transport_error(error: &reqwest::Error) -> bool {
    error.is_connect()
        || error.is_timeout()
        || error
            .to_string()
            .to_ascii_lowercase()
            .contains("tls handshake eof")
}

fn authkit_bootstrap_url() -> String {
    if let Ok(value) = std::env::var("ORBIT_AUTHKIT_BASE_URL") {
        return value;
    }
    if let Ok(value) = std::env::var("ORBIT_NOTARY_BASE_URL") {
        return format!("{}/ci/auth/auth/authkit", value.trim_end_matches('/'));
    }
    APP_STORE_CONNECT_AUTHKIT_URL.to_owned()
}

fn orbit_client_info(existing_client_info: &str) -> String {
    let mut parts = existing_client_info
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .split("> <")
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if parts.len() >= 3 {
        parts[2] = format!(
            "dev.orbit.cli/{} (orbit/{})",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION")
        );
        return format!("<{}>", parts.join("> <"));
    }
    default_orbit_client_info()
}

fn default_orbit_client_info() -> String {
    format!(
        "<UnknownMac> <macOS;unknown;unknown> <dev.orbit.cli/{} (orbit/{})>",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_VERSION")
    )
}
