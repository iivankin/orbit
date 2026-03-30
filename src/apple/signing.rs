use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use plist::{Dictionary, Value};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::apple::asc_api::{
    AscClient, BundleIdAttributes, CapabilityOption, CapabilitySetting, Resource,
    remote_capabilities_from_included,
};
use crate::apple::auth::{resolve_api_key_auth, resolve_user_auth_metadata};
use crate::apple::capabilities::{
    CapabilityRelationships, CapabilitySyncOptions, CapabilityUpdate, RemoteCapability,
    capability_sync_plan_from_dictionary_with_options, capability_sync_plan_from_entitlements,
};
use crate::apple::provisioning::{
    ProvisioningBundleId, ProvisioningCapabilityRelationships, ProvisioningCapabilityUpdate,
    ProvisioningClient, ProvisioningDevice, ProvisioningProfile,
};
use crate::apple::runtime::{
    apple_platform_from_cli, build_target_for_platform, distribution_from_cli,
    profile_for_distribution, resolve_build_distribution, resolve_platform,
};
use crate::cli::{SigningExportArgs, SigningImportArgs};
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest, TargetManifest};
use crate::util::{
    CliSpinner, copy_file, ensure_dir, ensure_parent_dir, prompt_confirm, prompt_multi_select,
    prompt_password, prompt_select, read_json_file_if_exists, run_command_capture, write_json_file,
};

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
const APP_IDENTIFIER_PREFIX_PLACEHOLDER: &str = "$(AppIdentifierPrefix)";
const TEAM_IDENTIFIER_PREFIX_PLACEHOLDER: &str = "$(TeamIdentifierPrefix)";
const MANAGED_SIGNING_ENTITLEMENTS: &[&str] = &[
    "application-identifier",
    "aps-environment",
    "com.apple.developer.team-identifier",
    "com.apple.developer.ubiquity-kvstore-identifier",
    "get-task-allow",
    "keychain-access-groups",
    "beta-reports-active",
    "com.apple.security.get-task-allow",
    "com.apple.security.app-sandbox",
    "com.apple.security.network.client",
    "com.apple.security.network.server",
    "com.apple.security.files.user-selected.read-only",
    "com.apple.security.files.user-selected.read-write",
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SigningState {
    certificates: Vec<ManagedCertificate>,
    profiles: Vec<ManagedProfile>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
enum CertificateOrigin {
    #[default]
    Generated,
    Imported,
}

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

fn certificate_has_local_signing_material(certificate: &ManagedCertificate) -> bool {
    certificate.system_signing_identity.is_some() || certificate.p12_path.exists()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedCertificate {
    id: String,
    certificate_type: String,
    serial_number: String,
    #[serde(default)]
    origin: CertificateOrigin,
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_keychain_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_signing_identity: Option<String>,
    private_key_path: PathBuf,
    certificate_der_path: PathBuf,
    p12_path: PathBuf,
    p12_password_account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedProfile {
    id: String,
    profile_type: String,
    bundle_id: String,
    path: PathBuf,
    uuid: Option<String>,
    certificate_ids: Vec<String>,
    device_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct ManagedBundleId {
    id: String,
    identifier: String,
}

impl From<&ProvisioningBundleId> for ManagedBundleId {
    fn from(value: &ProvisioningBundleId) -> Self {
        Self {
            id: value.id.clone(),
            identifier: value.identifier.clone(),
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

struct DeviceSelectionContext<'a> {
    state: &'a SigningState,
    bundle_identifier: &'a str,
    profile_type: &'a str,
    certificate_id: &'a str,
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

#[derive(Debug, Clone)]
struct SigningIdentity {
    hash: String,
    keychain_path: PathBuf,
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

#[derive(Debug, Clone)]
struct TeamSigningPaths {
    state_path: PathBuf,
    certificates_dir: PathBuf,
    profiles_dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct LocalSigningCleanSummary {
    pub removed_profiles: usize,
    pub removed_certificates: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteSigningCleanSummary {
    pub removed_apps: usize,
    pub removed_profiles: usize,
    pub removed_app_groups: usize,
    pub removed_merchants: usize,
    pub removed_cloud_containers: usize,
}

pub fn export_signing_credentials(
    project: &ProjectContext,
    args: &SigningExportArgs,
) -> Result<()> {
    let selection = resolve_signing_selection(
        project,
        args.platform.map(apple_platform_from_cli),
        distribution_from_cli(args.distribution),
    )?;
    let team_id = resolve_local_team_id(project)?;
    let state = load_state(project, &team_id)?;
    let profile_type = profile_type(selection.platform, &selection.profile)?;
    let managed_profile = state
        .profiles
        .iter()
        .rev()
        .find(|candidate| {
            candidate.bundle_id == selection.target.bundle_id
                && candidate.profile_type == profile_type
                && candidate.path.exists()
        })
        .with_context(|| {
            format!(
                "no Orbit-managed provisioning profile was found for `{}` ({}/{}) under Apple team `{team_id}`; build or submit the target first so Orbit can prepare signing material",
                selection.target.name,
                selection.platform,
                selection.profile.variant_name()
            )
        })?;
    let certificate_id = managed_profile
        .certificate_ids
        .first()
        .context("managed provisioning profile did not reference a signing certificate")?;
    let certificate = state
        .certificates
        .iter()
        .find(|candidate| candidate.id == *certificate_id && candidate.p12_path.exists())
        .with_context(|| {
            format!(
                "no local P12 was found for certificate `{certificate_id}` under Apple team `{team_id}`"
            )
        })?;

    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        project
            .project_paths
            .artifacts_dir
            .join("signing-export")
            .join(format!(
                "{}-{}-{}",
                selection.target.name,
                selection.platform,
                selection.profile.variant_name()
            ))
    });
    ensure_dir(&output_dir)?;

    let file_stem = format!(
        "{}-{}-{}",
        selection.target.name,
        selection.platform,
        selection.profile.variant_name()
    );
    let p12_path = output_dir.join(format!("{file_stem}.p12"));
    let profile_path = output_dir.join(format!("{file_stem}.mobileprovision"));
    copy_file(&certificate.p12_path, &p12_path)?;
    copy_file(&managed_profile.path, &profile_path)?;
    let password = load_p12_password(&certificate.p12_password_account)?;

    println!("team_id: {team_id}");
    println!("certificate_type: {}", certificate.certificate_type);
    println!("certificate_serial: {}", certificate.serial_number);
    println!("p12: {}", p12_path.display());
    println!("p12_password: {password}");
    println!("provisioning_profile: {}", profile_path.display());
    Ok(())
}

pub fn import_signing_credentials(
    project: &ProjectContext,
    args: &SigningImportArgs,
) -> Result<()> {
    let selection = resolve_signing_selection(
        project,
        args.platform.map(apple_platform_from_cli),
        distribution_from_cli(args.distribution),
    )?;
    let password = match &args.password {
        Some(password) => password.clone(),
        None if project.app.interactive => prompt_password("P12 password")?,
        None => bail!("--password is required in non-interactive mode"),
    };
    let team_id = resolve_local_team_id(project)?;
    let mut state = load_state(project, &team_id)?;
    let certificate_type = certificate_type(selection.platform, &selection.profile)?;
    let paths = team_signing_paths(project, &team_id);
    ensure_dir(&paths.certificates_dir)?;

    let slug = format!("{}-{}", crate::util::timestamp_slug(), uuid::Uuid::new_v4());
    let imported_p12_path = paths.certificates_dir.join(format!("{slug}.p12"));
    let private_key_path = paths.certificates_dir.join(format!("{slug}.key.pem"));
    let certificate_pem_path = paths.certificates_dir.join(format!("{slug}.cert.pem"));
    let certificate_der_path = paths.certificates_dir.join(format!("{slug}.cer"));
    copy_file(&args.p12, &imported_p12_path)?;

    extract_private_key_from_p12(&imported_p12_path, &private_key_path, &password)?;
    extract_certificate_from_p12(&imported_p12_path, &certificate_pem_path, &password)?;
    export_certificate_der(&certificate_pem_path, &certificate_der_path)?;

    let serial_number = read_certificate_serial(&certificate_der_path)?;
    let display_name = read_certificate_common_name(&certificate_pem_path)?;
    let _ = fs::remove_file(&certificate_pem_path);
    let password_account = format!("imported-{serial_number}");
    store_p12_password(&password_account, &password)?;

    if let Some(existing_index) = state.certificates.iter().position(|candidate| {
        candidate.certificate_type == certificate_type
            && candidate.serial_number.eq_ignore_ascii_case(&serial_number)
    }) {
        let existing = state.certificates.remove(existing_index);
        delete_certificate_files(&existing)?;
        let _ = delete_p12_password(&existing.p12_password_account);
    }

    state.certificates.push(ManagedCertificate {
        id: format!("imported:{serial_number}"),
        certificate_type: certificate_type.to_owned(),
        serial_number: serial_number.clone(),
        origin: CertificateOrigin::Imported,
        display_name,
        system_keychain_path: None,
        system_signing_identity: None,
        private_key_path,
        certificate_der_path,
        p12_path: imported_p12_path,
        p12_password_account: password_account,
    });
    save_state(project, &team_id, &state)?;

    println!("team_id: {team_id}");
    println!("certificate_type: {certificate_type}");
    println!("certificate_serial: {serial_number}");
    println!("p12: {}", args.p12.display());
    Ok(())
}

struct SigningSelection<'a> {
    target: &'a TargetManifest,
    platform: ApplePlatform,
    profile: ProfileManifest,
}

fn resolve_signing_selection<'a>(
    project: &'a ProjectContext,
    requested_platform: Option<ApplePlatform>,
    requested_distribution: Option<DistributionKind>,
) -> Result<SigningSelection<'a>> {
    let platform = resolve_platform(
        project,
        requested_platform,
        "Select a platform to manage signing for",
    )?;
    let target = build_target_for_platform(project, platform)?;
    let distribution = resolve_build_distribution(project, platform, requested_distribution)?;
    let profile = profile_for_distribution(distribution);
    Ok(SigningSelection {
        target,
        platform,
        profile,
    })
}

pub fn prepare_signing(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    profile: &ProfileManifest,
    device_udids: Option<Vec<String>>,
) -> Result<SigningMaterial> {
    if resolve_api_key_auth(&project.app)?.is_some() {
        return prepare_signing_with_api_key(project, target, platform, profile, device_udids);
    }

    let team_id = resolve_local_team_id(project)?;
    let mut provisioning = ProvisioningClient::authenticate(&project.app, team_id.clone())?;
    let mut state = load_state(project, &team_id)?;

    if let Some(host_target) = host_app_for_app_clip(project, target)? {
        let _ = signing_progress_step(
            format!(
                "Ensuring host bundle identifier `{}` for target `{}`",
                host_target.bundle_id, target.name
            ),
            |bundle_id: &ProvisioningBundleId| {
                format!("Host bundle identifier ready: {}.", bundle_id.identifier)
            },
            || ensure_bundle_id_with_developer_services(&mut provisioning, project, host_target),
        )?;
    }

    let bundle_id = signing_progress_step(
        format!(
            "Ensuring bundle identifier `{}` for target `{}`",
            target.bundle_id, target.name
        ),
        |bundle_id: &ProvisioningBundleId| {
            format!(
                "Bundle identifier ready for target `{}`: {}.",
                target.name, bundle_id.identifier
            )
        },
        || ensure_bundle_id_with_developer_services(&mut provisioning, project, target),
    )?;
    let _ = signing_progress_step(
        format!("Syncing capabilities for `{}`", bundle_id.identifier),
        |outcome: &CapabilitySyncOutcome| {
            capability_sync_success_message(&bundle_id.identifier, *outcome)
        },
        || sync_capabilities(&mut provisioning, project, target, &bundle_id),
    )?;

    let certificate = signing_progress_step(
        format!("Ensuring signing certificate for target `{}`", target.name),
        |certificate: &ManagedCertificate| {
            format!(
                "Signing certificate ready for target `{}`: {}.",
                target.name, certificate.serial_number
            )
        },
        || {
            ensure_certificate_for_apple_id(
                &mut provisioning,
                project,
                &mut state,
                platform,
                profile,
            )
        },
    )?;
    let profile_type = asc_profile_type(platform, profile)?;
    let device_ids = if matches!(
        profile.distribution,
        DistributionKind::Development | DistributionKind::AdHoc
    ) {
        let selected_udids = resolve_requested_device_udids(
            project,
            profile.distribution,
            platform,
            DeviceSelectionContext {
                state: &state,
                bundle_identifier: &bundle_id.identifier,
                profile_type,
                certificate_id: &certificate.id,
            },
            device_udids,
        )?;
        signing_progress_step(
            format!(
                "Resolving Apple devices for provisioning profile for target `{}`",
                target.name
            ),
            |device_ids: &Vec<String>| {
                format!(
                    "Resolved {} Apple device(s) for target `{}`.",
                    device_ids.len(),
                    target.name
                )
            },
            || {
                resolve_device_ids_with_developer_services(
                    project,
                    &mut provisioning,
                    platform,
                    &selected_udids,
                )
            },
        )?
    } else {
        Vec::new()
    };
    let provisioning_profile = signing_progress_step(
        format!("Ensuring provisioning profile for target `{}`", target.name),
        |profile: &ManagedProfile| {
            format!(
                "Provisioning profile ready for target `{}`: {}.",
                target.name, profile.id
            )
        },
        || {
            ensure_profile_with_developer_services(
                &mut provisioning,
                project,
                &mut state,
                &ManagedBundleId::from(&bundle_id),
                profile_type,
                &certificate,
                &device_ids,
            )
        },
    )?;
    let signing_identity = signing_progress_step(
        format!(
            "Importing signing certificate into Orbit keychain for target `{}`",
            target.name
        ),
        |identity: &SigningIdentity| {
            format!(
                "Signing identity ready for target `{}`: {}.",
                target.name, identity.hash
            )
        },
        || resolve_signing_identity(project, &certificate),
    )?;
    let entitlements_path = signing_progress_step(
        format!("Preparing entitlements for target `{}`", target.name),
        |path: &Option<PathBuf>| match path {
            Some(path) => format!(
                "Prepared entitlements for target `{}`: {}.",
                target.name,
                path.display()
            ),
            None => format!(
                "No generated entitlements were needed for target `{}`.",
                target.name
            ),
        },
        || materialize_signing_entitlements(project, target, &provisioning_profile.path),
    )?;
    save_state(project, &team_id, &state)?;

    Ok(SigningMaterial {
        signing_identity: signing_identity.hash,
        keychain_path: signing_identity.keychain_path,
        provisioning_profile_path: provisioning_profile.path,
        entitlements_path,
    })
}

pub fn prepare_package_signing(
    project: &ProjectContext,
    profile: &ProfileManifest,
) -> Result<PackageSigningMaterial> {
    if resolve_api_key_auth(&project.app)?.is_some() {
        return prepare_package_signing_with_api_key(project, profile);
    }

    let team_id = resolve_local_team_id(project)?;
    let mut provisioning = ProvisioningClient::authenticate(&project.app, team_id.clone())?;
    let mut state = load_state(project, &team_id)?;
    let certificate_type = installer_certificate_type(profile)?;
    let certificate = signing_progress_step(
        "Ensuring installer signing certificate",
        |certificate: &ManagedCertificate| {
            format!(
                "Installer signing certificate ready: {}.",
                certificate.serial_number
            )
        },
        || {
            ensure_certificate_with_developer_services(
                &mut provisioning,
                project,
                &mut state,
                certificate_type,
                developer_services_installer_certificate_type(profile),
            )?
            .with_context(|| {
                format!(
                    "Developer Services does not support installer certificate type `{certificate_type}`"
                )
            })
        },
    )?;
    let signing_identity = signing_progress_step(
        "Importing installer signing certificate into Orbit keychain",
        |identity: &SigningIdentity| {
            format!("Installer signing identity ready: {}.", identity.hash)
        },
        || resolve_signing_identity(project, &certificate),
    )?;
    save_state(project, &team_id, &state)?;
    Ok(PackageSigningMaterial {
        signing_identity: signing_identity.hash,
        keychain_path: signing_identity.keychain_path,
    })
}

fn prepare_signing_with_api_key(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    profile: &ProfileManifest,
    device_udids: Option<Vec<String>>,
) -> Result<SigningMaterial> {
    let client = AscClient::new(
        resolve_api_key_auth(&project.app)?
            .context("App Store Connect API key auth is not configured")?,
    )?;
    let team_id = resolve_api_key_team_id(project)?;
    let mut state = load_state(project, &team_id)?;

    if let Some(host_target) = host_app_for_app_clip(project, target)? {
        let _ = signing_progress_step(
            format!(
                "Ensuring host bundle identifier `{}` for target `{}`",
                host_target.bundle_id, target.name
            ),
            |bundle_id: &Resource<BundleIdAttributes>| {
                format!(
                    "Host bundle identifier ready: {}.",
                    bundle_id.attributes.identifier
                )
            },
            || ensure_bundle_id_with_api_key(&client, project, host_target, platform),
        )?;
    }

    let bundle_id = signing_progress_step(
        format!(
            "Ensuring bundle identifier `{}` for target `{}`",
            target.bundle_id, target.name
        ),
        |bundle_id: &Resource<BundleIdAttributes>| {
            format!(
                "Bundle identifier ready for target `{}`: {}.",
                target.name, bundle_id.attributes.identifier
            )
        },
        || ensure_bundle_id_with_api_key(&client, project, target, platform),
    )?;
    let _ = signing_progress_step(
        format!(
            "Syncing capabilities for `{}`",
            bundle_id.attributes.identifier
        ),
        |outcome: &CapabilitySyncOutcome| {
            capability_sync_success_message(&bundle_id.attributes.identifier, *outcome)
        },
        || sync_capabilities_with_api_key(&client, project, target, &bundle_id),
    )?;

    let certificate_type = asc_certificate_type(platform, profile)?;
    let certificate = signing_progress_step(
        format!("Ensuring signing certificate for target `{}`", target.name),
        |certificate: &ManagedCertificate| {
            format!(
                "Signing certificate ready for target `{}`: {}.",
                target.name, certificate.serial_number
            )
        },
        || ensure_certificate_with_api_key(&client, project, &mut state, certificate_type),
    )?;
    let profile_type = asc_profile_type(platform, profile)?;
    let device_ids = if matches!(
        profile.distribution,
        DistributionKind::Development | DistributionKind::AdHoc
    ) {
        let selected_udids = resolve_requested_device_udids(
            project,
            profile.distribution,
            platform,
            DeviceSelectionContext {
                state: &state,
                bundle_identifier: &bundle_id.attributes.identifier,
                profile_type,
                certificate_id: &certificate.id,
            },
            device_udids,
        )?;
        signing_progress_step(
            format!(
                "Resolving Apple devices for provisioning profile for target `{}`",
                target.name
            ),
            |device_ids: &Vec<String>| {
                format!(
                    "Resolved {} Apple device(s) for target `{}`.",
                    device_ids.len(),
                    target.name
                )
            },
            || resolve_device_ids_with_api_key(&client, platform, &selected_udids),
        )?
    } else {
        Vec::new()
    };
    let provisioning_profile = signing_progress_step(
        format!("Ensuring provisioning profile for target `{}`", target.name),
        |profile: &ManagedProfile| {
            format!(
                "Provisioning profile ready for target `{}`: {}.",
                target.name, profile.id
            )
        },
        || {
            ensure_profile_with_api_key(
                &client,
                project,
                &mut state,
                &bundle_id,
                profile_type,
                &certificate,
                &device_ids,
            )
        },
    )?;

    let signing_identity = signing_progress_step(
        format!(
            "Importing signing certificate into Orbit keychain for target `{}`",
            target.name
        ),
        |identity: &SigningIdentity| {
            format!(
                "Signing identity ready for target `{}`: {}.",
                target.name, identity.hash
            )
        },
        || resolve_signing_identity(project, &certificate),
    )?;
    let entitlements_path = signing_progress_step(
        format!("Preparing entitlements for target `{}`", target.name),
        |path: &Option<PathBuf>| match path {
            Some(path) => format!(
                "Prepared entitlements for target `{}`: {}.",
                target.name,
                path.display()
            ),
            None => format!(
                "No generated entitlements were needed for target `{}`.",
                target.name
            ),
        },
        || materialize_signing_entitlements(project, target, &provisioning_profile.path),
    )?;
    save_state(project, &team_id, &state)?;

    Ok(SigningMaterial {
        signing_identity: signing_identity.hash,
        keychain_path: signing_identity.keychain_path,
        provisioning_profile_path: provisioning_profile.path,
        entitlements_path,
    })
}

fn prepare_package_signing_with_api_key(
    project: &ProjectContext,
    profile: &ProfileManifest,
) -> Result<PackageSigningMaterial> {
    let client = AscClient::new(
        resolve_api_key_auth(&project.app)?
            .context("App Store Connect API key auth is not configured")?,
    )?;
    let team_id = resolve_api_key_team_id(project)?;
    let mut state = load_state(project, &team_id)?;
    let certificate_type = asc_installer_certificate_type(profile)?;
    let certificate = signing_progress_step(
        "Ensuring installer signing certificate",
        |certificate: &ManagedCertificate| {
            format!(
                "Installer signing certificate ready: {}.",
                certificate.serial_number
            )
        },
        || ensure_certificate_with_api_key(&client, project, &mut state, certificate_type),
    )?;
    let signing_identity = signing_progress_step(
        "Importing installer signing certificate into Orbit keychain",
        |identity: &SigningIdentity| {
            format!("Installer signing identity ready: {}.", identity.hash)
        },
        || resolve_signing_identity(project, &certificate),
    )?;
    save_state(project, &team_id, &state)?;
    Ok(PackageSigningMaterial {
        signing_identity: signing_identity.hash,
        keychain_path: signing_identity.keychain_path,
    })
}

fn developer_services_certificate_type(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<Option<&'static str>> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos
            | ApplePlatform::Macos,
            DistributionKind::Development,
        ) => Ok(Some("DEVELOPMENT")),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos
            | ApplePlatform::Macos,
            DistributionKind::AdHoc | DistributionKind::AppStore | DistributionKind::MacAppStore,
        ) => Ok(Some("DISTRIBUTION")),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => {
            Ok(Some("DEVELOPER_ID_APPLICATION"))
        }
        _ => bail!(
            "signing is not implemented for {platform} with {:?}",
            profile.distribution
        ),
    }
}

fn ensure_certificate_for_apple_id(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    state: &mut SigningState,
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<ManagedCertificate> {
    let certificate_type = certificate_type(platform, profile)?;
    ensure_certificate_with_developer_services(
        provisioning,
        project,
        state,
        certificate_type,
        developer_services_certificate_type(platform, profile)?,
    )?
    .with_context(|| {
        format!("Developer Services does not support certificate type `{certificate_type}`")
    })
}

fn ensure_certificate_with_developer_services(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    state: &mut SigningState,
    certificate_type: &str,
    developer_services_certificate_type: Option<&str>,
) -> Result<Option<ManagedCertificate>> {
    let Some(developer_services_certificate_type) = developer_services_certificate_type else {
        return Ok(None);
    };
    let remote_certificates =
        provisioning.list_certificates(developer_services_certificate_type)?;
    for remote in &remote_certificates {
        if let Some(local) = state.certificates.iter_mut().find(|certificate| {
            if certificate.certificate_type != certificate_type
                || certificate.system_signing_identity.is_some()
                || !certificate.p12_path.exists()
            {
                return false;
            }
            if certificate.id == remote.id {
                return true;
            }
            remote
                .serial_number
                .as_deref()
                .is_some_and(|serial| certificate.serial_number.eq_ignore_ascii_case(serial))
        }) {
            if local.id != remote.id {
                local.id = remote.id.clone();
            }
            if local.display_name.is_none() {
                local.display_name = remote.display_name.clone();
            }
            return Ok(Some(local.clone()));
        }
    }

    let paths = team_signing_paths(project, &resolve_local_team_id(project)?);
    ensure_dir(&paths.certificates_dir)?;
    for remote in &remote_certificates {
        let Some(serial_number) = remote.serial_number.as_deref() else {
            continue;
        };
        if let Some(certificate) = recover_orphaned_certificate(
            &paths,
            state,
            certificate_type,
            &remote.id,
            serial_number,
            remote.display_name.as_deref(),
        )? {
            return Ok(Some(certificate));
        }
    }

    for remote in &remote_certificates {
        if let Some(local) = state.certificates.iter_mut().find(|certificate| {
            if certificate.certificate_type != certificate_type
                || certificate.system_signing_identity.is_none()
                || !certificate_has_local_signing_material(certificate)
            {
                return false;
            }
            if certificate.id == remote.id {
                return true;
            }
            remote
                .serial_number
                .as_deref()
                .is_some_and(|serial| certificate.serial_number.eq_ignore_ascii_case(serial))
        }) {
            if local.id != remote.id {
                local.id = remote.id.clone();
            }
            if local.display_name.is_none() {
                local.display_name = remote.display_name.clone();
            }
            return Ok(Some(local.clone()));
        }
    }

    for remote in &remote_certificates {
        let Some(serial_number) = remote.serial_number.as_deref() else {
            continue;
        };
        if let Some(identity) =
            recover_system_keychain_identity(serial_number, remote.display_name.as_deref())?
        {
            let certificate = ManagedCertificate {
                id: remote.id.clone(),
                certificate_type: certificate_type.to_owned(),
                serial_number: serial_number.to_owned(),
                origin: CertificateOrigin::Generated,
                display_name: remote.display_name.clone(),
                system_keychain_path: Some(identity.keychain_path),
                system_signing_identity: Some(identity.hash),
                private_key_path: paths
                    .certificates_dir
                    .join(format!("{serial_number}.managed.key")),
                certificate_der_path: paths
                    .certificates_dir
                    .join(format!("{serial_number}.managed.cer")),
                p12_path: paths
                    .certificates_dir
                    .join(format!("{serial_number}.managed.p12")),
                p12_password_account: String::new(),
            };
            state.certificates.retain(|candidate| {
                !candidate.serial_number.eq_ignore_ascii_case(serial_number)
                    && candidate.id != remote.id
            });
            state.certificates.push(certificate.clone());
            return Ok(Some(certificate));
        }
    }

    let slug = crate::util::timestamp_slug();
    let private_key_path = paths.certificates_dir.join(format!("{slug}.key.pem"));
    let csr_path = paths.certificates_dir.join(format!("{slug}.csr.pem"));
    let certificate_der_path = paths.certificates_dir.join(format!("{slug}.cer"));
    let p12_path = paths.certificates_dir.join(format!("{slug}.p12"));

    let mut openssl_req = Command::new("openssl");
    openssl_req.args([
        "req",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        private_key_path
            .to_str()
            .context("private key path contains invalid UTF-8")?,
        "-subj",
        &format!("/CN=Orbit {slug}"),
        "-out",
        csr_path
            .to_str()
            .context("CSR path contains invalid UTF-8")?,
    ]);
    crate::util::run_command(&mut openssl_req)?;

    let csr_pem = fs::read_to_string(&csr_path)
        .with_context(|| format!("failed to read {}", csr_path.display()))?;
    let (machine_id, machine_name) =
        developer_services_machine_attributes(developer_services_certificate_type);
    let remote = provisioning.create_certificate(
        developer_services_certificate_type,
        &csr_pem,
        machine_id.as_deref(),
        machine_name.as_deref(),
    )?;
    let certificate_content = remote
        .certificate_content
        .as_deref()
        .context("Developer Services did not return the created certificate content")?;
    let certificate_bytes = base64::engine::general_purpose::STANDARD
        .decode(certificate_content)
        .context("failed to decode Developer Services certificate content")?;
    fs::write(&certificate_der_path, &certificate_bytes)
        .with_context(|| format!("failed to write {}", certificate_der_path.display()))?;

    let p12_password = uuid::Uuid::new_v4().to_string();
    export_p12_from_der_certificate(
        &private_key_path,
        &certificate_der_path,
        &p12_path,
        &p12_password,
    )?;

    let serial_number = match remote.serial_number.clone() {
        Some(serial_number) => serial_number,
        None => read_certificate_serial(&certificate_der_path)?,
    };
    let password_account = format!("{}-{serial_number}", remote.id);
    store_p12_password(&password_account, &p12_password)?;

    let certificate = ManagedCertificate {
        id: remote.id,
        certificate_type: certificate_type.to_owned(),
        serial_number,
        origin: CertificateOrigin::Generated,
        display_name: remote.display_name,
        system_keychain_path: None,
        system_signing_identity: None,
        private_key_path,
        certificate_der_path,
        p12_path,
        p12_password_account: password_account,
    };
    state.certificates.push(certificate.clone());
    Ok(Some(certificate))
}

fn ensure_profile_with_developer_services(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    state: &mut SigningState,
    bundle_id: &ManagedBundleId,
    profile_type: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
) -> Result<ManagedProfile> {
    if let Some(local) = state.profiles.iter().find(|profile| {
        profile.profile_type == profile_type
            && profile.bundle_id == bundle_id.identifier
            && profile.path.exists()
            && profile.certificate_ids == vec![certificate.id.clone()]
            && canonical_ids(&profile.device_ids) == canonical_ids(device_ids)
    }) {
        return Ok(local.clone());
    }

    let remote_profiles = provisioning.list_profiles(Some(profile_type))?;
    let mut remote_profile_ids = HashSet::new();
    let mut stale_orbit_profiles = Vec::new();
    for remote in remote_profiles {
        remote_profile_ids.insert(remote.id.clone());
        if remote.bundle_id_identifier.as_deref() != Some(bundle_id.identifier.as_str()) {
            continue;
        }

        let matches_certificate = remote.certificate_ids.contains(&certificate.id);
        let matches_devices = canonical_ids(&remote.device_ids) == canonical_ids(device_ids);
        if matches_certificate && matches_devices {
            let managed = persist_developer_services_profile(
                project,
                state,
                profile_type,
                &bundle_id.identifier,
                certificate,
                device_ids,
                remote,
            )?;
            cleanup_stale_profile_state(
                state,
                &bundle_id.identifier,
                profile_type,
                &remote_profile_ids,
            );
            return Ok(managed);
        }

        if is_orbit_managed_profile(state, &remote.id, &bundle_id.identifier, profile_type) {
            stale_orbit_profiles.push(remote.id);
        }
    }

    for profile_id in stale_orbit_profiles {
        provisioning
            .delete_profile(&profile_id)
            .with_context(|| format!("failed to repair provisioning profile `{profile_id}`"))?;
        state.profiles.retain(|profile| profile.id != profile_id);
    }
    cleanup_stale_profile_state(
        state,
        &bundle_id.identifier,
        profile_type,
        &remote_profile_ids,
    );

    let remote = provisioning.create_profile(profile_type, &bundle_id.id)?;
    persist_developer_services_profile(
        project,
        state,
        profile_type,
        &bundle_id.identifier,
        certificate,
        device_ids,
        remote,
    )
}

fn persist_developer_services_profile(
    project: &ProjectContext,
    state: &mut SigningState,
    profile_type: &str,
    bundle_identifier: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
    remote: ProvisioningProfile,
) -> Result<ManagedProfile> {
    let paths = team_signing_paths(project, &resolve_local_team_id(project)?);
    ensure_dir(&paths.profiles_dir)?;
    let profile_content = remote
        .profile_content
        .as_deref()
        .context("Developer Services did not return the provisioning profile content")?;
    let profile_bytes = base64::engine::general_purpose::STANDARD
        .decode(profile_content)
        .context("failed to decode Developer Services provisioning profile content")?;
    let profile_path = paths
        .profiles_dir
        .join(format!("{}-{}.mobileprovision", remote.id, profile_type));
    fs::write(&profile_path, profile_bytes)
        .with_context(|| format!("failed to write {}", profile_path.display()))?;

    state.profiles.retain(|profile| profile.id != remote.id);
    let profile = ManagedProfile {
        id: remote.id,
        profile_type: profile_type.to_owned(),
        bundle_id: bundle_identifier.to_owned(),
        path: profile_path,
        uuid: remote.uuid,
        certificate_ids: vec![certificate.id.clone()],
        device_ids: device_ids.to_vec(),
    };
    state.profiles.push(profile.clone());
    Ok(profile)
}

fn resolve_device_ids_with_developer_services(
    _project: &ProjectContext,
    provisioning: &mut ProvisioningClient,
    platform: ApplePlatform,
    udids: &[String],
) -> Result<Vec<String>> {
    let devices = provisioning.list_devices()?;
    if udids.is_empty() {
        return Ok(devices
            .into_iter()
            .filter(|device| provisioning_device_matches_platform(device, platform))
            .map(|device| device.id)
            .collect());
    }

    let mut device_ids = Vec::new();
    for udid in udids {
        let device = devices
            .iter()
            .find(|device| {
                device.udid.eq_ignore_ascii_case(udid)
                    && provisioning_device_matches_platform(device, platform)
            })
            .with_context(|| {
                format!(
                    "device `{udid}` is not registered with Apple; register it first with `orbit apple device register`"
                )
            })?;
        device_ids.push(device.id.clone());
    }
    Ok(device_ids)
}

fn provisioning_device_matches_platform(
    device: &ProvisioningDevice,
    platform: ApplePlatform,
) -> bool {
    asc_device_matches_platform(&device.platform, platform)
}

fn resolve_api_key_team_id(project: &ProjectContext) -> Result<String> {
    resolve_local_team_id_if_known(project)?.context(
        "API key signing state is scoped by Apple team; set `team_id` in orbit.json or export ORBIT_APPLE_TEAM_ID for CI runs",
    )
}

#[derive(Debug, Clone, Copy)]
struct PushConfiguration {
    broadcast: bool,
}

fn push_configuration(target: &TargetManifest) -> Option<PushConfiguration> {
    target.push.as_ref().map(|push| PushConfiguration {
        broadcast: push.broadcast_for_live_activities,
    })
}

fn entitlement_dictionary_for_capability_sync(
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<Dictionary> {
    match &target.entitlements {
        Some(entitlements_path) => load_plist_dictionary(&project.root.join(entitlements_path)),
        None => Ok(Dictionary::new()),
    }
}

fn capability_sync_plan_for_target(
    project: &ProjectContext,
    target: &TargetManifest,
    remote_capabilities: &[RemoteCapability],
    options: &CapabilitySyncOptions,
) -> Result<crate::apple::capabilities::CapabilitySyncPlan> {
    let entitlements = entitlement_dictionary_for_capability_sync(project, target)?;
    capability_sync_plan_from_dictionary_with_options(&entitlements, remote_capabilities, options)
}

fn capability_sync_options_for_target(target: &TargetManifest) -> CapabilitySyncOptions {
    let push = push_configuration(target);
    CapabilitySyncOptions {
        uses_push_notifications: push.is_some(),
        uses_broadcast_push_notifications: push.is_some_and(|push| push.broadcast),
    }
}

fn validate_push_setup_with_api_key(target: &TargetManifest) -> CapabilitySyncOptions {
    let Some(push) = push_configuration(target) else {
        return CapabilitySyncOptions::default();
    };
    if !push.broadcast {
        return CapabilitySyncOptions {
            uses_push_notifications: true,
            uses_broadcast_push_notifications: false,
        };
    }

    eprintln!(
        "warning: App Store Connect API key auth cannot configure broadcast push settings for target `{}`; continuing without broadcast support. Enable Broadcast Push Notifications manually in the Apple developer console if needed.",
        target.name
    );
    CapabilitySyncOptions {
        uses_push_notifications: true,
        uses_broadcast_push_notifications: false,
    }
}

fn ensure_bundle_id_with_api_key(
    client: &AscClient,
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
) -> Result<Resource<BundleIdAttributes>> {
    if let Some(bundle_id) = client
        .find_bundle_id(&target.bundle_id)?
        .map(|document| document.data)
    {
        return Ok(bundle_id);
    }

    client.create_bundle_id(
        &orbit_managed_app_name(&project.resolved_manifest.name),
        &target.bundle_id,
        asc_bundle_id_platform(platform),
    )
}

fn sync_capabilities_with_api_key(
    client: &AscClient,
    project: &ProjectContext,
    target: &TargetManifest,
    bundle_id: &Resource<BundleIdAttributes>,
) -> Result<CapabilitySyncOutcome> {
    let capability_sync_options = validate_push_setup_with_api_key(target);
    if target.entitlements.is_none() && !capability_sync_options.uses_push_notifications {
        return Ok(CapabilitySyncOutcome::Skipped);
    }
    let remote_bundle = client
        .find_bundle_id(&bundle_id.attributes.identifier)?
        .with_context(|| {
            format!(
                "bundle identifier `{}` exists in App Store Connect but could not be reloaded",
                bundle_id.attributes.identifier
            )
        })?;
    let remote_capabilities = remote_capabilities_from_included(&remote_bundle.included)?;
    let plan = capability_sync_plan_for_target(
        project,
        target,
        &remote_capabilities,
        &capability_sync_options,
    )?;
    if plan.updates.is_empty() {
        return Ok(CapabilitySyncOutcome::NoUpdates);
    }

    let mutations = plan_asc_capability_mutations(&plan.updates, &remote_capabilities)?;
    for mutation in mutations {
        if mutation.delete {
            if let Some(remote_id) = mutation.remote_id {
                client.delete_bundle_capability(&remote_id)?;
            }
            continue;
        }

        match mutation.remote_id {
            Some(remote_id) => {
                let _ = client.update_bundle_capability(
                    &remote_id,
                    &mutation.capability_type,
                    &mutation.settings,
                )?;
            }
            None => {
                let _ = client.create_bundle_capability(
                    &bundle_id.id,
                    &mutation.capability_type,
                    &mutation.settings,
                )?;
            }
        }
    }
    Ok(CapabilitySyncOutcome::Updated(plan.updates.len()))
}

fn plan_asc_capability_mutations(
    updates: &[CapabilityUpdate],
    remote_capabilities: &[RemoteCapability],
) -> Result<Vec<AscCapabilityMutation>> {
    let mut mutations = Vec::new();
    for update in updates {
        if !ASC_SUPPORTED_CAPABILITIES.contains(&update.capability_type.as_str()) {
            bail!(
                "App Store Connect API key auth does not support syncing capability `{}`; log in with Apple ID so Orbit can use the Developer Portal flow",
                update.capability_type
            );
        }
        if update.relationships.app_groups.is_some() {
            bail!(
                "App Store Connect API key auth cannot link App Groups for capability `{}`; log in with Apple ID so Orbit can use the Developer Portal flow",
                update.capability_type
            );
        }
        if update.relationships.cloud_containers.is_some() {
            bail!(
                "App Store Connect API key auth cannot link iCloud containers for capability `{}`; log in with Apple ID so Orbit can use the Developer Portal flow",
                update.capability_type
            );
        }
        if update.relationships.merchant_ids.is_some() {
            bail!(
                "App Store Connect API key auth cannot link merchant IDs for capability `{}`; log in with Apple ID so Orbit can use the Developer Portal flow",
                update.capability_type
            );
        }

        let remote_id = remote_capabilities
            .iter()
            .find(|candidate| candidate.capability_type == update.capability_type)
            .map(|candidate| candidate.id.clone());
        if update.option == ASC_OPTION_OFF {
            mutations.push(AscCapabilityMutation {
                remote_id,
                capability_type: update.capability_type.clone(),
                settings: Vec::new(),
                delete: true,
            });
            continue;
        }

        mutations.push(AscCapabilityMutation {
            remote_id,
            capability_type: update.capability_type.clone(),
            settings: asc_capability_settings(update)?,
            delete: false,
        });
    }
    Ok(mutations)
}

fn asc_capability_settings(update: &CapabilityUpdate) -> Result<Vec<CapabilitySetting>> {
    let setting = match update.capability_type.as_str() {
        "ICLOUD" => match update.option.as_str() {
            ASC_OPTION_ON => Some((ASC_SETTING_ICLOUD_VERSION, ASC_OPTION_ICLOUD_XCODE_6)),
            "XCODE_5" | ASC_OPTION_ICLOUD_XCODE_6 => {
                Some((ASC_SETTING_ICLOUD_VERSION, update.option.as_str()))
            }
            other => {
                bail!("App Store Connect API key auth does not support iCloud option `{other}`")
            }
        },
        "DATA_PROTECTION" => match update.option.as_str() {
            ASC_OPTION_ON => Some((
                ASC_SETTING_DATA_PROTECTION,
                ASC_OPTION_DATA_PROTECTION_COMPLETE,
            )),
            ASC_OPTION_DATA_PROTECTION_COMPLETE
            | ASC_OPTION_DATA_PROTECTION_PROTECTED_UNLESS_OPEN
            | ASC_OPTION_DATA_PROTECTION_PROTECTED_UNTIL_FIRST_USER_AUTH => {
                Some((ASC_SETTING_DATA_PROTECTION, update.option.as_str()))
            }
            other => bail!(
                "App Store Connect API key auth does not support data protection option `{other}`"
            ),
        },
        "APPLE_ID_AUTH" => match update.option.as_str() {
            ASC_OPTION_ON => Some((
                ASC_SETTING_APPLE_ID_AUTH,
                ASC_OPTION_APPLE_ID_PRIMARY_CONSENT,
            )),
            ASC_OPTION_APPLE_ID_PRIMARY_CONSENT => {
                Some((ASC_SETTING_APPLE_ID_AUTH, update.option.as_str()))
            }
            other => bail!(
                "App Store Connect API key auth does not support Sign In with Apple option `{other}`"
            ),
        },
        "PUSH_NOTIFICATIONS" if update.option == ASC_OPTION_PUSH_BROADCAST => {
            bail!(
                "App Store Connect API key auth cannot configure broadcast push settings; log in with Apple ID so Orbit can use the Developer Portal flow"
            )
        }
        _ => None,
    };

    Ok(setting
        .into_iter()
        .map(|(key, option)| CapabilitySetting {
            key: key.to_owned(),
            options: vec![CapabilityOption {
                key: option.to_owned(),
                enabled: true,
            }],
        })
        .collect())
}

fn ensure_certificate_with_api_key(
    client: &AscClient,
    project: &ProjectContext,
    state: &mut SigningState,
    certificate_type: &str,
) -> Result<ManagedCertificate> {
    let remote_certificates = client.list_certificates(certificate_type)?;
    for remote in &remote_certificates {
        if let Some(local) = state.certificates.iter_mut().find(|certificate| {
            if certificate.certificate_type != certificate_type
                || !certificate_has_local_signing_material(certificate)
            {
                return false;
            }
            if certificate.id == remote.id {
                return true;
            }
            remote
                .attributes
                .serial_number
                .as_deref()
                .is_some_and(|serial| certificate.serial_number.eq_ignore_ascii_case(serial))
        }) {
            if local.id != remote.id {
                local.id = remote.id.clone();
            }
            if local.display_name.is_none() {
                local.display_name = remote.attributes.display_name.clone();
            }
            return Ok(local.clone());
        }
    }

    let paths = team_signing_paths(project, &resolve_api_key_team_id(project)?);
    ensure_dir(&paths.certificates_dir)?;
    for remote in &remote_certificates {
        let Some(serial_number) = remote.attributes.serial_number.as_deref() else {
            continue;
        };
        if let Some(certificate) = recover_orphaned_certificate(
            &paths,
            state,
            certificate_type,
            &remote.id,
            serial_number,
            remote.attributes.display_name.as_deref(),
        )? {
            return Ok(certificate);
        }
    }

    let slug = crate::util::timestamp_slug();
    let private_key_path = paths.certificates_dir.join(format!("{slug}.key.pem"));
    let csr_path = paths.certificates_dir.join(format!("{slug}.csr.pem"));
    let certificate_der_path = paths.certificates_dir.join(format!("{slug}.cer"));
    let p12_path = paths.certificates_dir.join(format!("{slug}.p12"));

    let mut openssl_req = Command::new("openssl");
    openssl_req.args([
        "req",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        private_key_path
            .to_str()
            .context("private key path contains invalid UTF-8")?,
        "-subj",
        &format!("/CN=Orbit {slug}"),
        "-out",
        csr_path
            .to_str()
            .context("CSR path contains invalid UTF-8")?,
    ]);
    crate::util::run_command(&mut openssl_req)?;

    let csr_pem = fs::read_to_string(&csr_path)
        .with_context(|| format!("failed to read {}", csr_path.display()))?;
    let remote = client.create_certificate(certificate_type, &csr_pem)?;
    let certificate_content = remote
        .attributes
        .certificate_content
        .as_deref()
        .context("App Store Connect did not return the created certificate content")?;
    let certificate_bytes = base64::engine::general_purpose::STANDARD
        .decode(certificate_content)
        .context("failed to decode App Store Connect certificate content")?;
    fs::write(&certificate_der_path, &certificate_bytes)
        .with_context(|| format!("failed to write {}", certificate_der_path.display()))?;

    let p12_password = uuid::Uuid::new_v4().to_string();
    export_p12_from_der_certificate(
        &private_key_path,
        &certificate_der_path,
        &p12_path,
        &p12_password,
    )?;

    let serial_number = match remote.attributes.serial_number.clone() {
        Some(serial_number) => serial_number,
        None => read_certificate_serial(&certificate_der_path)?,
    };
    let password_account = format!("{}-{serial_number}", remote.id);
    store_p12_password(&password_account, &p12_password)?;

    let certificate = ManagedCertificate {
        id: remote.id,
        certificate_type: certificate_type.to_owned(),
        serial_number,
        origin: CertificateOrigin::Generated,
        display_name: remote.attributes.display_name.clone(),
        system_keychain_path: None,
        system_signing_identity: None,
        private_key_path,
        certificate_der_path,
        p12_path,
        p12_password_account: password_account,
    };
    state.certificates.push(certificate.clone());
    Ok(certificate)
}

fn ensure_profile_with_api_key(
    client: &AscClient,
    project: &ProjectContext,
    state: &mut SigningState,
    bundle_id: &Resource<BundleIdAttributes>,
    profile_type: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
) -> Result<ManagedProfile> {
    let profiles = client.list_profiles(profile_type)?;
    let bundle_identifier = &bundle_id.attributes.identifier;
    let mut remote_profile_ids = HashSet::new();
    let mut stale_orbit_profiles = Vec::new();

    for profile in profiles.data {
        remote_profile_ids.insert(profile.id.clone());
        let bundle_link = profile
            .relationships
            .get("bundleId")
            .and_then(|relationship| relationship.data.as_ref())
            .and_then(|relationship| match relationship {
                crate::apple::asc_api::RelationshipData::One(link) => Some(link.id.clone()),
                crate::apple::asc_api::RelationshipData::Many(_) => None,
            });
        if bundle_link.as_deref() != Some(bundle_id.id.as_str()) {
            continue;
        }

        let certificate_links = profile
            .relationships
            .get("certificates")
            .and_then(|relationship| relationship.data.as_ref())
            .map(|relationship| match relationship {
                crate::apple::asc_api::RelationshipData::Many(links) => {
                    links.iter().map(|link| link.id.clone()).collect::<Vec<_>>()
                }
                crate::apple::asc_api::RelationshipData::One(link) => vec![link.id.clone()],
            })
            .unwrap_or_default();
        let device_links = profile
            .relationships
            .get("devices")
            .and_then(|relationship| relationship.data.as_ref())
            .map(|relationship| match relationship {
                crate::apple::asc_api::RelationshipData::Many(links) => {
                    links.iter().map(|link| link.id.clone()).collect::<Vec<_>>()
                }
                crate::apple::asc_api::RelationshipData::One(link) => vec![link.id.clone()],
            })
            .unwrap_or_default();

        let matches_certificate = certificate_links.contains(&certificate.id);
        let matches_devices = canonical_ids(&device_links) == canonical_ids(device_ids);
        if matches_certificate && matches_devices {
            let managed = persist_asc_profile(
                project,
                state,
                profile_type,
                bundle_identifier,
                certificate,
                &device_links,
                profile,
            )?;
            cleanup_stale_profile_state(
                state,
                bundle_identifier,
                profile_type,
                &remote_profile_ids,
            );
            return Ok(managed);
        }

        if is_orbit_managed_profile(state, &profile.id, bundle_identifier, profile_type) {
            stale_orbit_profiles.push(profile.id.clone());
        }
    }

    for profile_id in stale_orbit_profiles {
        client
            .delete_profile(&profile_id)
            .with_context(|| format!("failed to repair provisioning profile `{profile_id}`"))?;
        state.profiles.retain(|profile| profile.id != profile_id);
    }
    cleanup_stale_profile_state(state, bundle_identifier, profile_type, &remote_profile_ids);

    let remote = client.create_profile(
        &format!(
            "*[orbit] {} {} {}",
            bundle_identifier,
            profile_type,
            crate::util::timestamp_slug()
        ),
        profile_type,
        &bundle_id.id,
        std::slice::from_ref(&certificate.id),
        device_ids,
    )?;
    persist_asc_profile(
        project,
        state,
        profile_type,
        bundle_identifier,
        certificate,
        device_ids,
        remote,
    )
}

fn persist_asc_profile(
    project: &ProjectContext,
    state: &mut SigningState,
    profile_type: &str,
    bundle_identifier: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
    remote: Resource<crate::apple::asc_api::ProfileAttributes>,
) -> Result<ManagedProfile> {
    let paths = team_signing_paths(project, &resolve_api_key_team_id(project)?);
    ensure_dir(&paths.profiles_dir)?;
    let profile_content = remote
        .attributes
        .profile_content
        .as_deref()
        .context("App Store Connect did not return the provisioning profile content")?;
    let profile_bytes = base64::engine::general_purpose::STANDARD
        .decode(profile_content)
        .context("failed to decode App Store Connect provisioning profile content")?;
    let profile_path = paths
        .profiles_dir
        .join(format!("{}-{}.mobileprovision", remote.id, profile_type));
    fs::write(&profile_path, profile_bytes)
        .with_context(|| format!("failed to write {}", profile_path.display()))?;

    state.profiles.retain(|profile| profile.id != remote.id);
    let profile = ManagedProfile {
        id: remote.id,
        profile_type: profile_type.to_owned(),
        bundle_id: bundle_identifier.to_owned(),
        path: profile_path,
        uuid: remote.attributes.uuid.clone(),
        certificate_ids: vec![certificate.id.clone()],
        device_ids: device_ids.to_vec(),
    };
    state.profiles.push(profile.clone());
    Ok(profile)
}

fn resolve_device_ids_with_api_key(
    client: &AscClient,
    platform: ApplePlatform,
    udids: &[String],
) -> Result<Vec<String>> {
    let devices = client.list_devices()?;
    if udids.is_empty() {
        return Ok(devices
            .into_iter()
            .filter(|device| asc_device_matches_platform(&device.attributes.platform, platform))
            .map(|device| device.id)
            .collect());
    }

    let mut device_ids = Vec::new();
    for udid in udids {
        let device = client
            .find_device_by_udid(udid)?
            .filter(|device| asc_device_matches_platform(&device.attributes.platform, platform))
            .with_context(|| format!("device `{udid}` is not registered with App Store Connect"))?;
        device_ids.push(device.id);
    }
    Ok(device_ids)
}

fn asc_bundle_id_platform(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Macos => "MAC_OS",
        ApplePlatform::Ios
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => "IOS",
    }
}

fn asc_device_matches_platform(device_platform: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios => device_platform == "IOS",
        ApplePlatform::Tvos => device_platform == "TVOS",
        ApplePlatform::Visionos => device_platform == "VISIONOS",
        ApplePlatform::Watchos => device_platform == "WATCHOS",
        ApplePlatform::Macos => device_platform == "MAC_OS",
    }
}

fn host_app_for_app_clip<'a>(
    project: &'a ProjectContext,
    target: &'a TargetManifest,
) -> Result<Option<&'a TargetManifest>> {
    if !is_app_clip_target(project, target)? {
        return Ok(None);
    }

    let mut hosts = project
        .resolved_manifest
        .targets
        .iter()
        .filter(|candidate| {
            candidate.name != target.name
                && candidate.kind == crate::manifest::TargetKind::App
                && candidate
                    .dependencies
                    .iter()
                    .any(|dependency| dependency == &target.name)
        })
        .collect::<Vec<_>>();
    match hosts.len() {
        0 => Ok(None),
        1 => Ok(hosts.pop()),
        _ => bail!(
            "App Clip target `{}` cannot be hosted by more than one app target",
            target.name
        ),
    }
}

fn hosted_app_clip_targets<'a>(
    project: &'a ProjectContext,
    target: &'a TargetManifest,
) -> Result<Vec<&'a TargetManifest>> {
    let mut hosted = Vec::new();
    for dependency_name in &target.dependencies {
        let dependency = project
            .resolved_manifest
            .resolve_target(Some(dependency_name))?;
        if is_app_clip_target(project, dependency)? {
            hosted.push(dependency);
        }
    }
    Ok(hosted)
}

