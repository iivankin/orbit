use std::collections::BTreeMap;
use std::io::Cursor;

use aes::Aes256;
use aes::cipher::consts::U16;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::{AesGcm, KeyInit, Nonce};
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::Mac;
use plist::{Dictionary, Value as PlistValue};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use time::OffsetDateTime;

use super::{
    AUTHKIT_APP_INFO, ClientProfile, GRAND_SLAM_SERVICE_URL, GrandSlamAppToken,
    GrandSlamAuthMaterial, TRANSPORTER_APP_BUNDLE_ID, TRANSPORTER_APP_NAME,
    TRANSPORTER_APP_VERSION, TRANSPORTER_FRAMEWORK_FOUNDATION, TRANSPORTER_FRAMEWORK_PACKAGE,
    XcodeMetadata, now_rfc3339,
};
use crate::util::{prompt_confirm, prompt_input};

#[derive(Debug)]
pub(super) struct HttpDebugResponse {
    pub(super) headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(super) struct ContentDeliveryHeaders {
    pub(super) headers: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcEnvelope<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct AuthenticateUserResult {
    #[serde(rename = "ProvidersByShortname", default)]
    pub(super) providers_by_shortname: BTreeMap<String, ProviderInfo>,
    #[serde(rename = "Success", default)]
    pub(super) success: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ProviderInfo {
    #[serde(rename = "ProviderName")]
    pub(super) provider_name: String,
    #[serde(rename = "ProviderPublicId")]
    pub(super) provider_public_id: String,
    #[serde(rename = "WWDRTeamID", default)]
    pub(super) team_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProvidersInfoResult {
    #[serde(rename = "ProvidersInfo", default)]
    pub(super) providers: BTreeMap<String, ProviderInfoEntry>,
    #[serde(rename = "Success", default)]
    pub(super) success: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProviderInfoEntry {
    #[serde(rename = "ProviderShortname")]
    pub(super) provider_name: String,
    #[serde(rename = "PublicID")]
    pub(super) provider_public_id: String,
    #[serde(rename = "WWDRTeamID", default)]
    pub(super) team_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AuthenticateForSessionResult {
    #[serde(rename = "SessionId", default)]
    pub(super) session_id: Option<String>,
    #[serde(rename = "SharedSecret", default)]
    pub(super) shared_secret: Option<String>,
    #[serde(rename = "Success", default)]
    pub(super) success: bool,
}

#[derive(Debug, Clone)]
pub(super) struct SelectedProvider {
    pub(super) provider_name: String,
    pub(super) provider_public_id: String,
}

#[derive(Debug)]
pub(super) struct JsonRpcCall<T> {
    pub(super) result: T,
    pub(super) raw_response: HttpDebugResponse,
}

pub(super) fn request_app_token_with_interactive_verification(
    client: &Client,
    profile: &ClientProfile,
    material: &GrandSlamAuthMaterial,
    service_id: &str,
    interactive: bool,
) -> Result<GrandSlamAppToken> {
    match request_app_token(client, profile, material, service_id) {
        Ok(token) => Ok(token),
        Err(error) if interactive => {
            if !prompt_confirm(
                "Apple may require an additional verification code for this account. Try trusted-device verification now?",
                true,
            )? {
                return Err(error);
            }
            complete_trusted_device_verification(client, profile, material)?;
            request_app_token(client, profile, material, service_id)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn trusted_factor_headers(
    profile: &ClientProfile,
    material: &GrandSlamAuthMaterial,
    xcode: &XcodeMetadata,
    code: Option<&str>,
) -> Result<BTreeMap<String, String>> {
    let identity_token = STANDARD.encode(format!(
        "{}:{}",
        material.ds_person_id, material.gs_idms_token
    ));
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_owned(), "text/x-xml-plist".to_owned());
    headers.insert("accept".to_owned(), "text/x-xml-plist".to_owned());
    headers.insert("accept-language".to_owned(), "en-us".to_owned());
    headers.insert("user-agent".to_owned(), "Xcode".to_owned());
    headers.insert("x-apple-identity-token".to_owned(), identity_token);
    headers.insert("x-apple-app-info".to_owned(), AUTHKIT_APP_INFO.to_owned());
    headers.insert("x-xcode-version".to_owned(), xcode.version_header());
    headers.insert("x-mme-client-info".to_owned(), xcode.authkit_client_info());
    headers.insert("x-apple-i-client-time".to_owned(), now_rfc3339());
    headers.insert("x-apple-i-timezone".to_owned(), profile.time_zone.clone());
    headers.insert("x-apple-locale".to_owned(), profile.locale.clone());
    headers.insert("loc".to_owned(), profile.locale.clone());
    headers.insert("x-apple-i-md-rinfo".to_owned(), profile.md_rinfo.clone());
    headers.insert(
        "x-apple-i-md-lu".to_owned(),
        STANDARD.encode(profile.logical_user_id.as_bytes()),
    );
    headers.insert("x-mme-device-id".to_owned(), profile.device_id.clone());
    headers.insert("x-apple-i-srl-no".to_owned(), profile.serial_number.clone());
    if let Some(md) = profile.md.as_deref() {
        headers.insert("x-apple-i-md".to_owned(), md.to_owned());
    }
    if let Some(md_m) = profile.md_m.as_deref() {
        headers.insert("x-apple-i-md-m".to_owned(), md_m.to_owned());
    }
    if let Some(code) = code {
        headers.insert("security-code".to_owned(), code.to_owned());
    }
    Ok(headers)
}

pub(super) fn build_client() -> Result<Client> {
    ClientBuilder::new()
        .brotli(true)
        .gzip(true)
        .deflate(true)
        .build()
        .context("failed to build GrandSlam debug HTTP client")
}

pub(super) fn execute_json_rpc<T: for<'de> Deserialize<'de>>(
    client: &Client,
    service_class: &str,
    base_headers: &BTreeMap<String, String>,
    method_name: &str,
    params: JsonValue,
) -> Result<JsonRpcCall<T>> {
    let request_id = content_delivery_request_id()?;
    let mut headers = base_headers.clone();
    set_header_value(&mut headers, "x-request-id", &request_id);
    set_header_value(&mut headers, "x-tx-method", method_name);
    set_header_value(&mut headers, "x-tx-client-name", "Transporter");
    set_header_value(&mut headers, "x-tx-client-version", TRANSPORTER_APP_VERSION);
    set_header_value(&mut headers, "x-apple-i-client-time", &now_rfc3339());

    let body = serde_json::to_vec(&json!({
        "id": request_id,
        "jsonrpc": "2.0",
        "method": method_name,
        "params": params,
    }))
    .context("failed to serialize content-delivery JSON-RPC body")?;
    let response = execute_plistless_json_post(client, service_class, &headers, &body)?;
    let envelope: JsonRpcEnvelope<T> = serde_json::from_slice(&response.body)
        .with_context(|| format!("failed to decode {service_class} {method_name} response"))?;
    if let Some(error) = envelope.error {
        bail!(
            "{} {} failed with {}: {}",
            service_class,
            method_name,
            error.code,
            error.message
        );
    }
    let result = envelope
        .result
        .with_context(|| format!("{service_class} {method_name} did not return a result"))?;
    Ok(JsonRpcCall {
        result,
        raw_response: response,
    })
}

pub(super) fn content_delivery_auth_params(
    profile: &ClientProfile,
    provider_name: Option<&str>,
) -> Result<JsonValue> {
    let mut params = JsonMap::new();
    params.insert(
        "Application".to_owned(),
        JsonValue::String(TRANSPORTER_APP_NAME.to_owned()),
    );
    params.insert(
        "Version".to_owned(),
        JsonValue::String(TRANSPORTER_APP_VERSION.to_owned()),
    );
    params.insert(
        "ApplicationBundleId".to_owned(),
        JsonValue::String(TRANSPORTER_APP_BUNDLE_ID.to_owned()),
    );
    params.insert(
        "OSIdentifier".to_owned(),
        JsonValue::String(transporter_os_identifier()?),
    );
    params.insert(
        "FrameworkVersions".to_owned(),
        json!({
            "com.apple.itunes.connect.ITunesConnectFoundation": TRANSPORTER_FRAMEWORK_FOUNDATION,
            "com.apple.itunes.connect.ITunesPackage": TRANSPORTER_FRAMEWORK_PACKAGE,
        }),
    );
    if let Some(provider_name) = provider_name {
        params.insert(
            "ProviderName".to_owned(),
            JsonValue::String(provider_name.to_owned()),
        );
    }
    if profile.service == "iTunes" {
        return Ok(JsonValue::Object(params));
    }
    Ok(JsonValue::Object(params))
}

pub(super) fn select_provider_metadata(
    explicit_team_id: Option<&str>,
    providers_by_shortname: &BTreeMap<String, ProviderInfo>,
    providers_info: &BTreeMap<String, ProviderInfoEntry>,
) -> Result<SelectedProvider> {
    if let Some(team_id) = explicit_team_id
        && let Some(provider) = providers_info
            .values()
            .find(|provider| provider.team_id.as_deref() == Some(team_id))
            .map(|provider| SelectedProvider {
                provider_name: provider.provider_name.clone(),
                provider_public_id: provider.provider_public_id.clone(),
            })
            .or_else(|| {
                providers_by_shortname
                    .values()
                    .find(|provider| provider.team_id.as_deref() == Some(team_id))
                    .map(|provider| SelectedProvider {
                        provider_name: provider.provider_name.clone(),
                        provider_public_id: provider.provider_public_id.clone(),
                    })
            })
    {
        return Ok(provider);
    }

    if providers_info.len() == 1
        && let Some(provider) = providers_info.values().next()
    {
        return Ok(SelectedProvider {
            provider_name: provider.provider_name.clone(),
            provider_public_id: provider.provider_public_id.clone(),
        });
    }
    if providers_by_shortname.len() == 1
        && let Some(provider) = providers_by_shortname.values().next()
    {
        return Ok(SelectedProvider {
            provider_name: provider.provider_name.clone(),
            provider_public_id: provider.provider_public_id.clone(),
        });
    }

    let available = providers_info
        .values()
        .map(|provider| {
            format!(
                "{} (team_id={}, public_id={})",
                provider.provider_name,
                provider.team_id.as_deref().unwrap_or("<unknown>"),
                provider.provider_public_id
            )
        })
        .chain(providers_by_shortname.values().map(|provider| {
            format!(
                "{} (team_id={}, public_id={})",
                provider.provider_name,
                provider.team_id.as_deref().unwrap_or("<unknown>"),
                provider.provider_public_id
            )
        }))
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "multiple providers available; set team_id in orbit.json or ORBIT_APPLE_TEAM_ID. available: {available}"
    )
}

pub(super) fn request_headers(profile: &ClientProfile) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/x-xml-plist"));
    headers.insert(USER_AGENT, HeaderValue::from_str(&profile.user_agent)?);
    headers.insert(
        HeaderName::from_static("x-mme-client-info"),
        HeaderValue::from_str(&profile.client_info)?,
    );
    headers.insert(
        HeaderName::from_static("accept-language"),
        HeaderValue::from_str(&profile.accept_language)?,
    );
    Ok(headers)
}

pub(super) fn execute_plist_post(
    client: &Client,
    url: &str,
    headers: HeaderMap,
    body: &[u8],
) -> Result<HttpDebugResponse> {
    let response = client
        .post(url)
        .headers(headers)
        .body(body.to_vec())
        .send()
        .with_context(|| format!("failed to execute POST {url}"))?;
    HttpDebugResponse::from_response(response)
}

pub(super) fn plist_body(request: Dictionary) -> Vec<u8> {
    let mut root = Dictionary::new();
    root.insert(
        "Header".to_owned(),
        PlistValue::Dictionary(Dictionary::from_iter([(
            "Version".to_owned(),
            PlistValue::String("1.0.1".to_owned()),
        )])),
    );
    root.insert("Request".to_owned(), PlistValue::Dictionary(request));
    let value = PlistValue::Dictionary(root);
    let mut body = Vec::new();
    value
        .to_writer_xml(&mut body)
        .expect("GrandSlam plist payload serialization must succeed");
    body
}

impl ContentDeliveryHeaders {
    pub(super) fn new(
        profile: &ClientProfile,
        material: &GrandSlamAuthMaterial,
        identity_value: &str,
        token_value: &str,
    ) -> Result<Self> {
        let mut headers = BTreeMap::new();
        headers.insert("accept".to_owned(), "*/*".to_owned());
        headers.insert("content-type".to_owned(), "application/json".to_owned());
        headers.insert("user-agent".to_owned(), transporter_user_agent(profile));
        headers.insert(
            "accept-language".to_owned(),
            profile.accept_language.clone(),
        );
        headers.insert("x-apple-gs-token".to_owned(), token_value.to_owned());
        headers.insert(
            "x-apple-i-identity-id".to_owned(),
            identity_value.to_owned(),
        );
        headers.insert(
            "x-apple-app-info".to_owned(),
            "com.apple.gs.itunesconnect.auth".to_owned(),
        );
        headers.insert("x-mme-device-id".to_owned(), profile.device_id.clone());
        headers.insert("x-mme-client-info".to_owned(), profile.client_info.clone());
        headers.insert("x-apple-i-timezone".to_owned(), profile.time_zone.clone());
        headers.insert("x-apple-i-client-time".to_owned(), now_rfc3339());
        headers.insert("x-apple-i-md-rinfo".to_owned(), profile.md_rinfo.clone());
        headers.insert("x-apple-i-locale".to_owned(), profile.locale.clone());
        headers.insert(
            "x-apple-i-md-lu".to_owned(),
            profile
                .cpd()
                .get("X-Apple-I-MD-LU")
                .and_then(PlistValue::as_string)
                .context("CPD is missing X-Apple-I-MD-LU")?
                .to_owned(),
        );
        if let Some(md_m) = profile.md_m.as_deref() {
            headers.insert("x-apple-i-md-m".to_owned(), md_m.to_owned());
        }
        if let Some(md) = profile.md.as_deref() {
            headers.insert("x-apple-i-md".to_owned(), md.to_owned());
        }
        if let Some(token) = material.service_tokens.get("com.apple.gs.idms.pet") {
            headers.insert("x-apple-idms-pet".to_owned(), token.to_owned());
        }
        Ok(Self { headers })
    }

    pub(super) fn merged(&self, response_headers: &BTreeMap<String, String>) -> Self {
        const AUTH_NAMES: &[&str] = &[
            "x-apple-gs-token",
            "x-apple-i-identity-id",
            "x-apple-app-info",
            "x-mme-device-id",
            "x-mme-client-info",
            "x-apple-i-timezone",
            "x-apple-i-client-time",
            "x-apple-i-md-rinfo",
            "x-apple-i-md-m",
            "x-apple-i-locale",
            "x-apple-i-md-lu",
            "x-apple-i-md",
        ];

        let mut headers = self.headers.clone();
        for name in AUTH_NAMES {
            if let Some(value) = header_value(response_headers, name) {
                set_header_value(&mut headers, name, value);
            }
        }
        set_header_value(&mut headers, "x-apple-i-client-time", &now_rfc3339());
        Self { headers }
    }
}

impl HttpDebugResponse {
    fn from_response(response: reqwest::blocking::Response) -> Result<Self> {
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or("<binary>").to_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let body = response.bytes()?.to_vec();
        Ok(Self { headers, body })
    }

    pub(super) fn plist(&self) -> Option<PlistValue> {
        PlistValue::from_reader(Cursor::new(&self.body)).ok()
    }
}

fn request_app_token(
    client: &Client,
    profile: &ClientProfile,
    material: &GrandSlamAuthMaterial,
    service_id: &str,
) -> Result<GrandSlamAppToken> {
    let checksum = <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(&material.session_key)
        .map_err(|error| anyhow::anyhow!("failed to initialize app token checksum HMAC: {error}"))?
        .chain_update(b"apptokens")
        .chain_update(material.adsid.as_bytes())
        .chain_update(service_id.as_bytes())
        .finalize()
        .into_bytes()
        .to_vec();

    let body = plist_body({
        let mut request = Dictionary::new();
        request.insert(
            "app".to_owned(),
            PlistValue::Array(vec![PlistValue::String(service_id.to_owned())]),
        );
        request.insert(
            "c".to_owned(),
            PlistValue::Data(material.continuation.clone()),
        );
        request.insert("checksum".to_owned(), PlistValue::Data(checksum));
        request.insert("cpd".to_owned(), PlistValue::Dictionary(profile.cpd()));
        request.insert("o".to_owned(), PlistValue::String("apptokens".to_owned()));
        request.insert("u".to_owned(), PlistValue::String(material.adsid.clone()));
        request.insert(
            "t".to_owned(),
            PlistValue::String(material.gs_idms_token.clone()),
        );
        request
    });

    let response = execute_plist_post(
        client,
        GRAND_SLAM_SERVICE_URL,
        request_headers(profile)?,
        &body,
    )?;
    let response_plist = response
        .plist()
        .context("GrandSlam app token request did not return a plist response")?;
    let response_dict = response_plist
        .as_dictionary()
        .context("GrandSlam app token response root is not a dictionary")?;
    let response_section = response_dict
        .get("Response")
        .and_then(PlistValue::as_dictionary)
        .context("GrandSlam app token response is missing `Response`")?;
    let response_body = printable_body(&response);

    if let Some(error) = response_section.get("ErrorCode").and_then(stringish) {
        bail!("GrandSlam app token request failed with ErrorCode={error}\n{response_body}");
    }

    let encrypted_token = response_data(response_section, "et").with_context(|| {
        format!("GrandSlam app token response did not include `et`\n{response_body}")
    })?;
    let decrypted_token = decrypt_app_token(&encrypted_token, &material.session_key)
        .context("failed to decrypt GrandSlam app token payload")?;
    let token_plist = PlistValue::from_reader(Cursor::new(&decrypted_token))
        .context("failed to parse decrypted GrandSlam app token plist")?;
    let token_dict = token_plist
        .as_dictionary()
        .context("decrypted GrandSlam app token root is not a dictionary")?;
    let status = response_unsigned(token_dict, "status-code")
        .context("decrypted GrandSlam app token is missing `status-code`")?;
    if status != 200 {
        bail!(
            "GrandSlam app token request returned status-code={status}\n{}",
            serde_json::to_string_pretty(&plist_to_json(&token_plist))
                .unwrap_or_else(|_| String::from_utf8_lossy(&decrypted_token).into_owned())
        );
    }

    let token_dictionary = token_dict
        .get("t")
        .and_then(PlistValue::as_dictionary)
        .context("decrypted GrandSlam app token is missing `t`")?;
    let app_token = token_dictionary
        .get(service_id)
        .and_then(PlistValue::as_dictionary)
        .with_context(|| {
            format!("decrypted GrandSlam app token is missing token entry for {service_id}")
        })?;

    Ok(GrandSlamAppToken {
        token: response_string(app_token, "token")
            .context("decrypted GrandSlam app token is missing `token`")?,
        expiry: response_unsigned(app_token, "expiry")
            .context("decrypted GrandSlam app token is missing `expiry`")?,
    })
}

fn complete_trusted_device_verification(
    client: &Client,
    profile: &ClientProfile,
    material: &GrandSlamAuthMaterial,
) -> Result<()> {
    let xcode = XcodeMetadata::detect()?;
    let trigger_headers = trusted_factor_headers(profile, material, &xcode, None)?;
    let trigger_response = client
        .get("https://gsa.apple.com/auth/verify/trusteddevice")
        .headers(to_header_map(&trigger_headers)?)
        .send()
        .context("failed to trigger Apple trusted-device verification prompt")?;
    if !trigger_response.status().is_success() {
        let status = trigger_response.status();
        let body = http_response_body(trigger_response);
        bail!(
            "failed to trigger Apple trusted-device verification ({}): {}",
            status,
            body
        );
    }

    loop {
        let code = prompt_input(
            "Enter the Apple verification code shown on your trusted device",
            None,
        )?;
        let verify_headers = trusted_factor_headers(profile, material, &xcode, Some(&code))?;
        let verify_response = client
            .get("https://gsa.apple.com/grandslam/GsService2/validate")
            .headers(to_header_map(&verify_headers)?)
            .send()
            .context("failed to submit Apple trusted-device verification code")?;
        if verify_response.status().is_success() {
            return Ok(());
        }

        let status = verify_response.status();
        let body = http_response_body(verify_response);
        if status.is_client_error() {
            println!("The Apple verification code was rejected. Try again.");
            continue;
        }
        bail!(
            "failed to verify Apple trusted-device code ({}): {}",
            status,
            body
        );
    }
}

fn http_response_body(response: reqwest::blocking::Response) -> String {
    match response.bytes() {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

fn decrypt_app_token(data: &[u8], session_key: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 3 + 16 + 16 {
        bail!(
            "encrypted app token is too short to be valid ({} bytes)",
            data.len()
        );
    }
    if &data[..3] != b"XYZ" {
        bail!(
            "encrypted app token uses unknown prefix `{}`",
            String::from_utf8_lossy(&data[..3])
        );
    }
    if session_key.len() != 32 {
        bail!(
            "GrandSlam session key is not 32 bytes (got {})",
            session_key.len()
        );
    }

    let iv = &data[3..19];
    let mut ciphertext = data[19..].to_vec();
    let cipher = AesGcm::<Aes256, U16>::new_from_slice(session_key)
        .context("failed to initialize app token AES-GCM cipher")?;
    let nonce = Nonce::<U16>::from_slice(iv);
    cipher
        .decrypt_in_place(nonce, b"XYZ", &mut ciphertext)
        .map_err(|error| anyhow::anyhow!("failed to decrypt app token AES-GCM payload: {error}"))?;
    Ok(ciphertext)
}

fn execute_plistless_json_post(
    client: &Client,
    service_class: &str,
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<HttpDebugResponse> {
    let response = client
        .post(format!(
            "https://contentdelivery.itunes.apple.com/WebObjects/MZLabelService.woa/json/{service_class}"
        ))
        .headers(to_header_map(headers)?)
        .body(body.to_vec())
        .send()
        .with_context(|| format!("failed to execute JSON POST for {service_class}"))?;
    HttpDebugResponse::from_response(response)
}

fn transporter_os_identifier() -> Result<String> {
    let version =
        crate::util::command_output(std::process::Command::new("sw_vers").arg("-productVersion"))
            .unwrap_or_else(|_| "26.0.0".to_owned());
    let arch = crate::util::command_output(std::process::Command::new("uname").arg("-m"))
        .unwrap_or_else(|_| std::env::consts::ARCH.to_owned());
    Ok(format!("Mac OS X {} ({})", version.trim(), arch.trim()))
}

fn transporter_user_agent(profile: &ClientProfile) -> String {
    if profile.user_agent.contains("Transporter") {
        return profile.user_agent.clone();
    }
    format!("Transporter/{TRANSPORTER_APP_VERSION}")
}

fn content_delivery_request_id() -> Result<String> {
    let format = time::format_description::parse(
        "[year][month][day][hour][minute][second]-[subsecond digits:3]",
    )
    .context("failed to build content-delivery request-id formatter")?;
    OffsetDateTime::now_utc()
        .format(&format)
        .context("failed to format content-delivery request id")
}

fn to_header_map(headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(map)
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn set_header_value(headers: &mut BTreeMap<String, String>, name: &str, value: &str) {
    if let Some(existing_name) = headers
        .keys()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.insert(existing_name, value.to_owned());
    } else {
        headers.insert(name.to_owned(), value.to_owned());
    }
}

pub(super) fn printable_body(response: &HttpDebugResponse) -> String {
    if let Some(plist) = response.plist() {
        serde_json::to_string_pretty(&plist_to_json(&plist))
            .unwrap_or_else(|_| String::from_utf8_lossy(&response.body).into_owned())
    } else if response.body.is_empty() {
        "<empty>".to_owned()
    } else {
        String::from_utf8_lossy(&response.body).into_owned()
    }
}

fn plist_to_json(value: &PlistValue) -> serde_json::Value {
    match value {
        PlistValue::Array(values) => {
            serde_json::Value::Array(values.iter().map(plist_to_json).collect())
        }
        PlistValue::Boolean(value) => serde_json::Value::Bool(*value),
        PlistValue::Data(bytes) => serde_json::Value::String(STANDARD.encode(bytes)),
        PlistValue::Date(date) => serde_json::Value::String(date.to_xml_format()),
        PlistValue::Dictionary(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), plist_to_json(value)))
                .collect(),
        ),
        PlistValue::Integer(value) => value
            .as_signed()
            .map(serde_json::Value::from)
            .or_else(|| value.as_unsigned().map(serde_json::Value::from))
            .unwrap_or(serde_json::Value::Null),
        PlistValue::Real(value) => serde_json::Value::from(*value),
        PlistValue::String(value) => serde_json::Value::String(value.clone()),
        PlistValue::Uid(value) => serde_json::Value::from(value.get()),
        _ => serde_json::Value::Null,
    }
}

pub(super) fn response_data(dictionary: &Dictionary, key: &str) -> Result<Vec<u8>> {
    dictionary
        .get(key)
        .and_then(|value| match value {
            PlistValue::Data(bytes) => Some(bytes.clone()),
            PlistValue::String(value) => STANDARD.decode(value).ok(),
            _ => None,
        })
        .with_context(|| format!("GrandSlam response is missing `{key}` data"))
}

pub(super) fn response_string(dictionary: &Dictionary, key: &str) -> Result<String> {
    dictionary
        .get(key)
        .and_then(stringish)
        .with_context(|| format!("GrandSlam response is missing `{key}` string"))
}

pub(super) fn response_unsigned(dictionary: &Dictionary, key: &str) -> Result<u64> {
    dictionary
        .get(key)
        .and_then(|value| match value {
            PlistValue::Integer(value) => value
                .as_unsigned()
                .or_else(|| value.as_signed().map(|n| n as u64)),
            PlistValue::String(value) => value.parse().ok(),
            _ => None,
        })
        .with_context(|| format!("GrandSlam response is missing `{key}` integer"))
}

pub(super) fn stringish(value: &PlistValue) -> Option<String> {
    match value {
        PlistValue::String(value) => Some(value.clone()),
        PlistValue::Integer(value) => value
            .as_signed()
            .map(|number| number.to_string())
            .or_else(|| value.as_unsigned().map(|number| number.to_string())),
        _ => None,
    }
}
