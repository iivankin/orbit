mod cache;
mod client_profile;
mod transport;

use std::collections::BTreeMap;
use std::io::Cursor;

use aes::Aes256;
use anyhow::{Context, Result, bail};
use apple_srp_client::{G_2048, SrpClient, SrpClientVerifier};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use cbc::Decryptor;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use getrandom::fill as fill_random;
use hmac::{Hmac, Mac};
use plist::{Dictionary, Value as PlistValue};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use sha2::Sha256;

use self::cache::{
    cached_submit_auth, cached_xcode_notary_auth, store_cached_submit_auth,
    store_cached_xcode_notary_auth,
};
#[cfg(test)]
use self::client_profile::SystemInfo;
use self::client_profile::{ClientProfile, XcodeMetadata, now_rfc3339};
#[cfg(test)]
use self::transport::trusted_factor_headers;
use self::transport::{
    AuthenticateForSessionResult, AuthenticateUserResult, ContentDeliveryHeaders,
    ProvidersInfoResult, build_client, content_delivery_auth_params, execute_json_rpc,
    execute_plist_post, plist_body, printable_body,
    request_app_token_with_interactive_verification, request_headers, response_data,
    response_string, response_unsigned, select_provider_metadata, stringish,
};
use crate::apple::anisette::load_local_anisette;
use crate::apple::auth::{
    EnsureUserAuthRequest, ensure_user_auth_with_password, resolve_user_auth_metadata,
};
use crate::apple::srp::encrypt_password;
use crate::context::AppContext;

const GRAND_SLAM_SERVICE_URL: &str = "https://gsa.apple.com/grandslam/GsService2";
const DEFAULT_ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";
const DEFAULT_LOCALE: &str = "en_US";
const DEFAULT_MD_RINFO: &str = "17106176";
const DEFAULT_SERVICE_APP_NAME: &str = "AppStore";
const TRANSPORTER_APP_NAME: &str = "TransporterApp";
const TRANSPORTER_APP_BUNDLE_ID: &str = "com.apple.TransporterApp";
const TRANSPORTER_APP_VERSION: &str = "1.4 (14025)";
const TRANSPORTER_FRAMEWORK_FOUNDATION: &str = "9.101 (26101)";
const TRANSPORTER_FRAMEWORK_PACKAGE: &str = "9.101 (26101)";
const CONTENT_DELIVERY_APP_TOKEN_SERVICE: &str = "com.apple.gs.itunesconnect.auth";
const XCODE_NOTARY_APP_TOKEN_SERVICE: &str = "com.apple.gs.xcode.auth";
const AUTHKIT_APP_INFO: &str = "com.apple.gs.xcode.auth";

struct SrpInitSession {
    c: String,
    verifier: SrpClientVerifier<Sha256>,
}