fn is_app_clip_target(project: &ProjectContext, target: &TargetManifest) -> Result<bool> {
    if target.kind != crate::manifest::TargetKind::App {
        return Ok(false);
    }
    Ok(target_parent_application_identifiers(project, target)?.is_some())
}

pub fn target_is_app_clip(project: &ProjectContext, target: &TargetManifest) -> Result<bool> {
    is_app_clip_target(project, target)
}

fn target_parent_application_identifiers(
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<Option<Vec<String>>> {
    let Some(entitlements_path) = &target.entitlements else {
        return Ok(None);
    };
    let entitlements = load_plist_dictionary(&project.root.join(entitlements_path))?;
    parent_application_identifiers_from_dictionary(&entitlements)
}

fn parent_application_identifiers_from_dictionary(
    dictionary: &Dictionary,
) -> Result<Option<Vec<String>>> {
    let Some(value) = dictionary.get("com.apple.developer.parent-application-identifiers") else {
        return Ok(None);
    };
    let values = string_array_value("com.apple.developer.parent-application-identifiers", value)?;
    if values.len() != 1 {
        bail!(
            "`com.apple.developer.parent-application-identifiers` must contain exactly one application identifier"
        );
    }
    Ok(Some(values))
}

fn materialize_signing_entitlements(
    project: &ProjectContext,
    target: &TargetManifest,
    provisioning_profile_path: &Path,
) -> Result<Option<PathBuf>> {
    let original_path = target
        .entitlements
        .as_ref()
        .map(|path| project.root.join(path));
    let profile_entitlements = provisioning_profile_entitlements(provisioning_profile_path)?;
    let mut entitlements = match &original_path {
        Some(path) => load_plist_dictionary(path)?,
        None => profile_entitlements.clone(),
    };
    let application_identifier_prefix =
        provisioning_profile_application_identifier_prefix(provisioning_profile_path)?;
    let mut changed = original_path.is_none();
    changed |= replace_entitlement_placeholders_in_dictionary(
        &mut entitlements,
        &application_identifier_prefix,
    );

    if let Some(parent_identifiers) = parent_application_identifiers_from_dictionary(&entitlements)?
    {
        let Some(host_target) = host_app_for_app_clip(project, target)? else {
            bail!(
                "App Clip target `{}` must be hosted by an app target in the manifest",
                target.name
            );
        };
        let expected_parent = format!("{application_identifier_prefix}{}", host_target.bundle_id);
        if parent_identifiers[0] != expected_parent {
            bail!(
                "App Clip target `{}` must reference its host app application identifier `{expected_parent}`",
                target.name
            );
        }
        if !target
            .bundle_id
            .starts_with(&format!("{}.", host_target.bundle_id))
        {
            bail!(
                "App Clip target `{}` bundle ID `{}` must use the host app bundle ID `{}` as its prefix",
                target.name,
                target.bundle_id,
                host_target.bundle_id
            );
        }
        changed |= set_dictionary_boolean(
            &mut entitlements,
            "com.apple.developer.on-demand-install-capable",
            true,
        );
    }

    let hosted_app_clip_identifiers = hosted_app_clip_targets(project, target)?
        .into_iter()
        .map(|hosted_target| format!("{application_identifier_prefix}{}", hosted_target.bundle_id))
        .collect::<Vec<_>>();
    if !hosted_app_clip_identifiers.is_empty() {
        if hosted_app_clip_identifiers.len() != 1 {
            bail!(
                "app target `{}` cannot host more than one App Clip because `com.apple.developer.associated-appclip-app-identifiers` must contain exactly one entry",
                target.name
            );
        }
        changed |= set_dictionary_string_array(
            &mut entitlements,
            "com.apple.developer.associated-appclip-app-identifiers",
            hosted_app_clip_identifiers,
        );
    }

    changed |= merge_managed_signing_entitlements(&mut entitlements, &profile_entitlements);

    if !changed {
        return Ok(original_path);
    }

    let generated_dir = project
        .project_paths
        .orbit_dir
        .join("signing")
        .join("entitlements");
    ensure_dir(&generated_dir)?;
    let path = generated_dir.join(format!("{}.entitlements", target.name));
    Value::Dictionary(entitlements)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(Some(path))
}

fn load_plist_dictionary(path: &Path) -> Result<Dictionary> {
    Value::from_file(path)
        .with_context(|| format!("failed to parse plist {}", path.display()))?
        .into_dictionary()
        .context("plist must contain a top-level dictionary")
}

fn provisioning_profile_entitlements(path: &Path) -> Result<Dictionary> {
    Ok(load_provisioning_profile_dictionary(path)?
        .get("Entitlements")
        .and_then(Value::as_dictionary)
        .cloned()
        .unwrap_or_default())
}

fn provisioning_profile_application_identifier_prefix(path: &Path) -> Result<String> {
    let profile = load_provisioning_profile_dictionary(path)?;
    if let Some(prefixes) = profile
        .get("ApplicationIdentifierPrefix")
        .and_then(Value::as_array)
        && let Some(prefix) = prefixes.first().and_then(Value::as_string)
    {
        return Ok(normalize_application_identifier_prefix(prefix));
    }

    let application_identifier = profile
        .get("Entitlements")
        .and_then(Value::as_dictionary)
        .and_then(|entitlements| entitlements.get("application-identifier"))
        .and_then(Value::as_string)
        .context("provisioning profile is missing an application identifier prefix")?;
    let prefix = application_identifier
        .split_once('.')
        .map(|(prefix, _)| prefix)
        .unwrap_or(application_identifier);
    Ok(normalize_application_identifier_prefix(prefix))
}

fn load_provisioning_profile_dictionary(path: &Path) -> Result<Dictionary> {
    if let Ok(value) = Value::from_file(path)
        && let Some(dictionary) = value.into_dictionary()
    {
        return Ok(dictionary);
    }

    let output =
        crate::util::command_output(Command::new("security").args(["cms", "-D", "-i"]).arg(path))?;
    Value::from_reader_xml(output.as_bytes())
        .context("failed to decode provisioning profile CMS payload")?
        .into_dictionary()
        .context("decoded provisioning profile did not contain a top-level dictionary")
}

fn normalize_application_identifier_prefix(prefix: &str) -> String {
    if prefix.ends_with('.') {
        prefix.to_owned()
    } else {
        format!("{prefix}.")
    }
}

fn replace_entitlement_placeholders_in_dictionary(
    dictionary: &mut Dictionary,
    application_identifier_prefix: &str,
) -> bool {
    let mut changed = false;
    for value in dictionary.values_mut() {
        changed |= replace_entitlement_placeholders_in_value(value, application_identifier_prefix);
    }
    changed
}

fn replace_entitlement_placeholders_in_value(
    value: &mut Value,
    application_identifier_prefix: &str,
) -> bool {
    match value {
        Value::Array(values) => values.iter_mut().any(|value| {
            replace_entitlement_placeholders_in_value(value, application_identifier_prefix)
        }),
        Value::Dictionary(dictionary) => replace_entitlement_placeholders_in_dictionary(
            dictionary,
            application_identifier_prefix,
        ),
        Value::String(text) => {
            let replaced = text
                .replace(
                    APP_IDENTIFIER_PREFIX_PLACEHOLDER,
                    application_identifier_prefix,
                )
                .replace(
                    TEAM_IDENTIFIER_PREFIX_PLACEHOLDER,
                    application_identifier_prefix,
                );
            if replaced != *text {
                *text = replaced;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

fn set_dictionary_boolean(dictionary: &mut Dictionary, key: &str, value: bool) -> bool {
    let next = Value::Boolean(value);
    if dictionary.get(key) == Some(&next) {
        return false;
    }
    dictionary.insert(key.to_owned(), next);
    true
}

fn set_dictionary_string_array(
    dictionary: &mut Dictionary,
    key: &str,
    values: Vec<String>,
) -> bool {
    let next = Value::Array(values.into_iter().map(Value::String).collect());
    if dictionary.get(key) == Some(&next) {
        return false;
    }
    dictionary.insert(key.to_owned(), next);
    true
}

fn merge_managed_signing_entitlements(
    target: &mut Dictionary,
    profile_entitlements: &Dictionary,
) -> bool {
    let mut changed = false;
    for key in MANAGED_SIGNING_ENTITLEMENTS {
        let Some(value) = profile_entitlements.get(key) else {
            continue;
        };
        if target.get(key) == Some(value) {
            continue;
        }
        target.insert((*key).to_owned(), value.clone());
        changed = true;
    }
    changed
}

fn string_array_value(key: &str, value: &Value) -> Result<Vec<String>> {
    let Some(values) = value.as_array() else {
        bail!("`{key}` must be an array");
    };
    values
        .iter()
        .map(|value| {
            value
                .as_string()
                .map(ToOwned::to_owned)
                .with_context(|| format!("`{key}` must contain only strings"))
        })
        .collect()
}

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

fn ensure_bundle_id_with_developer_services(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<ProvisioningBundleId> {
    provisioning.ensure_bundle_id(
        &orbit_managed_app_name(&project.resolved_manifest.name),
        &target.bundle_id,
    )
}

fn sync_capabilities(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    target: &TargetManifest,
    bundle_id: &ProvisioningBundleId,
) -> Result<CapabilitySyncOutcome> {
    let capability_sync_options = capability_sync_options_for_target(target);
    if target.entitlements.is_none() && !capability_sync_options.uses_push_notifications {
        return Ok(CapabilitySyncOutcome::Skipped);
    }
    let plan = capability_sync_plan_for_target(
        project,
        target,
        &bundle_id.capabilities,
        &capability_sync_options,
    )?;
    if plan.updates.is_empty() {
        return Ok(CapabilitySyncOutcome::NoUpdates);
    }

    let app_group_ids = resolve_app_group_ids(
        provisioning,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.app_groups.as_ref()
        }),
    )?;
    let merchant_identifiers = collect_identifier_values(&plan.updates, |relationships| {
        relationships.merchant_ids.as_ref()
    });
    let cloud_container_identifiers = collect_identifier_values(&plan.updates, |relationships| {
        relationships.cloud_containers.as_ref()
    });
    let merchant_ids = resolve_merchant_ids(provisioning, merchant_identifiers)?;
    let cloud_container_ids =
        resolve_cloud_container_ids(provisioning, cloud_container_identifiers)?;
    let updates = plan
        .updates
        .iter()
        .map(|update| {
            let remote_id = bundle_id
                .capabilities
                .iter()
                .find(|candidate| candidate.capability_type == update.capability_type)
                .map(|candidate| candidate.id.clone());
            Ok(ProvisioningCapabilityUpdate {
                remote_id,
                capability_type: update.capability_type.clone(),
                option: update.option.clone(),
                relationships: ProvisioningCapabilityRelationships {
                    app_groups: map_relationship_ids(
                        update.relationships.app_groups.as_ref(),
                        &app_group_ids,
                    )?,
                    merchant_ids: map_relationship_ids(
                        update.relationships.merchant_ids.as_ref(),
                        &merchant_ids,
                    )?,
                    cloud_containers: map_relationship_ids(
                        update.relationships.cloud_containers.as_ref(),
                        &cloud_container_ids,
                    )?,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let (deletes, upserts): (Vec<_>, Vec<_>) = updates
        .into_iter()
        .partition(|update| update.option == ASC_OPTION_OFF);
    for delete in deletes {
        if delete.capability_type == "ASSOCIATED_DOMAINS" {
            // Xcode does not emit a matching disable mutation when users remove
            // Associated Domains in Signing & Capabilities, so keep the remote
            // capability untouched to stay aligned with Apple tooling.
            continue;
        }
        if let Some(remote_id) = delete.remote_id.as_deref() {
            provisioning.delete_bundle_capability(remote_id)?;
        }
    }
    if !upserts.is_empty() {
        provisioning.update_bundle_capabilities(bundle_id, &upserts)?;
    }
    Ok(CapabilitySyncOutcome::Updated(plan.updates.len()))
}

fn collect_identifier_values<F>(updates: &[CapabilityUpdate], select: F) -> Vec<String>
where
    F: Fn(&CapabilityRelationships) -> Option<&Vec<String>>,
{
    let mut values = updates
        .iter()
        .flat_map(|update| {
            select(&update.relationships)
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn developer_services_machine_attributes(
    certificate_type: &str,
) -> (Option<String>, Option<String>) {
    if certificate_type != "DEVELOPMENT" {
        return (None, None);
    }

    let output = match crate::util::command_output(
        Command::new("system_profiler").args(["-json", "SPHardwareDataType"]),
    ) {
        Ok(output) => output,
        Err(_) => return (None, None),
    };
    let value: serde_json::Value = match serde_json::from_str(&output) {
        Ok(value) => value,
        Err(_) => return (None, None),
    };
    let Some(entry) = value
        .get("SPHardwareDataType")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.first())
    else {
        return (None, None);
    };
    let machine_id = entry
        .get("provisioning_UDID")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let machine_name = entry
        .get("_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    (machine_id, machine_name)
}

fn resolve_app_group_ids(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = provisioning.list_app_groups()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_group) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_group.id.clone()
        } else {
            let name = identifier_name("App Group", &identifier);
            provisioning.create_app_group(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn resolve_merchant_ids(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = provisioning.list_merchant_ids()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_merchant) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_merchant.id.clone()
        } else {
            let name = identifier_name("Merchant ID", &identifier);
            provisioning.create_merchant_id(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn resolve_cloud_container_ids(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = provisioning.list_cloud_containers()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_container) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_container.id.clone()
        } else {
            let name = identifier_name("iCloud Container", &identifier);
            provisioning.create_cloud_container(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn map_relationship_ids(
    identifiers: Option<&Vec<String>>,
    resolved: &HashMap<String, String>,
) -> Result<Option<Vec<String>>> {
    let Some(identifiers) = identifiers else {
        return Ok(None);
    };
    identifiers
        .iter()
        .map(|identifier| {
            resolved
                .get(identifier)
                .cloned()
                .with_context(|| format!("missing Apple identifier record for `{identifier}`"))
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn resolve_requested_device_udids(
    project: &ProjectContext,
    distribution: DistributionKind,
    platform: ApplePlatform,
    context: DeviceSelectionContext<'_>,
    explicit_udids: Option<Vec<String>>,
) -> Result<Vec<String>> {
    if let Some(explicit_udids) = explicit_udids.filter(|udids| !udids.is_empty()) {
        return Ok(explicit_udids);
    }

    select_device_udids(
        project,
        distribution,
        platform,
        current_profile_for_target(
            context.state,
            context.bundle_identifier,
            context.profile_type,
            context.certificate_id,
        ),
    )
}

fn select_device_udids(
    project: &ProjectContext,
    distribution: DistributionKind,
    platform: ApplePlatform,
    current_profile: Option<&ManagedProfile>,
) -> Result<Vec<String>> {
    let devices = ensure_registered_devices(project, platform)?;
    if devices.is_empty() {
        bail!(
            "no registered Apple devices found for {platform}; run `orbit apple device register` first"
        );
    }

    if !project.app.interactive {
        return Ok(devices.into_iter().map(|device| device.udid).collect());
    }

    if matches!(distribution, DistributionKind::AdHoc) {
        if let Some(current_profile) = current_profile {
            let provisioned_udids = profile_udids(current_profile, &devices);
            if !provisioned_udids.is_empty() {
                let registered_udids = devices
                    .iter()
                    .map(|device| device.udid.clone())
                    .collect::<Vec<_>>();
                if same_udid_set(&registered_udids, &provisioned_udids) {
                    match prompt_ad_hoc_profile_reuse(&provisioned_udids, &devices)? {
                        AdHocProfileReuse::ReuseCurrent => return Ok(provisioned_udids),
                        AdHocProfileReuse::ChooseAgain => {}
                    }
                } else {
                    let missing = missing_registered_devices(&devices, &provisioned_udids);
                    if !missing.is_empty() {
                        println!(
                            "warning: the current ad-hoc provisioning profile is missing the following devices:"
                        );
                        for device in &missing {
                            println!("- {}", format_cached_device_label(device));
                        }
                        if !prompt_confirm(
                            "Would you like to choose the devices to provision again?",
                            true,
                        )? {
                            return Ok(provisioned_udids);
                        }
                    }
                }
            }
        }
        return prompt_ad_hoc_device_selection(&devices, current_profile);
    }

    let labels = devices
        .iter()
        .map(format_cached_device_label)
        .collect::<Vec<_>>();
    let index = prompt_select("Select a device to provision", &labels)?;
    Ok(vec![devices[index].udid.clone()])
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum AdHocProfileReuse {
    ReuseCurrent,
    ChooseAgain,
}

fn prompt_ad_hoc_profile_reuse(
    provisioned_udids: &[String],
    devices: &[crate::apple::device::CachedDevice],
) -> Result<AdHocProfileReuse> {
    loop {
        let options = [
            "Yes",
            "Show devices and ask me again",
            "No, let me choose devices again",
        ];
        match prompt_select(
            "All your registered devices are present in the provisioning profile. Would you like to reuse it?",
            &options,
        )? {
            0 => return Ok(AdHocProfileReuse::ReuseCurrent),
            1 => {
                println!("Devices registered in the provisioning profile:");
                for device in devices {
                    if provisioned_udids.iter().any(|udid| udid == &device.udid) {
                        println!("- {}", format_cached_device_label(device));
                    }
                }
            }
            2 => return Ok(AdHocProfileReuse::ChooseAgain),
            _ => unreachable!("prompt_select returned an out-of-range index"),
        }
    }
}

fn prompt_ad_hoc_device_selection(
    devices: &[crate::apple::device::CachedDevice],
    current_profile: Option<&ManagedProfile>,
) -> Result<Vec<String>> {
    let preselected_udids = current_profile
        .map(|profile| profile_udids(profile, devices))
        .unwrap_or_default();
    let defaults = if preselected_udids.is_empty() {
        vec![true; devices.len()]
    } else {
        devices
            .iter()
            .map(|device| preselected_udids.iter().any(|udid| udid == &device.udid))
            .collect::<Vec<_>>()
    };
    let labels = devices
        .iter()
        .map(format_cached_device_label)
        .collect::<Vec<_>>();
    let selections = prompt_multi_select(
        "Select devices for the ad-hoc build",
        &labels,
        Some(&defaults),
    )?;
    if selections.is_empty() {
        bail!("select at least one device for an ad-hoc provisioning profile");
    }
    Ok(selections
        .into_iter()
        .map(|index| devices[index].udid.clone())
        .collect())
}

fn ensure_registered_devices(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<Vec<crate::apple::device::CachedDevice>> {
    loop {
        let cache = crate::apple::device::refresh_cache(&project.app)?;
        let devices = cache
            .devices
            .into_iter()
            .filter(|device| device_matches_platform(&device.platform, platform))
            .collect::<Vec<_>>();
        if !devices.is_empty() {
            return Ok(devices);
        }
        if !project.app.interactive {
            return Ok(devices);
        }
        if !prompt_confirm(
            &format!(
                "You don't have any registered Apple devices for {platform}. Would you like to register one now?"
            ),
            true,
        )? {
            return Ok(devices);
        }
        crate::apple::device::register_device(
            &project.app,
            &crate::cli::RegisterDeviceArgs {
                name: None,
                udid: None,
                platform: match platform {
                    ApplePlatform::Macos => crate::cli::DevicePlatform::MacOs,
                    _ => crate::cli::DevicePlatform::Ios,
                },
                current_machine: matches!(platform, ApplePlatform::Macos),
            },
        )?;
    }
}

fn current_profile_for_target<'a>(
    state: &'a SigningState,
    bundle_identifier: &str,
    profile_type: &str,
    certificate_id: &str,
) -> Option<&'a ManagedProfile> {
    state.profiles.iter().rev().find(|profile| {
        profile.bundle_id == bundle_identifier
            && profile.profile_type == profile_type
            && profile.path.exists()
            && profile
                .certificate_ids
                .iter()
                .any(|candidate| candidate == certificate_id)
    })
}

fn profile_udids(
    profile: &ManagedProfile,
    devices: &[crate::apple::device::CachedDevice],
) -> Vec<String> {
    devices
        .iter()
        .filter(|device| profile.device_ids.iter().any(|id| id == &device.id))
        .map(|device| device.udid.clone())
        .collect()
}

fn same_udid_set(left: &[String], right: &[String]) -> bool {
    canonical_ids(left) == canonical_ids(right)
}

fn missing_registered_devices<'a>(
    devices: &'a [crate::apple::device::CachedDevice],
    provisioned_udids: &[String],
) -> Vec<&'a crate::apple::device::CachedDevice> {
    devices
        .iter()
        .filter(|device| !provisioned_udids.iter().any(|udid| udid == &device.udid))
        .collect()
}

fn format_cached_device_label(device: &crate::apple::device::CachedDevice) -> String {
    let details = format_cached_device_details(device);
    let mut label = device.udid.clone();
    if !details.is_empty() {
        label.push(' ');
        label.push_str(&details);
    }
    if !device.name.is_empty() {
        label.push_str(&format!(" ({})", device.name));
    }
    if let Some(created_at) = device.created_at.as_deref() {
        label.push_str(&format!(" (created at: {created_at})"));
    }
    label
}

fn format_cached_device_details(device: &crate::apple::device::CachedDevice) -> String {
    let mut details = Vec::new();
    if let Some(device_class) = device.device_class.as_deref() {
        details.push(device_class.replace('_', " "));
    }
    if let Some(model) = device.model.as_deref()
        && details.iter().all(|detail| detail != model)
    {
        details.push(model.to_owned());
    }
    if details.is_empty() {
        String::new()
    } else {
        format!("({})", details.join(" "))
    }
}

fn device_matches_platform(device_platform: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios | ApplePlatform::Visionos => device_platform == "IOS",
        ApplePlatform::Tvos => device_platform == "TVOS",
        ApplePlatform::Watchos => device_platform == "WATCH" || device_platform == "WATCHOS",
        ApplePlatform::Macos => device_platform == "MAC_OS" || device_platform == "UNIVERSAL",
    }
}

fn is_orbit_managed_profile(
    state: &SigningState,
    profile_id: &str,
    bundle_identifier: &str,
    profile_type: &str,
) -> bool {
    state.profiles.iter().any(|profile| {
        profile.id == profile_id
            && profile.bundle_id == bundle_identifier
            && profile.profile_type == profile_type
    })
}

fn cleanup_stale_profile_state(
    state: &mut SigningState,
    bundle_identifier: &str,
    profile_type: &str,
    remote_profile_ids: &HashSet<String>,
) {
    state.profiles.retain(|profile| {
        if profile.bundle_id != bundle_identifier || profile.profile_type != profile_type {
            return true;
        }
        remote_profile_ids.contains(&profile.id)
    });
}

fn certificate_type(platform: ApplePlatform, profile: &ProfileManifest) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("83Q87W3TGH"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AdHoc | DistributionKind::AppStore,
        ) => Ok("WXV89964HE"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("749Y1QAGU7"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("HXZEUKP0FP"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("W0EURJRMC5"),
        _ => bail!(
            "signing is not implemented for {platform} with {:?}",
            profile.distribution
        ),
    }
}

fn asc_certificate_type(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("IOS_DEVELOPMENT"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AdHoc | DistributionKind::AppStore,
        ) => Ok("IOS_DISTRIBUTION"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("MAC_APP_DEVELOPMENT"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("MAC_APP_DISTRIBUTION"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("DEVELOPER_ID_APPLICATION"),
        _ => bail!(
            "App Store Connect API key auth does not support signing for {platform} with {:?}",
            profile.distribution
        ),
    }
}

fn profile_type(platform: ApplePlatform, profile: &ProfileManifest) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("limited"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AdHoc,
        ) => Ok("adhoc"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AppStore,
        ) => Ok("store"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("limited"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("store"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("direct"),
        _ => bail!("provisioning profiles are not implemented for {platform}"),
    }
}

fn asc_profile_type(platform: ApplePlatform, profile: &ProfileManifest) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios | ApplePlatform::Visionos | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("IOS_APP_DEVELOPMENT"),
        (
            ApplePlatform::Ios | ApplePlatform::Visionos | ApplePlatform::Watchos,
            DistributionKind::AdHoc,
        ) => Ok("IOS_APP_ADHOC"),
        (
            ApplePlatform::Ios | ApplePlatform::Visionos | ApplePlatform::Watchos,
            DistributionKind::AppStore,
        ) => Ok("IOS_APP_STORE"),
        (ApplePlatform::Tvos, DistributionKind::Development) => Ok("TVOS_APP_DEVELOPMENT"),
        (ApplePlatform::Tvos, DistributionKind::AdHoc) => Ok("TVOS_APP_ADHOC"),
        (ApplePlatform::Tvos, DistributionKind::AppStore) => Ok("TVOS_APP_STORE"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("MAC_APP_DEVELOPMENT"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("MAC_APP_STORE"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("MAC_APP_DIRECT"),
        _ => bail!(
            "App Store Connect API key auth does not support provisioning profiles for {platform} with {:?}",
            profile.distribution
        ),
    }
}

fn installer_certificate_type(profile: &ProfileManifest) -> Result<&'static str> {
    match profile.distribution {
        DistributionKind::MacAppStore => Ok("2PQI8IDXNH"),
        DistributionKind::DeveloperId => Ok("OYVN2GW35E"),
        _ => bail!(
            "installer signing is not implemented for {:?}",
            profile.distribution
        ),
    }
}

fn asc_installer_certificate_type(profile: &ProfileManifest) -> Result<&'static str> {
    match profile.distribution {
        DistributionKind::MacAppStore => Ok("MAC_INSTALLER_DISTRIBUTION"),
        DistributionKind::DeveloperId => bail!(
            "App Store Connect API key auth does not expose a Developer ID installer certificate type yet; use Apple ID so Orbit can use Developer Services cloud-managed certificates"
        ),
        _ => bail!(
            "installer signing is not implemented for {:?}",
            profile.distribution
        ),
    }
}

fn developer_services_installer_certificate_type(
    profile: &ProfileManifest,
) -> Option<&'static str> {
    match profile.distribution {
        DistributionKind::MacAppStore => Some("MAC_INSTALLER_DISTRIBUTION"),
        DistributionKind::DeveloperId => Some("DEVELOPER_ID_INSTALLER"),
        _ => None,
    }
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

fn read_certificate_serial(path: &Path) -> Result<String> {
    read_certificate_serial_with_format(path, Some("DER"))
}

fn read_certificate_serial_pem(path: &Path) -> Result<String> {
    read_certificate_serial_with_format(path, None)
}

fn read_certificate_serial_with_format(path: &Path, inform: Option<&str>) -> Result<String> {
    let mut command = Command::new("openssl");
    command.arg("x509");
    if let Some(inform) = inform {
        command.args(["-inform", inform]);
    }
    command.args([
        "-in",
        path.to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-noout",
        "-serial",
    ]);
    let output = crate::util::command_output(&mut command)?;
    output
        .trim()
        .strip_prefix("serial=")
        .map(ToOwned::to_owned)
        .context("openssl did not return a certificate serial number")
}

fn team_signing_paths(project: &ProjectContext, team_id: &str) -> TeamSigningPaths {
    let team_dir = project
        .app
        .global_paths
        .data_dir
        .join("teams")
        .join(team_id);
    TeamSigningPaths {
        state_path: team_dir.join("signing.json"),
        certificates_dir: team_dir.join("certificates"),
        profiles_dir: team_dir.join("profiles"),
    }
}

fn resolve_local_team_id_if_known(project: &ProjectContext) -> Result<Option<String>> {
    Ok(std::env::var("ORBIT_APPLE_TEAM_ID")
        .ok()
        .or_else(|| std::env::var("EXPO_APPLE_TEAM_ID").ok())
        .or_else(|| project.resolved_manifest.team_id.clone())
        .or_else(|| {
            resolve_user_auth_metadata(&project.app)
                .ok()
                .flatten()
                .and_then(|user| user.team_id)
        }))
}

fn resolve_local_team_id(project: &ProjectContext) -> Result<String> {
    resolve_local_team_id_if_known(project)?.context(
        "signing state is scoped by Apple team; set `team_id` in orbit.json, export ORBIT_APPLE_TEAM_ID, or log in once so Orbit can persist the team selection",
    )
}

fn load_state(project: &ProjectContext, team_id: &str) -> Result<SigningState> {
    let paths = team_signing_paths(project, team_id);
    Ok(read_json_file_if_exists(&paths.state_path)?.unwrap_or_default())
}

fn save_state(project: &ProjectContext, team_id: &str, state: &SigningState) -> Result<()> {
    let paths = team_signing_paths(project, team_id);
    write_json_file(&paths.state_path, state)
}

pub fn clean_local_signing_state(project: &ProjectContext) -> Result<LocalSigningCleanSummary> {
    let Some(team_id) = resolve_local_team_id_if_known(project)? else {
        return Ok(LocalSigningCleanSummary::default());
    };
    let mut state = load_state(project, &team_id)?;
    let bundle_ids = project_bundle_ids(project);
    let mut removed_profile_cert_ids = HashSet::new();
    let mut removed_profiles = 0usize;

    state.profiles.retain(|profile| {
        if !bundle_ids.contains(&profile.bundle_id) {
            return true;
        }
        let _ = delete_file_if_exists(&profile.path);
        removed_profile_cert_ids.extend(profile.certificate_ids.iter().cloned());
        removed_profiles += 1;
        false
    });

    let remaining_certificate_ids = state
        .profiles
        .iter()
        .flat_map(|profile| profile.certificate_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let mut removed_certificates = 0usize;
    state.certificates.retain(|certificate| {
        if !removed_profile_cert_ids.contains(&certificate.id)
            || remaining_certificate_ids.contains(&certificate.id)
        {
            return true;
        }
        let _ = delete_certificate_files(certificate);
        let _ = delete_p12_password(&certificate.p12_password_account);
        removed_certificates += 1;
        false
    });

    save_state(project, &team_id, &state)?;
    Ok(LocalSigningCleanSummary {
        removed_profiles,
        removed_certificates,
    })
}

pub fn clean_remote_signing_state(project: &ProjectContext) -> Result<RemoteSigningCleanSummary> {
    let team_id = resolve_local_team_id(project)?;
    let mut provisioning = ProvisioningClient::authenticate(&project.app, team_id.clone())?;
    let state = load_state(project, &team_id)?;
    let bundle_ids = project_bundle_ids(project);
    let orbit_app_name = orbit_managed_app_name(&project.resolved_manifest.name);
    let mut summary = RemoteSigningCleanSummary::default();

    let stored_project_profile_ids = state
        .profiles
        .iter()
        .filter(|profile| bundle_ids.contains(&profile.bundle_id))
        .map(|profile| profile.id.clone())
        .collect::<HashSet<_>>();
    remove_orbit_managed_profiles(
        &mut provisioning,
        &bundle_ids,
        &stored_project_profile_ids,
        &mut summary,
    )?;
    remove_orbit_managed_bundle_ids(&mut provisioning, project, &orbit_app_name, &mut summary)?;

    let ProjectEntitlementIdentifiers {
        app_groups: entitlement_app_groups,
        merchant_ids,
        cloud_containers,
    } = project_entitlement_identifiers(project)?;
    remove_orbit_managed_app_groups(&mut provisioning, entitlement_app_groups, &mut summary)?;
    remove_orbit_managed_merchants(&mut provisioning, merchant_ids, &mut summary)?;
    remove_orbit_managed_cloud_containers(&mut provisioning, cloud_containers, &mut summary)?;

    Ok(summary)
}

fn remove_orbit_managed_profiles(
    provisioning: &mut ProvisioningClient,
    bundle_ids: &HashSet<String>,
    stored_project_profile_ids: &HashSet<String>,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    for profile in provisioning.list_profiles(None)? {
        let Some(bundle_identifier) = profile.bundle_id_identifier.as_deref() else {
            continue;
        };
        if !bundle_ids.contains(bundle_identifier) {
            continue;
        }
        if stored_project_profile_ids.contains(&profile.id) || profile.name.starts_with("*[orbit] ")
        {
            provisioning.delete_profile(&profile.id)?;
            summary.removed_profiles += 1;
        }
    }
    Ok(())
}

fn remove_orbit_managed_bundle_ids(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    orbit_app_name: &str,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    for target in &project.resolved_manifest.targets {
        if let Some(bundle_id) = provisioning.find_bundle_id(&target.bundle_id)?
            && bundle_id.name == orbit_app_name
        {
            provisioning.delete_bundle_id(&bundle_id.id)?;
            summary.removed_apps += 1;
        }
    }
    Ok(())
}

fn remove_orbit_managed_app_groups(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    let app_groups = provisioning.list_app_groups()?;
    for identifier in identifiers {
        if let Some(group) = app_groups.iter().find(|group| {
            group.identifier == identifier
                && group.name == identifier_name("App Group", &identifier)
        }) {
            provisioning.delete_app_group(&group.id)?;
            summary.removed_app_groups += 1;
        }
    }
    Ok(())
}

fn remove_orbit_managed_merchants(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    let merchants = provisioning.list_merchant_ids()?;
    for identifier in identifiers {
        if let Some(merchant) = merchants.iter().find(|merchant| {
            merchant.identifier == identifier
                && merchant.name == identifier_name("Merchant ID", &identifier)
        }) {
            provisioning.delete_merchant_id(&merchant.id)?;
            summary.removed_merchants += 1;
        }
    }
    Ok(())
}

fn remove_orbit_managed_cloud_containers(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    let containers = provisioning.list_cloud_containers()?;
    for identifier in identifiers {
        if let Some(container) = containers.iter().find(|container| {
            container.identifier == identifier
                && container.name == identifier_name("iCloud Container", &identifier)
        }) {
            provisioning.delete_cloud_container(&container.id)?;
            summary.removed_cloud_containers += 1;
        }
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ProjectEntitlementIdentifiers {
    app_groups: Vec<String>,
    merchant_ids: Vec<String>,
    cloud_containers: Vec<String>,
}

fn project_bundle_ids(project: &ProjectContext) -> HashSet<String> {
    project
        .resolved_manifest
        .targets
        .iter()
        .map(|target| target.bundle_id.clone())
        .collect()
}

fn project_entitlement_identifiers(
    project: &ProjectContext,
) -> Result<ProjectEntitlementIdentifiers> {
    let mut app_groups = HashSet::new();
    let mut merchant_ids = HashSet::new();
    let mut cloud_containers = HashSet::new();

    for target in &project.resolved_manifest.targets {
        let Some(entitlements_path) = &target.entitlements else {
            continue;
        };
        let plan =
            capability_sync_plan_from_entitlements(&project.root.join(entitlements_path), &[])?;
        app_groups.extend(collect_identifier_values(&plan.updates, |relationships| {
            relationships.app_groups.as_ref()
        }));
        cloud_containers.extend(collect_identifier_values(&plan.updates, |relationships| {
            relationships.cloud_containers.as_ref()
        }));
        merchant_ids.extend(collect_identifier_values(&plan.updates, |relationships| {
            relationships.merchant_ids.as_ref()
        }));
    }

    Ok(ProjectEntitlementIdentifiers {
        app_groups: sorted_strings(app_groups),
        merchant_ids: sorted_strings(merchant_ids),
        cloud_containers: sorted_strings(cloud_containers),
    })
}

fn sorted_strings(values: HashSet<String>) -> Vec<String> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values
}

fn canonical_ids(ids: &[String]) -> Vec<String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids
}

fn store_p12_password(account: &str, password: &str) -> Result<()> {
    let mut command = Command::new("security");
    command.args([
        "add-generic-password",
        "-U",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
        "-w",
        password,
    ]);
    crate::util::command_output(&mut command).map(|_| ())
}

fn load_p12_password(account: &str) -> Result<String> {
    let mut command = Command::new("security");
    command.args([
        "find-generic-password",
        "-w",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
    ]);
    crate::util::command_output(&mut command).map(|value| value.trim().to_owned())
}

fn delete_p12_password(account: &str) -> Result<()> {
    let mut command = Command::new("security");
    command.args([
        "delete-generic-password",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
    ]);
    crate::util::command_output(&mut command).map(|_| ())
}

fn extract_private_key_from_p12(p12_path: &Path, output_path: &Path, password: &str) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "pkcs12",
        "-in",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-nodes",
        "-nocerts",
        "-out",
        output_path
            .to_str()
            .context("private key output path contains invalid UTF-8")?,
        "-passin",
        &format!("pass:{password}"),
    ]);
    crate::util::run_command(&mut command)
}

fn extract_certificate_from_p12(p12_path: &Path, output_path: &Path, password: &str) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "pkcs12",
        "-in",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-clcerts",
        "-nokeys",
        "-out",
        output_path
            .to_str()
            .context("certificate output path contains invalid UTF-8")?,
        "-passin",
        &format!("pass:{password}"),
    ]);
    crate::util::run_command(&mut command)
}

fn export_certificate_der(certificate_pem_path: &Path, output_path: &Path) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "x509",
        "-in",
        certificate_pem_path
            .to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-outform",
        "DER",
        "-out",
        output_path
            .to_str()
            .context("certificate output path contains invalid UTF-8")?,
    ]);
    crate::util::run_command(&mut command)
}

fn export_p12_from_der_certificate(
    private_key_path: &Path,
    certificate_der_path: &Path,
    output_path: &Path,
    password: &str,
) -> Result<()> {
    let certificate_pem = NamedTempFile::new()?;

    let mut decode = Command::new("openssl");
    decode.args([
        "x509",
        "-inform",
        "DER",
        "-in",
        certificate_der_path
            .to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-out",
        certificate_pem
            .path()
            .to_str()
            .context("temporary certificate path contains invalid UTF-8")?,
    ]);
    crate::util::run_command(&mut decode)?;

    let mut export = Command::new("openssl");
    export.args([
        "pkcs12",
        "-legacy",
        "-export",
        "-inkey",
        private_key_path
            .to_str()
            .context("private key path contains invalid UTF-8")?,
        "-in",
        certificate_pem
            .path()
            .to_str()
            .context("temporary certificate path contains invalid UTF-8")?,
        "-out",
        output_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-passout",
        &format!("pass:{password}"),
    ]);
    crate::util::run_command(&mut export)
}

fn recover_orphaned_certificate(
    paths: &TeamSigningPaths,
    state: &mut SigningState,
    certificate_type: &str,
    remote_id: &str,
    serial_number: &str,
    display_name: Option<&str>,
) -> Result<Option<ManagedCertificate>> {
    for entry in fs::read_dir(&paths.certificates_dir)
        .with_context(|| format!("failed to read {}", paths.certificates_dir.display()))?
    {
        let entry = entry?;
        let certificate_der_path = entry.path();
        if certificate_der_path
            .extension()
            .and_then(|value| value.to_str())
            != Some("cer")
        {
            continue;
        }

        let local_serial_number = read_certificate_serial(&certificate_der_path)?;
        if !local_serial_number.eq_ignore_ascii_case(serial_number) {
            continue;
        }

        let Some(stem) = certificate_der_path
            .file_stem()
            .and_then(|value| value.to_str())
        else {
            continue;
        };
        let private_key_path = paths.certificates_dir.join(format!("{stem}.key.pem"));
        if !private_key_path.exists() {
            continue;
        }

        let p12_path = paths.certificates_dir.join(format!("{stem}.p12"));
        let p12_password = uuid::Uuid::new_v4().to_string();
        export_p12_from_der_certificate(
            &private_key_path,
            &certificate_der_path,
            &p12_path,
            &p12_password,
        )?;

        let password_account = format!("{remote_id}-{serial_number}");
        store_p12_password(&password_account, &p12_password)?;

        state.certificates.retain(|candidate| {
            let matches_remote = candidate.id == remote_id
                || candidate.serial_number.eq_ignore_ascii_case(serial_number);
            if matches_remote {
                let _ = delete_p12_password(&candidate.p12_password_account);
            }
            !matches_remote
        });

        let certificate = ManagedCertificate {
            id: remote_id.to_owned(),
            certificate_type: certificate_type.to_owned(),
            serial_number: serial_number.to_owned(),
            origin: CertificateOrigin::Generated,
            display_name: display_name.map(ToOwned::to_owned),
            system_keychain_path: None,
            system_signing_identity: None,
            private_key_path,
            certificate_der_path,
            p12_path,
            p12_password_account: password_account,
        };
        state.certificates.push(certificate.clone());
        return Ok(Some(certificate));
    }

    Ok(None)
}

fn read_certificate_common_name(path: &Path) -> Result<Option<String>> {
    read_certificate_common_name_with_format(path, None)
}

fn read_der_certificate_common_name(path: &Path) -> Result<Option<String>> {
    read_certificate_common_name_with_format(path, Some("DER"))
}

fn read_certificate_common_name_with_format(
    path: &Path,
    inform: Option<&str>,
) -> Result<Option<String>> {
    let mut command = Command::new("openssl");
    command.arg("x509");
    if let Some(inform) = inform {
        command.args(["-inform", inform]);
    }
    command.args([
        "-in",
        path.to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-noout",
        "-subject",
    ]);
    let output = crate::util::command_output(&mut command)?;
    Ok(parse_certificate_common_name(output.trim()))
}

fn parse_certificate_common_name(subject: &str) -> Option<String> {
    subject
        .split(',')
        .find_map(|segment| {
            let segment = segment.trim();
            segment
                .strip_prefix("subject=")
                .unwrap_or(segment)
                .trim()
                .strip_prefix("CN = ")
                .or_else(|| segment.trim().strip_prefix("CN="))
                .map(ToOwned::to_owned)
        })
        .filter(|value| !value.is_empty())
}

fn parse_codesigning_identity_line(line: &str) -> Option<(String, String)> {
    let quote_start = line.find('"')?;
    let quote_end = line[quote_start + 1..].find('"')?;
    let name = line[quote_start + 1..quote_start + 1 + quote_end].to_owned();
    let hash = line.split_whitespace().nth(1)?.trim_matches('"').to_owned();
    Some((hash, name))
}

fn keychain_identities(keychain_path: &str, policy: &str) -> Result<Vec<(String, String)>> {
    let mut find_identity = Command::new("security");
    find_identity.args(["find-identity", "-v", "-p", policy, keychain_path]);
    let output = crate::util::command_output(&mut find_identity)?;
    Ok(output
        .lines()
        .filter_map(parse_codesigning_identity_line)
        .collect())
}

fn user_keychain_paths() -> Result<Vec<PathBuf>> {
    let mut command = Command::new("security");
    command.args(["list-keychains", "-d", "user"]);
    let output = crate::util::command_output(&mut command)?;
    let mut keychains = output
        .lines()
        .map(|line| PathBuf::from(line.trim().trim_matches('"')))
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    if keychains.is_empty() {
        keychains.push(PathBuf::from("login.keychain-db"));
    }
    Ok(keychains)
}

fn keychain_certificate_records(keychain_path: &str) -> Result<Vec<(String, String)>> {
    let mut command = Command::new("security");
    command.args(["find-certificate", "-a", "-Z", "-p", keychain_path]);
    let output = crate::util::command_output(&mut command)?;
    let mut records = Vec::new();
    let mut current_sha1 = None::<String>;
    let mut current_pem = Vec::new();
    let mut in_pem = false;
    for line in output.lines() {
        if let Some(hash) = line.strip_prefix("SHA-1 hash: ") {
            current_sha1 = Some(hash.trim().to_owned());
            continue;
        }
        if line == "-----BEGIN CERTIFICATE-----" {
            in_pem = true;
            current_pem.clear();
        }
        if in_pem {
            current_pem.push(line.to_owned());
            if line == "-----END CERTIFICATE-----" {
                if let Some(hash) = current_sha1.take() {
                    records.push((hash, current_pem.join("\n")));
                }
                current_pem.clear();
                in_pem = false;
            }
        }
    }
    Ok(records)
}

fn recover_system_keychain_identity(
    serial_number: &str,
    display_name: Option<&str>,
) -> Result<Option<SigningIdentity>> {
    for keychain_path in user_keychain_paths()? {
        let keychain_str = keychain_path
            .to_str()
            .context("keychain path contains invalid UTF-8")?;
        let mut identities = HashMap::new();
        for policy in ["codesigning", "basic"] {
            for (hash, name) in keychain_identities(keychain_str, policy)? {
                identities.entry(hash).or_insert(name);
            }
        }
        if identities.is_empty() {
            continue;
        }

        for (hash, pem) in keychain_certificate_records(keychain_str)? {
            let Some(identity_name) = identities.get(&hash) else {
                continue;
            };
            let temp = NamedTempFile::new()?;
            fs::write(temp.path(), pem.as_bytes())
                .with_context(|| format!("failed to write {}", temp.path().display()))?;
            let local_serial = read_certificate_serial_pem(temp.path())?;
            if !local_serial.eq_ignore_ascii_case(serial_number) {
                continue;
            }
            if let Some(display_name) = display_name
                && !identity_name.contains(display_name)
            {
                let local_common_name = read_certificate_common_name(temp.path())?;
                if !local_common_name
                    .as_deref()
                    .is_some_and(|common_name| common_name.contains(display_name))
                {
                    continue;
                }
            }
            return Ok(Some(SigningIdentity {
                hash,
                keychain_path: keychain_path.clone(),
            }));
        }
    }
    Ok(None)
}

fn delete_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn delete_certificate_files(certificate: &ManagedCertificate) -> Result<()> {
    delete_file_if_exists(&certificate.private_key_path)?;
    delete_file_if_exists(&certificate.certificate_der_path)?;
    delete_file_if_exists(&certificate.p12_path)
}

fn import_p12_into_keychain(p12_path: &Path, keychain_path: &str, password: &str) -> Result<()> {
    let mut import = Command::new("security");
    import.args([
        "import",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-k",
        keychain_path,
        "-P",
        password,
        "-T",
        "/usr/bin/codesign",
        "-T",
        "/usr/bin/productbuild",
        "-T",
        "/usr/bin/productsign",
        "-T",
        "/usr/bin/security",
    ]);
    run_command_capture(&mut import).map(|_| ())
}

fn ensure_keychain_in_search_list(keychain_path: &str) -> Result<()> {
    let mut list = Command::new("security");
    list.args(["list-keychains", "-d", "user"]);
    let output = crate::util::command_output(&mut list)?;
    let existing = output
        .lines()
        .map(|line| line.trim().trim_matches('"'))
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if existing.iter().any(|candidate| candidate == keychain_path) {
        return Ok(());
    }

    let mut update = Command::new("security");
    update.args(["list-keychains", "-d", "user", "-s", keychain_path]);
    for candidate in &existing {
        update.arg(candidate);
    }
    crate::util::run_command(&mut update)
}

fn resolve_signing_identity(
    project: &ProjectContext,
    certificate: &ManagedCertificate,
) -> Result<SigningIdentity> {
    if let (Some(hash), Some(keychain_path)) = (
        certificate.system_signing_identity.as_ref(),
        certificate.system_keychain_path.as_ref(),
    ) {
        return Ok(SigningIdentity {
            hash: hash.clone(),
            keychain_path: keychain_path.clone(),
        });
    }

    let keychain_path = &project.app.global_paths.keychain_path;
    if !keychain_path.exists() {
        let mut create = Command::new("security");
        create.args([
            "create-keychain",
            "-p",
            "",
            keychain_path
                .to_str()
                .context("keychain path contains invalid UTF-8")?,
        ]);
        crate::util::run_command(&mut create)?;
    }

    let keychain_str = keychain_path
        .to_str()
        .context("keychain path contains invalid UTF-8")?;
    let mut unlock = Command::new("security");
    unlock.args(["unlock-keychain", "-p", "", keychain_str]);
    let _ = crate::util::run_command(&mut unlock);

    let mut settings = Command::new("security");
    settings.args(["set-keychain-settings", "-lut", "21600", keychain_str]);
    let _ = crate::util::run_command(&mut settings);
    ensure_keychain_in_search_list(keychain_str)?;

    let p12_password = load_p12_password(&certificate.p12_password_account)?;
    if let Err(error) = import_p12_into_keychain(&certificate.p12_path, keychain_str, &p12_password)
    {
        if !certificate.private_key_path.exists() || !certificate.certificate_der_path.exists() {
            return Err(error);
        }

        let repaired_password = uuid::Uuid::new_v4().to_string();
        // Re-export with macOS-compatible PKCS#12 settings so `security import` can read it.
        export_p12_from_der_certificate(
            &certificate.private_key_path,
            &certificate.certificate_der_path,
            &certificate.p12_path,
            &repaired_password,
        )
        .context("failed to repair local P12 for codesigning import")?;
        store_p12_password(&certificate.p12_password_account, &repaired_password)?;
        import_p12_into_keychain(&certificate.p12_path, keychain_str, &repaired_password)
            .context("failed to import repaired codesigning certificate into Orbit keychain")?;
    }

    let mut partition = Command::new("security");
    partition.args([
        "set-key-partition-list",
        "-S",
        "apple-tool:,apple:",
        "-s",
        "-k",
        "",
        keychain_str,
    ]);
    let _ = crate::util::command_output_allow_failure(&mut partition);

    let expected_common_name = read_der_certificate_common_name(&certificate.certificate_der_path)?
        .or_else(|| certificate.display_name.clone());
    for policy in ["codesigning", "basic"] {
        let identities = keychain_identities(keychain_str, policy)?;
        if let Some(expected_common_name) = expected_common_name.as_ref()
            && let Some((hash, _)) = identities
                .iter()
                .find(|(_, name)| name == expected_common_name)
        {
            return Ok(SigningIdentity {
                hash: hash.clone(),
                keychain_path: keychain_path.clone(),
            });
        }

        if let [identity] = identities.as_slice() {
            return Ok(SigningIdentity {
                hash: identity.0.clone(),
                keychain_path: keychain_path.clone(),
            });
        }
    }

    bail!(
        "failed to resolve imported signing identity for certificate {}",
        certificate.id
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use plist::Value;
    use tempfile::TempDir;

    use super::{
        ASC_OPTION_APPLE_ID_PRIMARY_CONSENT, ASC_OPTION_DATA_PROTECTION_COMPLETE,
        ASC_OPTION_PUSH_BROADCAST, CertificateOrigin, ManagedCertificate, ManagedProfile,
        ProfileManifest, ProjectEntitlementIdentifiers, SigningState, asc_capability_settings,
        asc_profile_type, clean_local_signing_state, current_profile_for_target,
        format_cached_device_label, load_state, materialize_signing_entitlements,
        missing_registered_devices, orbit_managed_app_name, parse_certificate_common_name,
        parse_codesigning_identity_line, plan_asc_capability_mutations, profile_udids,
        project_entitlement_identifiers, same_udid_set, save_state, target_is_app_clip,
        team_signing_paths, validate_push_setup_with_api_key,
    };
    use crate::apple::capabilities::{CapabilityRelationships, CapabilityUpdate, RemoteCapability};
    use crate::apple::device::CachedDevice;
    use crate::context::{AppContext, GlobalPaths, ProjectContext, ProjectPaths};
    use crate::manifest::{
        ApplePlatform, BuildConfiguration, DistributionKind, ManifestSchema, PlatformManifest,
        ResolvedManifest, TargetKind, TargetManifest,
    };

    fn test_project() -> (TempDir, ProjectContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let data_dir = temp.path().join("data");
        let cache_dir = temp.path().join("cache");
        let orbit_dir = root.join(".orbit");
        let build_dir = orbit_dir.join("build");
        let artifacts_dir = orbit_dir.join("artifacts");
        let receipts_dir = orbit_dir.join("receipts");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        std::fs::create_dir_all(&receipts_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manifest = ResolvedManifest {
            name: "OrbitFixture".to_owned(),
            version: "0.1.0".to_owned(),
            team_id: Some("TEAM123456".to_owned()),
            provider_id: None,
            platforms: BTreeMap::from([(
                ApplePlatform::Ios,
                PlatformManifest {
                    deployment_target: "18.0".to_owned(),
                },
            )]),
            targets: vec![TargetManifest {
                name: "ExampleApp".to_owned(),
                kind: TargetKind::App,
                bundle_id: "dev.orbit.fixture".to_owned(),
                display_name: None,
                build_number: None,
                platforms: vec![ApplePlatform::Ios],
                sources: vec![root.join("Sources/App")],
                resources: Vec::new(),
                dependencies: Vec::new(),
                frameworks: Vec::new(),
                weak_frameworks: Vec::new(),
                system_libraries: Vec::new(),
                xcframeworks: Vec::new(),
                swift_packages: Vec::new(),
                info_plist: BTreeMap::new(),
                ios: None,
                entitlements: None,
                push: None,
                extension: None,
            }],
        };
        let manifest_path = root.join("orbit.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let app = AppContext {
            cwd: root.clone(),
            interactive: false,
            global_paths: GlobalPaths {
                data_dir: data_dir.clone(),
                cache_dir,
                auth_state_path: data_dir.join("auth.json"),
                device_cache_path: data_dir.join("devices.json"),
                keychain_path: data_dir.join("orbit.keychain-db"),
            },
        };
        let project = ProjectContext {
            app,
            root: root.clone(),
            manifest_path,
            manifest_schema: ManifestSchema::AppleAppV1,
            resolved_manifest: manifest,
            project_paths: ProjectPaths {
                orbit_dir,
                build_dir,
                artifacts_dir,
                receipts_dir,
            },
        };
        (temp, project)
    }

    #[test]
    fn local_cleanup_removes_only_current_project_profiles_and_unused_certs() {
        let (_temp, project) = test_project();
        let team_id = "TEAM123456";
        let team_paths = team_signing_paths(&project, team_id);
        std::fs::create_dir_all(&team_paths.certificates_dir).unwrap();
        std::fs::create_dir_all(&team_paths.profiles_dir).unwrap();

        let current_profile_path = team_paths.profiles_dir.join("current.mobileprovision");
        let other_profile_path = team_paths.profiles_dir.join("other.mobileprovision");
        let current_key_path = team_paths.certificates_dir.join("current.key.pem");
        let current_cer_path = team_paths.certificates_dir.join("current.cer");
        let current_p12_path = team_paths.certificates_dir.join("current.p12");
        let other_key_path = team_paths.certificates_dir.join("other.key.pem");
        let other_cer_path = team_paths.certificates_dir.join("other.cer");
        let other_p12_path = team_paths.certificates_dir.join("other.p12");
        for path in [
            &current_profile_path,
            &other_profile_path,
            &current_key_path,
            &current_cer_path,
            &current_p12_path,
            &other_key_path,
            &other_cer_path,
            &other_p12_path,
        ] {
            std::fs::write(path, b"fixture").unwrap();
        }

        let state = SigningState {
            certificates: vec![
                ManagedCertificate {
                    id: "CERT-CURRENT".to_owned(),
                    certificate_type: "83Q87W3TGH".to_owned(),
                    serial_number: "CURRENT".to_owned(),
                    origin: CertificateOrigin::Generated,
                    display_name: None,
                    system_keychain_path: None,
                    system_signing_identity: None,
                    private_key_path: current_key_path.clone(),
                    certificate_der_path: current_cer_path.clone(),
                    p12_path: current_p12_path.clone(),
                    p12_password_account: "current-password".to_owned(),
                },
                ManagedCertificate {
                    id: "CERT-OTHER".to_owned(),
                    certificate_type: "83Q87W3TGH".to_owned(),
                    serial_number: "OTHER".to_owned(),
                    origin: CertificateOrigin::Generated,
                    display_name: None,
                    system_keychain_path: None,
                    system_signing_identity: None,
                    private_key_path: other_key_path.clone(),
                    certificate_der_path: other_cer_path.clone(),
                    p12_path: other_p12_path.clone(),
                    p12_password_account: "other-password".to_owned(),
                },
            ],
            profiles: vec![
                ManagedProfile {
                    id: "PROFILE-CURRENT".to_owned(),
                    profile_type: "limited".to_owned(),
                    bundle_id: "dev.orbit.fixture".to_owned(),
                    path: current_profile_path.clone(),
                    uuid: None,
                    certificate_ids: vec!["CERT-CURRENT".to_owned()],
                    device_ids: Vec::new(),
                },
                ManagedProfile {
                    id: "PROFILE-OTHER".to_owned(),
                    profile_type: "limited".to_owned(),
                    bundle_id: "dev.orbit.other".to_owned(),
                    path: other_profile_path.clone(),
                    uuid: None,
                    certificate_ids: vec!["CERT-OTHER".to_owned()],
                    device_ids: Vec::new(),
                },
            ],
        };
        save_state(&project, team_id, &state).unwrap();

        let summary = clean_local_signing_state(&project).unwrap();
        assert_eq!(summary.removed_profiles, 1);
        assert_eq!(summary.removed_certificates, 1);
        assert!(!current_profile_path.exists());
        assert!(!current_p12_path.exists());
        assert!(other_profile_path.exists());
        assert!(other_p12_path.exists());

        let cleaned = load_state(&project, team_id).unwrap();
        assert_eq!(cleaned.profiles.len(), 1);
        assert_eq!(cleaned.profiles[0].id, "PROFILE-OTHER");
        assert_eq!(cleaned.certificates.len(), 1);
        assert_eq!(cleaned.certificates[0].id, "CERT-OTHER");
    }

    #[test]
    fn collects_project_identifier_cleanup_inputs_from_entitlements() {
        let (_temp, mut project) = test_project();
        let entitlements_path = project.root.join("App.entitlements");
        let entitlements = Value::Dictionary(plist::Dictionary::from_iter([
            (
                "com.apple.security.application-groups".to_owned(),
                Value::Array(vec![Value::String("group.dev.orbit.fixture".to_owned())]),
            ),
            (
                "com.apple.developer.in-app-payments".to_owned(),
                Value::Array(vec![Value::String("merchant.dev.orbit.fixture".to_owned())]),
            ),
            (
                "com.apple.developer.icloud-container-identifiers".to_owned(),
                Value::Array(vec![Value::String("iCloud.dev.orbit.fixture".to_owned())]),
            ),
        ]));
        entitlements.to_file_xml(&entitlements_path).unwrap();
        project.resolved_manifest.targets[0].entitlements = Some("App.entitlements".into());

        let identifiers = project_entitlement_identifiers(&project).unwrap();
        assert_eq!(
            identifiers,
            ProjectEntitlementIdentifiers {
                app_groups: vec!["group.dev.orbit.fixture".to_owned()],
                merchant_ids: vec!["merchant.dev.orbit.fixture".to_owned()],
                cloud_containers: vec!["iCloud.dev.orbit.fixture".to_owned()],
            }
        );
    }

    #[test]
    fn materializes_app_clip_entitlements_for_clip_and_host_app() {
        let (_temp, mut project) = test_project();
        let clip_entitlements_path = project.root.join("Clip.entitlements");
        let profile_path = project.root.join("Clip.mobileprovision");

        Value::Dictionary(plist::Dictionary::from_iter([(
            "com.apple.developer.parent-application-identifiers".to_owned(),
            Value::Array(vec![Value::String(
                "$(AppIdentifierPrefix)dev.orbit.fixture".to_owned(),
            )]),
        )]))
        .to_file_xml(&clip_entitlements_path)
        .unwrap();
        Value::Dictionary(plist::Dictionary::from_iter([(
            "ApplicationIdentifierPrefix".to_owned(),
            Value::Array(vec![Value::String("TEAM123456".to_owned())]),
        )]))
        .to_file_xml(&profile_path)
        .unwrap();

        project.resolved_manifest.targets[0]
            .dependencies
            .push("ExampleClip".to_owned());
        project.resolved_manifest.targets.push(TargetManifest {
            name: "ExampleClip".to_owned(),
            kind: TargetKind::App,
            bundle_id: "dev.orbit.fixture.clip".to_owned(),
            display_name: None,
            build_number: None,
            platforms: vec![ApplePlatform::Ios],
            sources: vec![project.root.join("Sources/Clip")],
            resources: Vec::new(),
            dependencies: Vec::new(),
            frameworks: Vec::new(),
            weak_frameworks: Vec::new(),
            system_libraries: Vec::new(),
            xcframeworks: Vec::new(),
            swift_packages: Vec::new(),
            info_plist: BTreeMap::new(),
            ios: None,
            entitlements: Some("Clip.entitlements".into()),
            push: None,
            extension: None,
        });

        let clip = project
            .resolved_manifest
            .resolve_target(Some("ExampleClip"))
            .unwrap();
        assert!(target_is_app_clip(&project, clip).unwrap());
        let clip_entitlements = materialize_signing_entitlements(&project, clip, &profile_path)
            .unwrap()
            .unwrap();
        let clip_dictionary = plist::Value::from_file(&clip_entitlements)
            .unwrap()
            .into_dictionary()
            .unwrap();
        assert_eq!(
            clip_dictionary
                .get("com.apple.developer.parent-application-identifiers")
                .and_then(plist::Value::as_array)
                .unwrap()[0]
                .as_string()
                .unwrap(),
            "TEAM123456.dev.orbit.fixture"
        );
        assert_eq!(
            clip_dictionary
                .get("com.apple.developer.on-demand-install-capable")
                .and_then(plist::Value::as_boolean),
            Some(true)
        );

        let host = project
            .resolved_manifest
            .resolve_target(Some("ExampleApp"))
            .unwrap();
        let host_entitlements = materialize_signing_entitlements(&project, host, &profile_path)
            .unwrap()
            .unwrap();
        let host_dictionary = plist::Value::from_file(&host_entitlements)
            .unwrap()
            .into_dictionary()
            .unwrap();
        assert_eq!(
            host_dictionary
                .get("com.apple.developer.associated-appclip-app-identifiers")
                .and_then(plist::Value::as_array)
                .unwrap()[0]
                .as_string()
                .unwrap(),
            "TEAM123456.dev.orbit.fixture.clip"
        );
    }

    #[test]
    fn materializes_profile_entitlements_when_target_has_no_entitlements_file() {
        let (_temp, project) = test_project();
        let profile_path = project.root.join("Example.mobileprovision");
        Value::Dictionary(plist::Dictionary::from_iter([
            (
                "ApplicationIdentifierPrefix".to_owned(),
                Value::Array(vec![Value::String("TEAM123456".to_owned())]),
            ),
            (
                "Entitlements".to_owned(),
                Value::Dictionary(plist::Dictionary::from_iter([
                    (
                        "application-identifier".to_owned(),
                        Value::String("TEAM123456.dev.orbit.fixture".to_owned()),
                    ),
                    (
                        "com.apple.developer.team-identifier".to_owned(),
                        Value::String("TEAM123456".to_owned()),
                    ),
                    ("get-task-allow".to_owned(), Value::Boolean(true)),
                    (
                        "keychain-access-groups".to_owned(),
                        Value::Array(vec![Value::String("TEAM123456.*".to_owned())]),
                    ),
                    (
                        "com.apple.developer.game-center".to_owned(),
                        Value::Boolean(true),
                    ),
                ])),
            ),
        ]))
        .to_file_xml(&profile_path)
        .unwrap();

        let target = project
            .resolved_manifest
            .resolve_target(Some("ExampleApp"))
            .unwrap();
        let generated = materialize_signing_entitlements(&project, target, &profile_path)
            .unwrap()
            .unwrap();
        let dictionary = plist::Value::from_file(&generated)
            .unwrap()
            .into_dictionary()
            .unwrap();
        assert_eq!(
            dictionary
                .get("application-identifier")
                .and_then(plist::Value::as_string),
            Some("TEAM123456.dev.orbit.fixture")
        );
        assert_eq!(
            dictionary
                .get("com.apple.developer.team-identifier")
                .and_then(plist::Value::as_string),
            Some("TEAM123456")
        );
        assert_eq!(
            dictionary
                .get("get-task-allow")
                .and_then(plist::Value::as_boolean),
            Some(true)
        );
    }

    #[test]
    fn merges_managed_profile_entitlements_into_explicit_entitlements() {
        let (_temp, mut project) = test_project();
        let entitlements_path = project.root.join("App.entitlements");
        let profile_path = project.root.join("Example.mobileprovision");

        Value::Dictionary(plist::Dictionary::from_iter([(
            "aps-environment".to_owned(),
            Value::String("development".to_owned()),
        )]))
        .to_file_xml(&entitlements_path)
        .unwrap();
        Value::Dictionary(plist::Dictionary::from_iter([
            (
                "ApplicationIdentifierPrefix".to_owned(),
                Value::Array(vec![Value::String("TEAM123456".to_owned())]),
            ),
            (
                "Entitlements".to_owned(),
                Value::Dictionary(plist::Dictionary::from_iter([
                    (
                        "application-identifier".to_owned(),
                        Value::String("TEAM123456.dev.orbit.fixture".to_owned()),
                    ),
                    (
                        "com.apple.developer.team-identifier".to_owned(),
                        Value::String("TEAM123456".to_owned()),
                    ),
                    ("get-task-allow".to_owned(), Value::Boolean(true)),
                    (
                        "keychain-access-groups".to_owned(),
                        Value::Array(vec![Value::String("TEAM123456.*".to_owned())]),
                    ),
                ])),
            ),
        ]))
        .to_file_xml(&profile_path)
        .unwrap();
        project.resolved_manifest.targets[0].entitlements = Some("App.entitlements".into());

        let target = project
            .resolved_manifest
            .resolve_target(Some("ExampleApp"))
            .unwrap();
        let generated = materialize_signing_entitlements(&project, target, &profile_path)
            .unwrap()
            .unwrap();
        let dictionary = plist::Value::from_file(&generated)
            .unwrap()
            .into_dictionary()
            .unwrap();
        assert_eq!(
            dictionary
                .get("aps-environment")
                .and_then(plist::Value::as_string),
            Some("development")
        );
        assert_eq!(
            dictionary
                .get("application-identifier")
                .and_then(plist::Value::as_string),
            Some("TEAM123456.dev.orbit.fixture")
        );
        assert_eq!(
            dictionary
                .get("keychain-access-groups")
                .and_then(plist::Value::as_array)
                .map(|values| values.len()),
            Some(1)
        );
    }

    #[test]
    fn api_key_capability_mutations_fail_for_identifier_linking() {
        let error = plan_asc_capability_mutations(
            &[CapabilityUpdate {
                capability_type: "APP_GROUPS".to_owned(),
                option: "ON".to_owned(),
                relationships: CapabilityRelationships {
                    app_groups: Some(vec!["group.dev.orbit.fixture".to_owned()]),
                    merchant_ids: None,
                    cloud_containers: None,
                },
            }],
            &[],
        )
        .unwrap_err();

        assert!(error.to_string().contains("cannot link App Groups"));
    }

    #[test]
    fn api_key_capability_mutations_fail_for_broadcast_push() {
        let error = asc_capability_settings(&CapabilityUpdate {
            capability_type: "PUSH_NOTIFICATIONS".to_owned(),
            option: ASC_OPTION_PUSH_BROADCAST.to_owned(),
            relationships: CapabilityRelationships::default(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("broadcast push"));
    }

    #[test]
    fn api_key_capability_mutations_build_expected_settings() {
        let remote = vec![RemoteCapability {
            id: "CAP-APPLE-ID".to_owned(),
            capability_type: "APPLE_ID_AUTH".to_owned(),
            enabled: Some(true),
            settings: Vec::new(),
        }];
        let updates = vec![
            CapabilityUpdate {
                capability_type: "APPLE_ID_AUTH".to_owned(),
                option: ASC_OPTION_APPLE_ID_PRIMARY_CONSENT.to_owned(),
                relationships: CapabilityRelationships::default(),
            },
            CapabilityUpdate {
                capability_type: "DATA_PROTECTION".to_owned(),
                option: ASC_OPTION_DATA_PROTECTION_COMPLETE.to_owned(),
                relationships: CapabilityRelationships::default(),
            },
        ];

        let mutations = plan_asc_capability_mutations(&updates, &remote).unwrap();
        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0].remote_id.as_deref(), Some("CAP-APPLE-ID"));
        assert_eq!(mutations[0].settings[0].key, "APPLE_ID_AUTH_APP_CONSENT");
        assert_eq!(
            mutations[0].settings[0].options[0].key,
            "PRIMARY_APP_CONSENT"
        );
        assert_eq!(
            mutations[1].settings[0].key,
            "DATA_PROTECTION_PERMISSION_LEVEL"
        );
        assert_eq!(
            mutations[1].settings[0].options[0].key,
            "COMPLETE_PROTECTION"
        );
    }

    #[test]
    fn api_key_profile_type_uses_ios_profiles_for_watch_and_vision_targets() {
        let profile = ProfileManifest::new(BuildConfiguration::Release, DistributionKind::AppStore);

        assert_eq!(
            asc_profile_type(ApplePlatform::Watchos, &profile).unwrap(),
            "IOS_APP_STORE"
        );
        assert_eq!(
            asc_profile_type(ApplePlatform::Visionos, &profile).unwrap(),
            "IOS_APP_STORE"
        );
    }

    #[test]
    fn plain_push_flag_is_allowed_with_api_key_auth() {
        let (_temp, mut project) = test_project();
        project.resolved_manifest.targets[0].push = Some(crate::manifest::PushManifest {
            broadcast_for_live_activities: false,
        });
        let target = &project.resolved_manifest.targets[0];
        let options = validate_push_setup_with_api_key(target);
        assert!(options.uses_push_notifications);
        assert!(!options.uses_broadcast_push_notifications);
    }

    #[test]
    fn api_key_path_warns_and_skips_broadcast_push_setting() {
        let (_temp, mut project) = test_project();
        project.resolved_manifest.targets[0].push = Some(crate::manifest::PushManifest {
            broadcast_for_live_activities: true,
        });

        let target = &project.resolved_manifest.targets[0];
        let options = validate_push_setup_with_api_key(target);
        assert!(options.uses_push_notifications);
        assert!(!options.uses_broadcast_push_notifications);
    }

    #[test]
    fn sanitizes_orbit_managed_app_name_for_apple() {
        assert_eq!(
            orbit_managed_app_name("@orbit/Example IOS-App"),
            "Orbit orbit Example IOS App"
        );
        assert_eq!(orbit_managed_app_name(""), "Orbit App");
    }

    #[test]
    fn parses_certificate_common_name_from_subject_output() {
        assert_eq!(
            parse_certificate_common_name(
                "subject=CN = Apple Development: Ilya Ivankin (FVTTLAH6QU), OU = Example"
            )
            .as_deref(),
            Some("Apple Development: Ilya Ivankin (FVTTLAH6QU)")
        );
    }

    #[test]
    fn parses_codesigning_identity_hash_and_name() {
        assert_eq!(
            parse_codesigning_identity_line(
                r#"  1) 04B011F1ABF0F7B8DDF99CD8BC88D5366AC8CC4D "Apple Development: Ilya Ivankin (FVTTLAH6QU)""#
            ),
            Some((
                "04B011F1ABF0F7B8DDF99CD8BC88D5366AC8CC4D".to_owned(),
                "Apple Development: Ilya Ivankin (FVTTLAH6QU)".to_owned()
            ))
        );
    }

    #[test]
    fn ad_hoc_device_helpers_match_eas_style_reuse_logic() {
        let devices = vec![
            CachedDevice {
                id: "DEV-1".to_owned(),
                name: "Alice iPhone".to_owned(),
                udid: "UDID-1".to_owned(),
                platform: "IOS".to_owned(),
                status: "ENABLED".to_owned(),
                device_class: Some("IPHONE".to_owned()),
                model: Some("iPhone17,1".to_owned()),
                created_at: Some("2026-03-30T00:00:00Z".to_owned()),
            },
            CachedDevice {
                id: "DEV-2".to_owned(),
                name: "Bob iPad".to_owned(),
                udid: "UDID-2".to_owned(),
                platform: "IOS".to_owned(),
                status: "ENABLED".to_owned(),
                device_class: Some("IPAD".to_owned()),
                model: Some("iPad16,3".to_owned()),
                created_at: None,
            },
        ];
        let profile = ManagedProfile {
            id: "PROFILE".to_owned(),
            profile_type: "IOS_APP_ADHOC".to_owned(),
            bundle_id: "dev.orbit.fixture".to_owned(),
            path: std::path::PathBuf::from("/tmp/profile.mobileprovision"),
            uuid: None,
            certificate_ids: vec!["CERT".to_owned()],
            device_ids: vec!["DEV-1".to_owned()],
        };

        assert_eq!(profile_udids(&profile, &devices), vec!["UDID-1".to_owned()]);
        assert!(!same_udid_set(
            &devices
                .iter()
                .map(|device| device.udid.clone())
                .collect::<Vec<_>>(),
            &profile_udids(&profile, &devices),
        ));
        let missing = missing_registered_devices(&devices, &profile_udids(&profile, &devices));
        assert_eq!(missing.len(), 1);
        assert!(format_cached_device_label(&devices[0]).contains("UDID-1"));
        assert!(format_cached_device_label(&devices[0]).contains("Alice iPhone"));
    }

    #[test]
    fn current_profile_lookup_prefers_latest_matching_profile() {
        let first = ManagedProfile {
            id: "PROFILE-OLD".to_owned(),
            profile_type: "IOS_APP_ADHOC".to_owned(),
            bundle_id: "dev.orbit.fixture".to_owned(),
            path: std::path::PathBuf::from("/tmp/old.mobileprovision"),
            uuid: None,
            certificate_ids: vec!["CERT".to_owned()],
            device_ids: vec!["DEV-1".to_owned()],
        };
        let second = ManagedProfile {
            id: "PROFILE-NEW".to_owned(),
            profile_type: "IOS_APP_ADHOC".to_owned(),
            bundle_id: "dev.orbit.fixture".to_owned(),
            path: std::path::PathBuf::from("/tmp/new.mobileprovision"),
            uuid: None,
            certificate_ids: vec!["CERT".to_owned()],
            device_ids: vec!["DEV-2".to_owned()],
        };
        std::fs::write(&first.path, b"old").unwrap();
        std::fs::write(&second.path, b"new").unwrap();
        let state = SigningState {
            certificates: Vec::new(),
            profiles: vec![first, second],
        };

        let profile =
            current_profile_for_target(&state, "dev.orbit.fixture", "IOS_APP_ADHOC", "CERT")
                .unwrap();
        assert_eq!(profile.id, "PROFILE-NEW");
    }
}
