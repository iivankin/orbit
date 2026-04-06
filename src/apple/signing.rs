mod capability_sync;
mod cleanup;
mod credentials;
mod device_selection;
mod entitlements;
mod local_state;
mod prepare;
mod profile_types;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use self::capability_sync::{
    ensure_bundle_id_with_api_key, ensure_bundle_id_with_developer_services, sync_capabilities,
    sync_capabilities_with_api_key,
};
pub use self::cleanup::{
    LocalSigningCleanSummary, RemoteSigningCleanSummary, clean_local_signing_state,
    clean_remote_signing_state,
};
use self::device_selection::{CurrentProfileLookup, resolve_profile_device_ids};
use self::entitlements::load_plist_dictionary;
pub use self::entitlements::target_is_app_clip;
use self::entitlements::{
    host_app_for_app_clip, materialize_macos_debug_trace_entitlements,
    materialize_signing_entitlements,
};
use self::local_state::{
    CertificateOrigin, ManagedCertificate, ManagedProfile, SigningIdentity, SigningState,
    certificate_has_local_signing_material, delete_certificate_files, delete_file_if_exists,
    delete_p12_password, export_certificate_der, export_p12_from_der_certificate,
    extract_certificate_from_p12, extract_private_key_from_p12, load_p12_password, load_state,
    read_certificate_common_name, read_certificate_serial, recover_orphaned_certificate,
    recover_system_keychain_identity, resolve_signing_identity, save_state, store_p12_password,
    team_signing_paths,
};
use self::profile_types::asc_bundle_id_platform;
use self::profile_types::{
    asc_certificate_type, asc_installer_certificate_type, asc_profile_type, certificate_type,
    developer_services_certificate_type, developer_services_installer_certificate_type,
    installer_certificate_type, profile_type,
};
use crate::apple::asc_api::{
    AscClient, BundleIdAttributes, CapabilityOption, CapabilitySetting, Resource,
    remote_capabilities_from_included,
};
use crate::apple::auth::resolve_api_key_auth;
use crate::apple::capabilities::{
    CapabilityRelationships, CapabilitySyncOptions, CapabilityUpdate, RemoteCapability,
    capability_sync_plan_from_dictionary_with_options,
};
use crate::apple::provisioning::{ProvisioningBundleId, ProvisioningClient, ProvisioningProfile};
use crate::apple::runtime::{
    apple_platform_from_cli, build_target_for_platform, distribution_from_cli,
    profile_for_distribution, resolve_build_distribution, resolve_platform,
};
use crate::cli::{SigningExportArgs, SigningImportArgs};
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest, TargetManifest};
use crate::util::{
    CliSpinner, copy_file, ensure_dir, ensure_parent_dir, prompt_password, read_json_file,
};
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use plist::Dictionary;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const P12_PASSWORD_SERVICE: &str = "dev.orbit.cli.codesign-p12";
const ASC_SUPPORTED_CAPABILITIES: &[&str] = &[
    "ACCESS_WIFI_INFORMATION",
    "APPLE_ID_AUTH",
    "APP_GROUPS",
    "APPLE_PAY",
    "ASSOCIATED_DOMAINS",
    "AUTOFILL_CREDENTIAL_PROVIDER",
    "CLASSKIT",
    "COREMEDIA_HLS_LOW_LATENCY",
    "DATA_PROTECTION",
    "GAME_CENTER",
    "HEALTHKIT",
    "HOMEKIT",
    "HOT_SPOT",
    "ICLOUD",
    "IN_APP_PURCHASE",
    "INTER_APP_AUDIO",
    "MAPS",
    "MULTIPATH",
    "NETWORK_CUSTOM_PROTOCOL",
    "NETWORK_EXTENSIONS",
    "NFC_TAG_READING",
    "PERSONAL_VPN",
    "PUSH_NOTIFICATIONS",
    "SIRIKIT",
    "SYSTEM_EXTENSION_INSTALL",
    "USER_MANAGEMENT",
    "WALLET",
    "WIRELESS_ACCESSORY_CONFIGURATION",
];
const ASC_SETTING_ICLOUD_VERSION: &str = "ICLOUD_VERSION";
const ASC_SETTING_DATA_PROTECTION: &str = "DATA_PROTECTION_PERMISSION_LEVEL";
const ASC_SETTING_APPLE_ID_AUTH: &str = "APPLE_ID_AUTH_APP_CONSENT";
const ASC_OPTION_OFF: &str = "OFF";
const ASC_OPTION_ON: &str = "ON";
const ASC_OPTION_ICLOUD_XCODE_6: &str = "XCODE_6";
const ASC_OPTION_APPLE_ID_PRIMARY_CONSENT: &str = "PRIMARY_APP_CONSENT";
const ASC_OPTION_DATA_PROTECTION_COMPLETE: &str = "COMPLETE_PROTECTION";
const ASC_OPTION_DATA_PROTECTION_PROTECTED_UNLESS_OPEN: &str = "PROTECTED_UNLESS_OPEN";
const ASC_OPTION_DATA_PROTECTION_PROTECTED_UNTIL_FIRST_USER_AUTH: &str =
    "PROTECTED_UNTIL_FIRST_USER_AUTH";
const ASC_OPTION_PUSH_BROADCAST: &str = "PUSH_NOTIFICATION_FEATURE_BROADCAST";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum CapabilitySyncOutcome {
    Skipped,
    NoUpdates,
    Updated(usize),
}

