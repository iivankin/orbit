use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aes::Aes256;
use aes::cipher::consts::U16;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::{AesGcm, KeyInit, Nonce};
use anyhow::{Context, Result, bail};
use apple_srp_client::{G_2048, SrpClient, SrpClientVerifier};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use cbc::Decryptor;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use getrandom::fill as fill_random;
use hmac::{Hmac, Mac};
use plist::{Dictionary, Value as PlistValue};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::apple::anisette::load_local_anisette;
use crate::apple::auth::{
    EnsureUserAuthRequest, ensure_user_auth_with_password, resolve_user_auth_metadata,
};
use crate::apple::srp::encrypt_password;
use crate::context::AppContext;
use crate::util::{
    command_output, prompt_confirm, prompt_input, read_json_file_if_exists, write_json_file,
};

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
const GRAND_SLAM_CACHE_SAFETY_WINDOW_SECS: u64 = 300;

#[derive(Debug, Clone)]
struct ClientProfile {
    service: String,
    client_identifier: String,
    logical_user_id: String,
    client_info: String,
    user_agent: String,
    accept_language: String,
    locale: String,
    time_zone: String,
    device_id: String,
    serial_number: String,
    md: Option<String>,
    md_m: Option<String>,
    md_rinfo: String,
}

struct SrpInitSession {
    c: String,
    verifier: SrpClientVerifier<Sha256>,
}

#[derive(Debug)]
struct SrpCompleteOutcome {
    spd_plaintext: Option<Vec<u8>>,
}

