use std::process::Command;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use plist::{Dictionary, Value as PlistValue};
use sha1::{Digest as Sha1Digest, Sha1};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::apple::anisette::load_local_anisette;
use crate::util::command_output;

use super::{DEFAULT_ACCEPT_LANGUAGE, DEFAULT_LOCALE, DEFAULT_MD_RINFO};

#[derive(Debug, Clone)]
pub(super) struct ClientProfile {
    pub(super) service: String,
    pub(super) client_identifier: String,
    pub(super) logical_user_id: String,
    pub(super) client_info: String,
    pub(super) user_agent: String,
    pub(super) accept_language: String,
    pub(super) locale: String,
    pub(super) time_zone: String,
    pub(super) device_id: String,
    pub(super) serial_number: String,
    pub(super) md: Option<String>,
    pub(super) md_m: Option<String>,
    pub(super) md_rinfo: String,
}

#[derive(Debug)]
pub(super) struct SystemInfo {
    pub(super) model: String,
    pub(super) product_version: String,
    pub(super) build_version: String,
    pub(super) platform_uuid: Option<String>,
    pub(super) serial_number: Option<String>,
    pub(super) time_zone: String,
}

#[derive(Debug)]
pub(super) struct XcodeMetadata {
    pub(super) short_version: String,
    pub(super) build_version: String,
    pub(super) xcode_build_id: String,
    pub(super) itunes_software_service_build: String,
    pub(super) cfnetwork_version: String,
    pub(super) darwin_version: String,
    pub(super) system_info: SystemInfo,
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

impl ClientProfile {
    pub(super) fn default_detect() -> Result<Self> {
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
        let time_zone = system_info.time_zone.clone();

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

    pub(super) fn cpd(&self) -> Dictionary {
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
            PlistValue::String(super::DEFAULT_SERVICE_APP_NAME.to_owned()),
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
    pub(super) fn detect() -> Result<Self> {
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

    pub(super) fn authkit_client_info(&self) -> String {
        format!(
            "<{}> <macOS;{};{}> <com.apple.AuthKit/1 (com.apple.dt.Xcode/{})>",
            self.system_info.model,
            self.system_info.product_version,
            self.system_info.build_version,
            self.build_version
        )
    }

    pub(super) fn notary_client_info(&self) -> String {
        format!(
            "<{}> <macOS;{};{}> <com.apple.AuthKit/1 (com.apple.dt.Xcode.ITunesSoftwareService/{})>",
            self.system_info.model,
            self.system_info.product_version,
            self.system_info.build_version,
            self.itunes_software_service_build
        )
    }

    pub(super) fn authkit_user_agent(&self) -> String {
        format!(
            "Xcode/{} CFNetwork/{} Darwin/{}",
            self.build_version, self.cfnetwork_version, self.darwin_version
        )
    }

    pub(super) fn version_header(&self) -> String {
        format!("{} ({})", self.short_version, self.xcode_build_id)
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

pub(super) fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}