fn signing_progress_step<T, F, G>(
    message: impl Into<String>,
    success_message: G,
    action: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
    G: FnOnce(&T) -> String,
{
    let spinner = CliSpinner::new(message.into());
    match action() {
        Ok(value) => {
            spinner.finish_success(success_message(&value));
            Ok(value)
        }
        Err(error) => {
            spinner.finish_clear();
            Err(error)
        }
    }
}

fn capability_sync_success_message(
    bundle_identifier: &str,
    outcome: CapabilitySyncOutcome,
) -> String {
    match outcome {
        CapabilitySyncOutcome::Skipped => {
            format!("No entitlements to sync for `{bundle_identifier}`.")
        }
        CapabilitySyncOutcome::NoUpdates => {
            format!("Synced capabilities for `{bundle_identifier}`: no updates.")
        }
        CapabilitySyncOutcome::Updated(count) => {
            format!("Synced capabilities for `{bundle_identifier}`: {count} update(s).")
        }
    }
}

#[derive(Debug, Clone)]
struct AscCapabilityMutation {
    remote_id: Option<String>,
    capability_type: String,
    settings: Vec<CapabilitySetting>,
    delete: bool,
}

#[derive(Debug, Clone)]
pub struct SigningMaterial {
    pub signing_identity: String,
    pub keychain_path: PathBuf,
    pub provisioning_profile_path: PathBuf,
    pub entitlements_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PackageSigningMaterial {
    pub signing_identity: String,
    pub keychain_path: PathBuf,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushEnvironment {
    Development,
    Production,
}

impl std::fmt::Display for PushEnvironment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Development => "development",
            Self::Production => "production",
        })
    }
}
pub use self::credentials::{export_signing_credentials, import_signing_credentials};
pub use self::prepare::{prepare_package_signing, prepare_signing};

pub fn sign_bundle(
    platform: ApplePlatform,
    distribution: DistributionKind,
    bundle_path: &Path,
    material: &SigningMaterial,
) -> Result<()> {
    let embedded_profile = if platform == ApplePlatform::Macos {
        bundle_path
            .join("Contents")
            .join("embedded.provisionprofile")
    } else {
        bundle_path.join("embedded.mobileprovision")
    };
    ensure_parent_dir(&embedded_profile)?;
    fs::copy(&material.provisioning_profile_path, &embedded_profile).with_context(|| {
        format!(
            "failed to embed provisioning profile into {}",
            bundle_path.display()
        )
    })?;

    let mut command = Command::new("codesign");
    command.args(["--force", "--sign"]);
    command.arg(&material.signing_identity);
    command.args(["--keychain"]);
    command.arg(&material.keychain_path);
    if platform == ApplePlatform::Macos && distribution == DistributionKind::DeveloperId {
        // Developer ID notarization requires the app binary to opt into hardened runtime.
        command.args(["--options", "runtime"]);
    }
    if let Some(entitlements) = &material.entitlements_path {
        command.args(["--entitlements"]);
        command.arg(entitlements);
    }
    command.arg(bundle_path);
    crate::util::run_command(&mut command)
}

pub fn prepare_macos_bundle_for_debug_tracing(
    project: &ProjectContext,
    target: &TargetManifest,
    bundle_path: &Path,
) -> Result<()> {
    let entitlements_path =
        materialize_macos_debug_trace_entitlements(project, target, bundle_path)?;
    let mut command = Command::new("codesign");
    command.args(["--force", "--sign", "-", "--entitlements"]);
    command.arg(&entitlements_path);
    command.arg(bundle_path);
    crate::util::run_command(&mut command)
}

fn identifier_name(prefix: &str, identifier: &str) -> String {
    format!("{prefix} {identifier}")
}

fn orbit_managed_app_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' => character,
            _ => ' ',
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if sanitized.is_empty() {
        "Orbit App".to_owned()
    } else {
        format!("Orbit {sanitized}")
    }
}

fn resolve_local_team_id_if_known(project: &ProjectContext) -> Result<Option<String>> {
    Ok(std::env::var("ORBIT_APPLE_TEAM_ID")
        .ok()
        .or_else(|| project.resolved_manifest.team_id.clone())
        .or_else(|| {
            persisted_manifest_team_id(&project.manifest_path)
                .ok()
                .flatten()
        }))
}

fn resolve_local_team_id(project: &ProjectContext) -> Result<String> {
    resolve_local_team_id_if_known(project)?.context(
        "signing state is scoped by Apple team; set `team_id` in orbit.json or export ORBIT_APPLE_TEAM_ID",
    )
}

fn canonical_ids(ids: &[String]) -> Vec<String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids
}

fn profile_covers_requested_ids(actual: &[String], requested: &[String]) -> bool {
    if requested.is_empty() {
        return actual.is_empty();
    }

    // Apple-managed development profiles can legitimately expand to a superset
    // of the requested devices, especially on macOS where team provisioning
    // profiles may include every registered Mac for the team.
    let actual = actual.iter().map(String::as_str).collect::<HashSet<_>>();
    requested.iter().all(|id| actual.contains(id.as_str()))
}

fn persisted_manifest_team_id(manifest_path: &Path) -> Result<Option<String>> {
    let manifest: JsonValue = read_json_file(manifest_path)?;
    Ok(manifest
        .get("team_id")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned))
}

#[cfg(test)]
mod tests;