#[derive(Debug)]
struct HttpDebugResponse {
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
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
struct ContentDeliveryHeaders {
    headers: BTreeMap<String, String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GrandSlamCacheState {
    xcode_notary_auth: Option<CachedXcodeNotaryAuth>,
    submit_auth: Vec<CachedSubmitAuth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedXcodeNotaryAuth {
    apple_id: String,
    expires_at_unix: u64,
    auth: XcodeNotaryAuth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSubmitAuth {
    apple_id: String,
    team_id: Option<String>,
    expires_at_unix: u64,
    lookup: LiveLookupAuth,
    upload: LiveProviderUploadAuth,
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
struct AuthenticateUserResult {
    #[serde(rename = "ProvidersByShortname", default)]
    providers_by_shortname: BTreeMap<String, ProviderInfo>,
    #[serde(rename = "Success", default)]
    success: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ProviderInfo {
    #[serde(rename = "ProviderName")]
    provider_name: String,
    #[serde(rename = "ProviderPublicId")]
    provider_public_id: String,
    #[serde(rename = "WWDRTeamID", default)]
    team_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProvidersInfoResult {
    #[serde(rename = "ProvidersInfo", default)]
    providers: BTreeMap<String, ProviderInfoEntry>,
    #[serde(rename = "Success", default)]
    success: bool,
}

#[derive(Debug, Deserialize)]
struct ProviderInfoEntry {
    #[serde(rename = "ProviderShortname")]
    provider_name: String,
    #[serde(rename = "PublicID")]
    provider_public_id: String,
    #[serde(rename = "WWDRTeamID", default)]
    team_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthenticateForSessionResult {
    #[serde(rename = "SessionId", default)]
    session_id: Option<String>,
    #[serde(rename = "SharedSecret", default)]
    shared_secret: Option<String>,
    #[serde(rename = "Success", default)]
    success: bool,
}

impl ClientProfile {
    fn default_detect() -> Result<Self> {
        Self::from_detection_options(ClientProfileDetectionOptions {
            service: "iTunes".to_owned(),
            client_identifier: None,
            client_info: None,
            user_agent: None,
            accept_language: None,
            locale: None,
            device_id: None,
            serial_number: None,
            md: None,
            md_m: None,
            md_rinfo: None,
        })
    }

    fn from_detection_options(options: ClientProfileDetectionOptions) -> Result<Self> {
        let system_info = SystemInfo::detect();
        let anisette = if options.md.is_some() && options.md_m.is_some() {
            None
        } else {
            load_local_anisette().ok()
        };
        let client_identifier = options
            .client_identifier
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string().to_uppercase());
        let logical_user_id = client_identifier.clone();
        let locale = options
            .locale
            .clone()
            .unwrap_or_else(|| DEFAULT_LOCALE.to_owned());
        let device_id = options
            .device_id
            .clone()
            .unwrap_or_else(|| system_info.device_id());
        let serial_number = options
            .serial_number
            .clone()
            .unwrap_or_else(|| system_info.serial_number());
        let client_info = options.client_info.clone().unwrap_or_else(|| {
            format!(
                "<{}> <Mac OS X;{};{}> <com.apple.akd/1.0 (com.apple.akd/1.0)>",
                system_info.model, system_info.product_version, system_info.build_version
            )
        });
        let user_agent = options
            .user_agent
            .clone()
            .unwrap_or_else(|| "akd/1.0".to_owned());
        let accept_language = options
            .accept_language
            .clone()
            .unwrap_or_else(|| DEFAULT_ACCEPT_LANGUAGE.to_owned());
        let time_zone = system_info.time_zone;

        Ok(Self {
            service: options.service,
            client_identifier,
            logical_user_id,
            client_info,
            user_agent,
            accept_language,
            locale,
            time_zone,
            device_id,
            serial_number,
            md: options
                .md
                .clone()
                .or_else(|| anisette.as_ref().map(|value| value.md.clone())),
            md_m: options
                .md_m
                .clone()
                .or_else(|| anisette.as_ref().map(|value| value.md_m.clone())),
            md_rinfo: options
                .md_rinfo
                .clone()
                .unwrap_or_else(|| DEFAULT_MD_RINFO.to_owned()),
        })
    }

    fn cpd(&self) -> Dictionary {
        let mut cpd = Dictionary::new();
        cpd.insert(
            "AppleIDClientIdentifier".to_owned(),
            PlistValue::String(self.client_identifier.clone()),
        );
        cpd.insert(
            "X-Apple-I-Client-Time".to_owned(),
            PlistValue::String(now_rfc3339()),
        );
        cpd.insert(
            "X-Apple-I-TimeZone".to_owned(),
            PlistValue::String(self.time_zone.clone()),
        );
        cpd.insert(
            "X-Apple-Locale".to_owned(),
            PlistValue::String(self.locale.clone()),
        );
        cpd.insert("loc".to_owned(), PlistValue::String(self.locale.clone()));
        cpd.insert(
            "X-Apple-I-MD-LU".to_owned(),
            PlistValue::String(STANDARD.encode(self.logical_user_id.as_bytes())),
        );
        cpd.insert(
            "X-Mme-Device-Id".to_owned(),
            PlistValue::String(self.device_id.clone()),
        );
        cpd.insert(
            "X-Apple-I-SRL-NO".to_owned(),
            PlistValue::String(self.serial_number.clone()),
        );
        cpd.insert(
            "X-Apple-I-MD-RINFO".to_owned(),
            PlistValue::String(self.md_rinfo.clone()),
        );
        cpd.insert("bootstrap".to_owned(), PlistValue::Boolean(true));
        cpd.insert("ckgen".to_owned(), PlistValue::Boolean(true));
        cpd.insert("pbe".to_owned(), PlistValue::Boolean(false));
        cpd.insert("svct".to_owned(), PlistValue::String(self.service.clone()));
        cpd.insert(
            "capp".to_owned(),
            PlistValue::String(DEFAULT_SERVICE_APP_NAME.to_owned()),
        );

        if let Some(md) = &self.md {
            cpd.insert("X-Apple-I-MD".to_owned(), PlistValue::String(md.clone()));
        }
        if let Some(md_m) = &self.md_m {
            cpd.insert(
                "X-Apple-I-MD-M".to_owned(),
                PlistValue::String(md_m.clone()),
            );
        }

        cpd
    }
}

struct ClientProfileDetectionOptions {
    service: String,
    client_identifier: Option<String>,
    client_info: Option<String>,
    user_agent: Option<String>,
    accept_language: Option<String>,
    locale: Option<String>,
    device_id: Option<String>,
    serial_number: Option<String>,
    md: Option<String>,
    md_m: Option<String>,
    md_rinfo: Option<String>,
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

#[derive(Debug)]
struct SystemInfo {
    model: String,
    product_version: String,
    build_version: String,
    platform_uuid: Option<String>,
    serial_number: Option<String>,
    time_zone: String,
}

#[derive(Debug)]
struct XcodeMetadata {
    short_version: String,
    build_version: String,
    xcode_build_id: String,
    itunes_software_service_build: String,
    cfnetwork_version: String,
    darwin_version: String,
    system_info: SystemInfo,
}

impl SystemInfo {
    fn detect() -> Self {
        let model = command_output(Command::new("sysctl").args(["-n", "hw.model"]))
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|_| "MacBookPro18,3".to_owned());
        let product_version = command_output(Command::new("sw_vers").arg("-productVersion"))
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|_| "15.0".to_owned());
        let build_version = command_output(Command::new("sw_vers").arg("-buildVersion"))
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|_| "0".to_owned());
        let time_zone = command_output(Command::new("date").arg("+%Z"))
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|_| "UTC".to_owned());
        let ioreg_output =
            command_output(Command::new("ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]))
                .unwrap_or_default();

        Self {
            model,
            product_version,
            build_version,
            platform_uuid: extract_quoted_ioreg_value(&ioreg_output, "IOPlatformUUID"),
            serial_number: extract_quoted_ioreg_value(&ioreg_output, "IOPlatformSerialNumber"),
            time_zone,
        }
    }

    fn device_id(&self) -> String {
        self.platform_uuid
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string().to_uppercase())
    }

    fn serial_number(&self) -> String {
        self.serial_number
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| sha1_hex_lower(self.device_id().as_bytes()))
    }
}

impl XcodeMetadata {
    fn detect() -> Result<Self> {
        let short_version = command_output(
            Command::new("defaults")
                .arg("read")
                .arg("/Applications/Xcode.app/Contents/Info")
                .arg("CFBundleShortVersionString"),
        )
        .map(|value| value.trim().to_owned())
        .or_else(|_| {
            command_output(Command::new("xcodebuild").arg("-version")).and_then(|output| {
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("Xcode "))
                    .map(|value| value.trim().to_owned())
                    .context("failed to parse `xcodebuild -version` output")
            })
        })?;
        let build_version = command_output(
            Command::new("defaults")
                .arg("read")
                .arg("/Applications/Xcode.app/Contents/Info")
                .arg("CFBundleVersion"),
        )
        .map(|value| value.trim().to_owned())?;
        let xcode_build_id =
            command_output(Command::new("xcodebuild").arg("-version")).and_then(|output| {
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("Build version "))
                    .map(|value| value.trim().to_owned())
                    .context("failed to parse Xcode build version from `xcodebuild -version`")
            })?;
        let itunes_software_service_build = command_output(
            Command::new("defaults")
                .arg("read")
                .arg("/Applications/Xcode.app/Contents/SharedFrameworks/DVTITunesSoftware.framework/Versions/A/XPCServices/com.apple.dt.Xcode.ITunesSoftwareService.xpc/Contents/Info")
                .arg("CFBundleVersion"),
        )
        .map(|value| value.trim().to_owned())
        .unwrap_or_else(|_| build_version.clone());
        let cfnetwork_version = command_output(
            Command::new("defaults")
                .arg("read")
                .arg("/System/Library/Frameworks/CFNetwork.framework/Resources/Info")
                .arg("CFBundleVersion"),
        )
        .map(|value| value.trim().to_owned())
        .unwrap_or_else(|_| "0".to_owned());
        let darwin_version = command_output(Command::new("uname").arg("-r"))
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|_| "0".to_owned());

        Ok(Self {
            short_version,
            build_version,
            xcode_build_id,
            itunes_software_service_build,
            cfnetwork_version,
            darwin_version,
            system_info: SystemInfo::detect(),
        })
    }