#[derive(Debug)]
struct SrpCompleteOutcome {
    spd_plaintext: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct GrandSlamAuthMaterial {
    ds_person_id: String,
    adsid: String,
    gs_idms_token: String,
    session_key: Vec<u8>,
    continuation: Vec<u8>,
    service_tokens: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct GrandSlamAppToken {
    token: String,
    expiry: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LiveLookupAuth {
    pub provider_name: String,
    pub provider_public_id: String,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LiveProviderUploadAuth {
    pub provider_public_id: String,
    pub headers: BTreeMap<String, String>,
    pub session_id: String,
    pub shared_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct XcodeNotaryAuth {
    pub gs_token: String,
    pub identity_id: String,
    pub device_id: String,
    pub locale: String,
    pub time_zone: String,
    pub md_lu: String,
    pub md: String,
    pub md_m: String,
    pub md_rinfo: String,
    pub authkit_client_info: String,
    pub notary_client_info: String,
    pub authkit_user_agent: String,
    pub xcode_version_header: String,
}

impl GrandSlamAuthMaterial {
    fn from_plist(plist: &PlistValue) -> Result<Self> {
        let dictionary = plist
            .as_dictionary()
            .context("GrandSlam SPD root must be a dictionary")?;
        let service_tokens = dictionary
            .get("t")
            .and_then(PlistValue::as_dictionary)
            .map(|services| {
                services
                    .iter()
                    .filter_map(|(name, value)| {
                        let token = value
                            .as_dictionary()?
                            .get("token")
                            .and_then(PlistValue::as_string)?;
                        Some((name.clone(), token.to_owned()))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();

        Ok(Self {
            ds_person_id: response_string(dictionary, "DsPrsId")
                .or_else(|_| {
                    response_unsigned(dictionary, "DsPrsId").map(|value| value.to_string())
                })
                .context("GrandSlam SPD is missing `DsPrsId`")?,
            adsid: response_string(dictionary, "adsid")
                .context("GrandSlam SPD is missing `adsid`")?,
            gs_idms_token: response_string(dictionary, "GsIdmsToken")
                .context("GrandSlam SPD is missing `GsIdmsToken`")?,
            session_key: response_data(dictionary, "sk")
                .context("GrandSlam SPD is missing `sk`")?,
            continuation: response_data(dictionary, "c").context("GrandSlam SPD is missing `c`")?,
            service_tokens,
        })
    }
}

impl XcodeNotaryAuth {
    fn with_xcode_metadata(&self, xcode: &XcodeMetadata) -> Self {
        let mut auth = self.clone();
        auth.authkit_client_info = xcode.authkit_client_info();
        auth.notary_client_info = xcode.notary_client_info();
        auth.authkit_user_agent = xcode.authkit_user_agent();
        auth.xcode_version_header = xcode.version_header();
        auth
    }

    fn fresh_md(&self) -> String {
        // Xcode refreshes X-Apple-I-MD during long-running notarization polling.
        // Recompute it from local AOSKit whenever possible instead of reusing the
        // value captured during the initial GrandSlam bootstrap.
        load_local_anisette()
            .map(|anisette| anisette.md)
            .unwrap_or_else(|_| self.md.clone())
    }

    pub(crate) fn authkit_headers(&self) -> BTreeMap<String, String> {
        let md = self.fresh_md();
        let mut headers = BTreeMap::new();
        headers.insert("accept".to_owned(), "*/*".to_owned());
        headers.insert("accept-language".to_owned(), "en-US".to_owned());
        headers.insert("content-type".to_owned(), "application/json".to_owned());
        headers.insert("priority".to_owned(), "u=3".to_owned());
        headers.insert("user-agent".to_owned(), self.authkit_user_agent.clone());
        headers.insert("x-apple-app-info".to_owned(), AUTHKIT_APP_INFO.to_owned());
        headers.insert("x-apple-gs-token".to_owned(), self.gs_token.clone());
        headers.insert("x-apple-i-client-time".to_owned(), now_rfc3339());
        headers.insert("x-apple-i-device-type".to_owned(), "1".to_owned());
        headers.insert("x-apple-i-identity-id".to_owned(), self.identity_id.clone());
        headers.insert("x-apple-i-locale".to_owned(), self.locale.clone());
        headers.insert("x-apple-i-md".to_owned(), md);
        headers.insert("x-apple-i-md-lu".to_owned(), self.md_lu.clone());
        headers.insert("x-apple-i-md-m".to_owned(), self.md_m.clone());
        headers.insert("x-apple-i-md-rinfo".to_owned(), self.md_rinfo.clone());
        headers.insert("x-apple-i-timezone".to_owned(), self.time_zone.clone());
        headers.insert(
            "x-mme-client-info".to_owned(),
            self.authkit_client_info.clone(),
        );
        headers.insert("x-mme-device-id".to_owned(), self.device_id.clone());
        headers
    }

    pub(crate) fn notary_headers(&self, team_id: &str) -> BTreeMap<String, String> {
        let md = self.fresh_md();
        let mut headers = BTreeMap::new();
        headers.insert("accept".to_owned(), "*/*".to_owned());
        headers.insert(
            "accept-language".to_owned(),
            DEFAULT_ACCEPT_LANGUAGE.to_owned(),
        );
        headers.insert("content-type".to_owned(), "application/json".to_owned());
        headers.insert("priority".to_owned(), "u=3".to_owned());
        headers.insert("user-agent".to_owned(), "Xcode".to_owned());
        headers.insert("x-apple-app-info".to_owned(), AUTHKIT_APP_INFO.to_owned());
        headers.insert("x-apple-gs-token".to_owned(), self.gs_token.clone());
        headers.insert("x-apple-i-client-time".to_owned(), now_rfc3339());
        headers.insert("x-apple-i-device-type".to_owned(), "1".to_owned());
        headers.insert("x-apple-i-identity-id".to_owned(), self.identity_id.clone());
        headers.insert("x-apple-i-locale".to_owned(), self.locale.clone());
        headers.insert("x-apple-i-md".to_owned(), md);
        headers.insert("x-apple-i-md-lu".to_owned(), self.md_lu.clone());
        headers.insert("x-apple-i-md-m".to_owned(), self.md_m.clone());
        headers.insert("x-apple-i-md-rinfo".to_owned(), self.md_rinfo.clone());
        headers.insert("x-apple-i-timezone".to_owned(), self.time_zone.clone());
        headers.insert("x-developer-team-id".to_owned(), team_id.to_owned());
        headers.insert(
            "x-mme-client-info".to_owned(),
            self.notary_client_info.clone(),
        );
        headers.insert("x-mme-device-id".to_owned(), self.device_id.clone());
        headers.insert(
            "x-xcode-version".to_owned(),
            self.xcode_version_header.clone(),
        );
        headers
    }

    pub(crate) fn authkit_request_body(&self, client_public_key: &str) -> JsonValue {
        let md = self.fresh_md();
        json!({
            "client_public_key": client_public_key,
            "x_apple_gs_token": STANDARD.encode(format!("{}:{}", self.identity_id, self.gs_token)),
            "x_apple_i_md": md,
            "x_apple_i_md_m": self.md_m.clone(),
            "x_apple_i_rinfo": self.md_rinfo.clone(),
            "x_mme_client_Info": self.authkit_client_info.clone(),
            "x_mme_device_id": self.device_id.clone(),
        })
    }

    pub(crate) fn developer_services_headers(&self) -> BTreeMap<String, String> {
        let md = self.fresh_md();
        let mut headers = BTreeMap::new();
        headers.insert("user-agent".to_owned(), "Xcode".to_owned());
        headers.insert("x-apple-gs-token".to_owned(), self.gs_token.clone());
        headers.insert("x-apple-i-md".to_owned(), md);
        headers.insert("x-apple-i-identity-id".to_owned(), self.identity_id.clone());
        headers.insert("x-apple-i-md-lu".to_owned(), self.md_lu.clone());
        headers.insert("x-apple-app-info".to_owned(), AUTHKIT_APP_INFO.to_owned());
        headers.insert("x-mme-device-id".to_owned(), self.device_id.clone());
        headers.insert(
            "x-mme-client-info".to_owned(),
            self.authkit_client_info.clone(),
        );
        headers.insert("x-apple-i-timezone".to_owned(), self.time_zone.clone());
        headers.insert("x-apple-i-client-time".to_owned(), now_rfc3339());
        headers.insert(
            "x-xcode-version".to_owned(),
            self.xcode_version_header.clone(),
        );
        headers.insert(
            "accept-language".to_owned(),
            DEFAULT_ACCEPT_LANGUAGE.to_owned(),
        );
        headers.insert("x-apple-i-md-rinfo".to_owned(), self.md_rinfo.clone());
        headers.insert("x-apple-i-device-type".to_owned(), "1".to_owned());
        headers.insert("x-apple-i-locale".to_owned(), self.locale.clone());
        if !self.md_m.is_empty() {
            headers.insert("x-apple-i-md-m".to_owned(), self.md_m.clone());
        }
        headers
    }
}

fn start_srp_session(
    client: &Client,
    profile: &ClientProfile,
    apple_id: &str,
    password: &str,
) -> Result<SrpInitSession> {
    let mut secret = [0u8; 32];
    fill_random(&mut secret)
        .map_err(|error| anyhow::anyhow!("failed to generate GrandSlam SRP secret: {error}"))?;

    let srp = SrpClient::<Sha256>::new(&G_2048);
    let a_pub = srp.compute_public_ephemeral(&secret);
    let body = plist_body({
        let mut request = Dictionary::new();
        request.insert("A2k".to_owned(), PlistValue::Data(a_pub));
        request.insert("cpd".to_owned(), PlistValue::Dictionary(profile.cpd()));
        request.insert("o".to_owned(), PlistValue::String("init".to_owned()));
        request.insert(
            "ps".to_owned(),
            PlistValue::Array(vec![
                PlistValue::String("s2k".to_owned()),
                PlistValue::String("s2k_fo".to_owned()),
            ]),
        );
        request.insert("u".to_owned(), PlistValue::String(apple_id.to_owned()));
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
        .context("GrandSlam SRP init did not return a plist response")?;
    let response_dict = response_plist
        .as_dictionary()
        .context("GrandSlam SRP init root is not a dictionary")?;
    let response_section = response_dict
        .get("Response")
        .and_then(PlistValue::as_dictionary)
        .context("GrandSlam SRP init response is missing `Response`")?;
    let response_body = printable_body(&response);

    if let Some(error) = response_section.get("ErrorCode").and_then(stringish) {
        bail!("GrandSlam SRP init failed with ErrorCode={error}\n{response_body}");
    }

    let salt = response_data(response_section, "s").with_context(|| {
        format!("GrandSlam SRP init response did not include salt\n{response_body}")
    })?;
    let server_public = response_data(response_section, "B").with_context(|| {
        format!("GrandSlam SRP init response did not include server public value\n{response_body}")
    })?;
    let challenge = response_string(response_section, "c").with_context(|| {
        format!("GrandSlam SRP init response did not include challenge id\n{response_body}")
    })?;
    let iterations = response_unsigned(response_section, "i").with_context(|| {
        format!("GrandSlam SRP init response did not include iteration count\n{response_body}")
    })? as u32;
    let protocol = response_string(response_section, "sp").with_context(|| {
        format!("GrandSlam SRP init response did not include protocol\n{response_body}")
    })?;
    let encrypted_password = encrypt_password(password.as_bytes(), &salt, iterations, &protocol)?;
    let verifier = srp
        .process_reply(
            &secret,
            apple_id.as_bytes(),
            &encrypted_password,
            &salt,
            &server_public,
        )
        .map_err(|error| anyhow::anyhow!(error))
        .context("failed to process GrandSlam SRP challenge")?;

    Ok(SrpInitSession {
        c: challenge,
        verifier,
    })
}

fn complete_srp_session(
    client: &Client,
    profile: &ClientProfile,
    apple_id: &str,
    session: SrpInitSession,
) -> Result<SrpCompleteOutcome> {
    let body = plist_body({
        let mut request = Dictionary::new();
        request.insert(
            "M1".to_owned(),
            PlistValue::Data(session.verifier.proof().to_vec()),
        );
        request.insert("c".to_owned(), PlistValue::String(session.c));
        request.insert("cpd".to_owned(), PlistValue::Dictionary(profile.cpd()));
        request.insert("o".to_owned(), PlistValue::String("complete".to_owned()));
        request.insert("u".to_owned(), PlistValue::String(apple_id.to_owned()));
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
        .context("GrandSlam SRP complete did not return a plist response")?;
    let response_dict = response_plist
        .as_dictionary()
        .context("GrandSlam SRP complete root is not a dictionary")?;
    let response_section = response_dict
        .get("Response")
        .and_then(PlistValue::as_dictionary)
        .context("GrandSlam SRP complete response is missing `Response`")?;
    let response_body = printable_body(&response);

    if let Some(error_message) = response_section.get("Status").and_then(stringish) {
        bail!("GrandSlam SRP complete failed with Status={error_message}\n{response_body}");
    }

    let server_m2 = response_data(response_section, "M2")
        .or_else(|_| response_data(response_section, "m2"))
        .with_context(|| format!("GrandSlam SRP complete did not include `M2`\n{response_body}"))?;
    session
        .verifier
        .verify_server(&server_m2)
        .map_err(|error| anyhow::anyhow!(error))
        .context("GrandSlam SRP server proof verification failed")?;

    let spd_ciphertext = response_section.get("spd").and_then(|value| match value {
        PlistValue::Data(data) => Some(data.clone()),
        PlistValue::String(value) => STANDARD.decode(value).ok(),
        _ => None,
    });

    Ok(SrpCompleteOutcome {
        spd_plaintext: spd_ciphertext
            .as_ref()
            .map(|value| decrypt_spd(session.verifier.key(), value))
            .transpose()
            .context("failed to decrypt GrandSlam `spd` payload")?,
    })
}

fn decrypt_spd(session_key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let key = derive_spd_material(session_key, b"extra data key:")?;
    let iv_material = derive_spd_material(session_key, b"extra data iv:")?;
    let iv = iv_material
        .get(..16)
        .context("GrandSlam SPD IV derivation produced less than 16 bytes")?;
    let mut buffer = ciphertext.to_vec();
    let plaintext = Decryptor::<Aes256>::new_from_slices(&key, iv)
        .context("failed to initialize GrandSlam SPD decryptor")?
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|error| {
            anyhow::anyhow!("failed to remove GrandSlam SPD PKCS#7 padding: {error}")
        })?;
    Ok(plaintext.to_vec())
}

fn derive_spd_material(session_key: &[u8], label: &[u8]) -> Result<Vec<u8>> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(session_key)
        .map_err(|error| anyhow::anyhow!("invalid HMAC key: {error}"))?;
    mac.update(label);
    Ok(mac.finalize().into_bytes().to_vec())
}

pub(crate) fn establish_submit_auth(
    app: &AppContext,
    team_id: Option<&str>,
) -> Result<(LiveLookupAuth, LiveProviderUploadAuth)> {
    let requested_team_id = team_id
        .map(str::to_owned)
        .or_else(|| std::env::var("ORBIT_APPLE_TEAM_ID").ok())
        .or_else(|| {
            resolve_user_auth_metadata(app)
                .ok()
                .flatten()
                .and_then(|user| user.team_id)
        });
    if let Some(user) = resolve_user_auth_metadata(app)?
        && let Some(cached) = cached_submit_auth(app, &user.apple_id, requested_team_id.as_deref())?
    {
        return Ok((cached.lookup, cached.upload));
    }

    let profile = ClientProfile::default_detect()?;
    let client = build_client()?;
    let credentials = ensure_user_auth_with_password(
        app,
        EnsureUserAuthRequest {
            team_id: requested_team_id.clone(),
            prompt_for_missing: app.interactive,
            ..Default::default()
        },
    )
    .context("content-delivery submit requires Apple ID credentials")?;

    let srp_session = start_srp_session(
        &client,
        &profile,
        &credentials.user.apple_id,
        &credentials.password,
    )?;
    let complete_response =
        complete_srp_session(&client, &profile, &credentials.user.apple_id, srp_session)?;
    let spd_plaintext = complete_response
        .spd_plaintext
        .context("GrandSlam complete response did not include `spd`")?;
    let spd = PlistValue::from_reader(Cursor::new(&spd_plaintext))
        .context("GrandSlam SPD is not a plist")?;
    let material = GrandSlamAuthMaterial::from_plist(&spd)?;
    let app_token = request_app_token_with_interactive_verification(
        &client,
        &profile,
        &material,
        CONTENT_DELIVERY_APP_TOKEN_SERVICE,
        app.interactive,
    )?;
    let auth_headers = ContentDeliveryHeaders::new(
        &profile,
        &material,
        material.adsid.as_str(),
        app_token.token.as_str(),
    )?;

    let authenticate_user_response = execute_json_rpc::<AuthenticateUserResult>(
        &client,
        "MZContentDeliveryService",
        &auth_headers.headers,
        "authenticateUserWithArguments",
        content_delivery_auth_params(&profile, None)?,
    )?;
    if !authenticate_user_response.result.success {
        bail!("authenticateUserWithArguments returned Success=false");
    }

    let authenticate_session_headers =
        auth_headers.merged(&authenticate_user_response.raw_response.headers);
    let authenticate_session_response = execute_json_rpc::<AuthenticateForSessionResult>(
        &client,
        "MZContentDeliveryService",
        &authenticate_session_headers.headers,
        "authenticateForSession",
        content_delivery_auth_params(&profile, None)?,
    )?;
    if !authenticate_session_response.result.success {
        bail!("authenticateForSession returned Success=false");
    }

    let provider_headers =
        authenticate_session_headers.merged(&authenticate_session_response.raw_response.headers);
    let providers_info_response = execute_json_rpc::<ProvidersInfoResult>(
        &client,
        "MZITunesProducerService",
        &provider_headers.headers,
        "providersInfoWithArguments",
        content_delivery_auth_params(&profile, None)?,
    )?;
    if !providers_info_response.result.success {
        bail!("providersInfoWithArguments returned Success=false");
    }

    let selected_team_id = requested_team_id;
    let provider = select_provider_metadata(
        selected_team_id.as_deref(),
        &authenticate_user_response.result.providers_by_shortname,
        &providers_info_response.result.providers,
    )?;
    let provider_session_response = execute_json_rpc::<AuthenticateForSessionResult>(
        &client,
        "MZContentDeliveryService",
        &provider_headers.headers,
        "authenticateForSession",
        content_delivery_auth_params(&profile, Some(provider.provider_name.as_str()))?,
    )?;
    if !provider_session_response.result.success {
        bail!("provider authenticateForSession returned Success=false");
    }

    let session_id = provider_session_response
        .result
        .session_id
        .context("provider authenticateForSession did not include SessionId")?;
    let shared_secret = provider_session_response
        .result
        .shared_secret
        .context("provider authenticateForSession did not include SharedSecret")?;

    let lookup = LiveLookupAuth {
        provider_name: provider.provider_name.clone(),
        provider_public_id: provider.provider_public_id.clone(),
        headers: provider_headers.headers.clone(),
    };
    let upload = LiveProviderUploadAuth {
        provider_public_id: provider.provider_public_id,
        headers: provider_headers.headers,
        session_id,
        shared_secret,
    };
    store_cached_submit_auth(
        app,
        &credentials.user.apple_id,
        selected_team_id
            .as_deref()
            .or(credentials.user.team_id.as_deref()),
        app_token.expiry,
        &lookup,
        &upload,
    )?;
    Ok((lookup, upload))
}

pub(crate) fn establish_xcode_notary_auth(app: &AppContext) -> Result<XcodeNotaryAuth> {
    if let Some(user) = resolve_user_auth_metadata(app)?
        && let Some(cached) = cached_xcode_notary_auth(app, &user.apple_id)?
    {
        return Ok(cached);
    }

    establish_xcode_notary_auth_with_metadata(app, XcodeMetadata::detect()?, true)
}

pub(crate) fn establish_xcode_download_auth(
    app: &AppContext,
    short_version: &str,
    build_version: &str,
) -> Result<XcodeNotaryAuth> {
    let xcode = XcodeMetadata::synthetic(short_version, build_version);
    if let Some(user) = resolve_user_auth_metadata(app)?
        && let Some(cached) = cached_xcode_notary_auth(app, &user.apple_id)?
    {
        return Ok(cached.with_xcode_metadata(&xcode));
    }
    establish_xcode_notary_auth_with_metadata(app, xcode, false)
}

fn establish_xcode_notary_auth_with_metadata(
    app: &AppContext,
    xcode: XcodeMetadata,
    persist_cache: bool,
) -> Result<XcodeNotaryAuth> {
    let profile = ClientProfile::default_detect()?;
    let client = build_client()?;
    let credentials = ensure_user_auth_with_password(
        app,
        EnsureUserAuthRequest {
            prompt_for_missing: app.interactive,
            ..Default::default()
        },
    )
    .context("Xcode-like notarization requires Apple ID credentials")?;

    let srp_session = start_srp_session(
        &client,
        &profile,
        &credentials.user.apple_id,
        &credentials.password,
    )?;
    let complete_response =
        complete_srp_session(&client, &profile, &credentials.user.apple_id, srp_session)?;
    let spd_plaintext = complete_response
        .spd_plaintext
        .context("GrandSlam complete response did not include `spd`")?;
    let spd = PlistValue::from_reader(Cursor::new(&spd_plaintext))
        .context("GrandSlam SPD is not a plist")?;
    let material = GrandSlamAuthMaterial::from_plist(&spd)?;
    let app_token = request_app_token_with_interactive_verification(
        &client,
        &profile,
        &material,
        XCODE_NOTARY_APP_TOKEN_SERVICE,
        app.interactive,
    )
    .context("failed to acquire the Xcode notary app token")?;
    let md_lu = profile
        .cpd()
        .get("X-Apple-I-MD-LU")
        .and_then(PlistValue::as_string)
        .map(ToOwned::to_owned)
        .context("CPD is missing X-Apple-I-MD-LU")?;
    let md = profile
        .md
        .clone()
        .context("Xcode-like notarization requires X-Apple-I-MD anisette")?;
    let md_m = profile
        .md_m
        .clone()
        .context("Xcode-like notarization requires X-Apple-I-MD-M anisette")?;

    let auth = XcodeNotaryAuth {
        gs_token: app_token.token,
        identity_id: material.adsid,
        device_id: profile.device_id,
        locale: profile.locale,
        time_zone: profile.time_zone,
        md_lu,
        md,
        md_m,
        md_rinfo: profile.md_rinfo,
        authkit_client_info: xcode.authkit_client_info(),
        notary_client_info: xcode.notary_client_info(),
        authkit_user_agent: xcode.authkit_user_agent(),
        xcode_version_header: xcode.version_header(),
    };
    if persist_cache {
        store_cached_xcode_notary_auth(app, &credentials.user.apple_id, app_token.expiry, &auth)?;
    }
    Ok(auth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::Encryptor;
    use cbc::cipher::BlockEncryptMut;

    #[test]
    fn cpd_includes_expected_identity_fields() {
        let profile = ClientProfile {
            service: "iTunes".to_owned(),
            client_identifier: "CLIENT-ID".to_owned(),
            logical_user_id: "USER-ID".to_owned(),
            client_info: "client-info".to_owned(),
            user_agent: "akd/1.0".to_owned(),
            accept_language: DEFAULT_ACCEPT_LANGUAGE.to_owned(),
            locale: "en_US".to_owned(),
            time_zone: "UTC".to_owned(),
            device_id: "DEVICE-ID".to_owned(),
            serial_number: "SERIAL".to_owned(),
            md: Some("MD".to_owned()),
            md_m: Some("MD-M".to_owned()),
            md_rinfo: DEFAULT_MD_RINFO.to_owned(),
        };

        let cpd = profile.cpd();
        assert_eq!(
            cpd.get("AppleIDClientIdentifier")
                .and_then(PlistValue::as_string),
            Some("CLIENT-ID")
        );
        assert_eq!(
            cpd.get("X-Mme-Device-Id").and_then(PlistValue::as_string),
            Some("DEVICE-ID")
        );
        assert_eq!(
            cpd.get("X-Apple-I-SRL-NO").and_then(PlistValue::as_string),
            Some("SERIAL")
        );
        assert_eq!(
            cpd.get("X-Apple-I-MD").and_then(PlistValue::as_string),
            Some("MD")
        );
        assert_eq!(
            cpd.get("X-Apple-I-MD-M").and_then(PlistValue::as_string),
            Some("MD-M")
        );
    }

    #[test]
    fn decrypt_spd_uses_expected_session_key_derivation() {
        let session_key = b"grand-slam-session-key";
        let plaintext = br#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>adsid</key><string>123456789</string></dict></plist>"#;
        let key = derive_spd_material(session_key, b"extra data key:")
            .expect("key derivation should work");
        let iv_material =
            derive_spd_material(session_key, b"extra data iv:").expect("IV derivation should work");
        let iv = &iv_material[..16];

        let mut buffer = vec![0u8; plaintext.len() + 16];
        buffer[..plaintext.len()].copy_from_slice(plaintext);
        let ciphertext = Encryptor::<Aes256>::new_from_slices(&key, iv)
            .expect("encryptor should initialize")
            .encrypt_padded_mut::<Pkcs7>(&mut buffer, plaintext.len())
            .expect("padding should succeed")
            .to_vec();

        let decrypted = decrypt_spd(session_key, &ciphertext).expect("decrypt should succeed");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn trusted_factor_headers_include_identity_token_and_anisette() {
        let profile = ClientProfile {
            service: "iTunes".to_owned(),
            client_identifier: "CLIENT-ID".to_owned(),
            logical_user_id: "USER-ID".to_owned(),
            client_info: "client-info".to_owned(),
            user_agent: "akd/1.0".to_owned(),
            accept_language: DEFAULT_ACCEPT_LANGUAGE.to_owned(),
            locale: "en_US".to_owned(),
            time_zone: "UTC".to_owned(),
            device_id: "DEVICE-ID".to_owned(),
            serial_number: "SERIAL".to_owned(),
            md: Some("MD".to_owned()),
            md_m: Some("MD-M".to_owned()),
            md_rinfo: DEFAULT_MD_RINFO.to_owned(),
        };
        let material = GrandSlamAuthMaterial {
            ds_person_id: "123456789".to_owned(),
            adsid: "000111-222333".to_owned(),
            gs_idms_token: "TOKEN".to_owned(),
            session_key: vec![0; 32],
            continuation: vec![1, 2, 3],
            service_tokens: BTreeMap::new(),
        };
        let xcode = XcodeMetadata {
            short_version: "16.0".to_owned(),
            build_version: "12345".to_owned(),
            xcode_build_id: "16A123".to_owned(),
            itunes_software_service_build: "12345".to_owned(),
            cfnetwork_version: "1".to_owned(),
            darwin_version: "1".to_owned(),
            system_info: SystemInfo {
                model: "MacBookPro".to_owned(),
                product_version: "15.0".to_owned(),
                build_version: "24A".to_owned(),
                platform_uuid: Some("UUID".to_owned()),
                serial_number: Some("SERIAL".to_owned()),
                time_zone: "UTC".to_owned(),
            },
        };

        let headers = trusted_factor_headers(&profile, &material, &xcode, Some("123456")).unwrap();
        assert_eq!(
            headers.get("x-apple-identity-token"),
            Some(&STANDARD.encode("123456789:TOKEN"))
        );
        assert_eq!(headers.get("x-apple-i-md"), Some(&"MD".to_owned()));
        assert_eq!(headers.get("x-apple-i-md-m"), Some(&"MD-M".to_owned()));
        assert_eq!(headers.get("security-code"), Some(&"123456".to_owned()));
    }
}