    fn authkit_client_info(&self) -> String {
        format!(
            "<{}> <macOS;{};{}> <com.apple.AuthKit/1 (com.apple.dt.Xcode/{})>",
            self.system_info.model,
            self.system_info.product_version,
            self.system_info.build_version,
            self.build_version
        )
    }

    fn notary_client_info(&self) -> String {
        format!(
            "<{}> <macOS;{};{}> <com.apple.AuthKit/1 (com.apple.dt.Xcode.ITunesSoftwareService/{})>",
            self.system_info.model,
            self.system_info.product_version,
            self.system_info.build_version,
            self.itunes_software_service_build
        )
    }

    fn authkit_user_agent(&self) -> String {
        format!(
            "Xcode/{} CFNetwork/{} Darwin/{}",
            self.build_version, self.cfnetwork_version, self.darwin_version
        )
    }

    fn version_header(&self) -> String {
        format!("{} ({})", self.short_version, self.xcode_build_id)
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
    let xcode = XcodeMetadata::detect()?;
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
    store_cached_xcode_notary_auth(app, &credentials.user.apple_id, app_token.expiry, &auth)?;
    Ok(auth)
}

fn grand_slam_cache_path(app: &AppContext) -> PathBuf {
    app.global_paths.cache_dir.join("grand-slam-auth.json")
}

fn load_grand_slam_cache_state(app: &AppContext) -> Result<GrandSlamCacheState> {
    Ok(read_json_file_if_exists(&grand_slam_cache_path(app))?.unwrap_or_default())
}

fn save_grand_slam_cache_state(app: &AppContext, state: &GrandSlamCacheState) -> Result<()> {
    write_json_file(&grand_slam_cache_path(app), state)
}

fn cached_xcode_notary_auth(app: &AppContext, apple_id: &str) -> Result<Option<XcodeNotaryAuth>> {
    let state = load_grand_slam_cache_state(app)?;
    Ok(state
        .xcode_notary_auth
        .filter(|cached| {
            cached.apple_id == apple_id && grand_slam_cache_is_fresh(cached.expires_at_unix)
        })
        .map(|cached| cached.auth))
}

fn store_cached_xcode_notary_auth(
    app: &AppContext,
    apple_id: &str,
    expires_at_unix: u64,
    auth: &XcodeNotaryAuth,
) -> Result<()> {
    let mut state = load_grand_slam_cache_state(app)?;
    state.xcode_notary_auth = Some(CachedXcodeNotaryAuth {
        apple_id: apple_id.to_owned(),
        expires_at_unix,
        auth: auth.clone(),
    });
    save_grand_slam_cache_state(app, &state)
}

fn cached_submit_auth(
    app: &AppContext,
    apple_id: &str,
    team_id: Option<&str>,
) -> Result<Option<CachedSubmitAuth>> {
    let state = load_grand_slam_cache_state(app)?;
    Ok(state.submit_auth.into_iter().find(|cached| {
        cached.apple_id == apple_id
            && grand_slam_cache_is_fresh(cached.expires_at_unix)
            && cached.team_id.as_deref() == team_id
    }))
}

fn store_cached_submit_auth(
    app: &AppContext,
    apple_id: &str,
    team_id: Option<&str>,
    expires_at_unix: u64,
    lookup: &LiveLookupAuth,
    upload: &LiveProviderUploadAuth,
) -> Result<()> {
    let mut state = load_grand_slam_cache_state(app)?;
    state.submit_auth.retain(|cached| {
        grand_slam_cache_is_fresh(cached.expires_at_unix)
            && !(cached.apple_id == apple_id && cached.team_id.as_deref() == team_id)
    });
    state.submit_auth.push(CachedSubmitAuth {
        apple_id: apple_id.to_owned(),
        team_id: team_id.map(ToOwned::to_owned),
        expires_at_unix,
        lookup: lookup.clone(),
        upload: upload.clone(),
    });
    save_grand_slam_cache_state(app, &state)
}

fn grand_slam_cache_is_fresh(expires_at_unix: u64) -> bool {
    expires_at_unix > current_unix_time().saturating_add(GRAND_SLAM_CACHE_SAFETY_WINDOW_SECS)
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// Transporter does not use the raw GsIdmsToken directly for content-delivery.
// It first exchanges the GrandSlam session for an app-scoped token bound to
// `com.apple.gs.itunesconnect.auth`, then uses that token as X-Apple-GS-Token.
fn request_app_token(
    client: &Client,
    profile: &ClientProfile,
    material: &GrandSlamAuthMaterial,
    service_id: &str,
) -> Result<GrandSlamAppToken> {
    let checksum = <Hmac<Sha256> as Mac>::new_from_slice(&material.session_key)
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

fn request_app_token_with_interactive_verification(
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

fn trusted_factor_headers(
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

#[derive(Debug, Clone)]
struct SelectedProvider {
    provider_name: String,
    provider_public_id: String,
}

fn select_provider_metadata(
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

impl ContentDeliveryHeaders {
    fn new(
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

    fn merged(&self, response_headers: &BTreeMap<String, String>) -> Self {
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

#[derive(Debug)]
struct JsonRpcCall<T> {
    result: T,
    raw_response: HttpDebugResponse,
}

fn execute_json_rpc<T: for<'de> Deserialize<'de>>(
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

fn content_delivery_auth_params(
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

fn build_client() -> Result<Client> {
    ClientBuilder::new()
        .brotli(true)
        .gzip(true)
        .deflate(true)
        .build()
        .context("failed to build GrandSlam debug HTTP client")
}

fn request_headers(profile: &ClientProfile) -> Result<HeaderMap> {
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

fn execute_plist_post(
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

fn plist_body(request: Dictionary) -> Vec<u8> {
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

    fn plist(&self) -> Option<PlistValue> {
        PlistValue::from_reader(Cursor::new(&self.body)).ok()
    }
}

fn printable_body(response: &HttpDebugResponse) -> String {
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

fn response_data(dictionary: &Dictionary, key: &str) -> Result<Vec<u8>> {
    dictionary
        .get(key)
        .and_then(|value| match value {
            PlistValue::Data(bytes) => Some(bytes.clone()),
            PlistValue::String(value) => STANDARD.decode(value).ok(),
            _ => None,
        })
        .with_context(|| format!("GrandSlam response is missing `{key}` data"))
}

fn response_string(dictionary: &Dictionary, key: &str) -> Result<String> {
    dictionary
        .get(key)
        .and_then(stringish)
        .with_context(|| format!("GrandSlam response is missing `{key}` string"))
}

fn response_unsigned(dictionary: &Dictionary, key: &str) -> Result<u64> {
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

fn stringish(value: &PlistValue) -> Option<String> {
    match value {
        PlistValue::String(value) => Some(value.clone()),
        PlistValue::Integer(value) => value
            .as_signed()
            .map(|number| number.to_string())
            .or_else(|| value.as_unsigned().map(|number| number.to_string())),
        _ => None,
    }
}

fn extract_quoted_ioreg_value(output: &str, key: &str) -> Option<String> {
    output.lines().find_map(|line| {
        if !line.contains(key) {
            return None;
        }
        let parts = line.split('"').collect::<Vec<_>>();
        parts.get(3).map(|value| value.trim().to_owned())
    })
}

fn sha1_hex_lower(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    output
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => unreachable!("nibble must be in range 0..=15"),
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
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
    fn plist_request_contains_request_section() {
        let body = plist_body(Dictionary::from_iter([(
            "o".to_owned(),
            PlistValue::String("init".to_owned()),
        )]));
        let plist = PlistValue::from_reader(Cursor::new(body)).expect("plist request should parse");
        let root = plist.as_dictionary().expect("root must be a dictionary");
        assert!(root.contains_key("Header"));
        assert!(root.contains_key("Request"));
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
