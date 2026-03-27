use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::apple::auth::{EnsureUserAuthRequest, ensure_portal_authenticated};
use crate::apple::capabilities::{
    CapabilityRelationships, CapabilityUpdate, capability_sync_plan_from_entitlements,
};
use crate::apple::portal::{
    PortalAppId, PortalClient, PortalDeviceClass, PortalProfilePlatform, PortalProvisioningProfile,
};
use crate::apple::provisioning::{
    ProvisioningCapabilityRelationships, ProvisioningCapabilityUpdate, ProvisioningClient,
};
use crate::cli::SigningSyncArgs;
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest, TargetManifest};
use crate::util::{
    ensure_dir, prompt_multi_select, prompt_select, read_json_file_if_exists, write_json_file,
};

const P12_PASSWORD_SERVICE: &str = "dev.orbit.cli.codesign-p12";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SigningState {
    certificates: Vec<ManagedCertificate>,
    profiles: Vec<ManagedProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedCertificate {
    id: String,
    certificate_type: String,
    serial_number: String,
    display_name: Option<String>,
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

pub fn sync_signing(project: &ProjectContext, args: &SigningSyncArgs) -> Result<()> {
    let target = resolve_signing_target(project, args.target.as_deref())?;
    let platform = project.manifest.resolve_platform_for_target(target, None)?;
    let profile_name = resolve_profile_name(project, platform, args.profile.as_deref())?;
    let profile = project.manifest.profile_for(platform, &profile_name)?;

    if !args.device && args.simulator {
        println!("simulator builds do not require signing");
        return Ok(());
    }

    let device_udids = if matches!(
        profile.distribution,
        DistributionKind::Development | DistributionKind::AdHoc
    ) {
        Some(select_device_udids(
            project,
            profile.distribution,
            platform,
        )?)
    } else {
        None
    };

    let material = prepare_signing(project, target, platform, profile, device_udids)?;
    println!("identity: {}", material.signing_identity);
    println!("keychain: {}", material.keychain_path.display());
    println!(
        "provisioning_profile: {}",
        material.provisioning_profile_path.display()
    );
    if let Some(entitlements_path) = &material.entitlements_path {
        println!("entitlements: {}", entitlements_path.display());
    }
    Ok(())
}

fn resolve_signing_target<'a>(
    project: &'a ProjectContext,
    requested_target: Option<&str>,
) -> Result<&'a TargetManifest> {
    if let Some(requested_target) = requested_target {
        return project.manifest.resolve_target(Some(requested_target));
    }

    let mut candidates = project.manifest.selectable_root_targets();
    if candidates.len() <= 1 || !project.app.interactive {
        return candidates
            .drain(..)
            .next()
            .context("manifest did not contain any targets");
    }

    let labels = candidates
        .iter()
        .map(|target| format!("{} ({})", target.name, target.bundle_id))
        .collect::<Vec<_>>();
    let index = prompt_select("Select a target to sync signing for", &labels)?;
    Ok(candidates.remove(index))
}

fn resolve_profile_name(
    project: &ProjectContext,
    platform: ApplePlatform,
    requested_profile: Option<&str>,
) -> Result<String> {
    if let Some(requested_profile) = requested_profile {
        let _ = project.manifest.profile_for(platform, requested_profile)?;
        return Ok(requested_profile.to_owned());
    }

    let profiles = project.manifest.profile_names(platform)?;
    if profiles.len() == 1 {
        return Ok(profiles[0].clone());
    }
    if !project.app.interactive {
        bail!(
            "multiple profiles are available for platform `{platform}`; pass --profile ({})",
            profiles.join(", ")
        );
    }

    let index = prompt_select("Select a signing profile", &profiles)?;
    Ok(profiles[index].clone())
}

pub fn prepare_signing(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    profile: &ProfileManifest,
    device_udids: Option<Vec<String>>,
) -> Result<SigningMaterial> {
    let auth = ensure_portal_authenticated(
        &project.app,
        EnsureUserAuthRequest {
            team_id: project.manifest.team_id.clone(),
            provider_id: project.manifest.provider_id.clone(),
            prompt_for_missing: project.app.interactive,
            ..Default::default()
        },
    )?;
    let team_id = auth.user.team_id.clone().context(
        "signing requires an Apple Developer team selection; log in again and choose a team if prompted",
    )?;
    let mut client = PortalClient::from_session(&auth.session, team_id.clone())?;
    let provisioning = ProvisioningClient::from_session(&auth.session, team_id)?;
    let mut state = load_state(project)?;

    let bundle_id = ensure_bundle_id(&mut client, project, target, platform)?;
    sync_capabilities(
        &mut client,
        &provisioning,
        project,
        target,
        platform,
        &bundle_id,
    )?;

    let certificate_type = certificate_type(platform, profile)?;
    let certificate = ensure_certificate(&mut client, project, &mut state, certificate_type)?;
    let profile_type = profile_type(platform, profile)?;
    let device_ids = if matches!(
        profile.distribution,
        DistributionKind::Development | DistributionKind::AdHoc
    ) {
        let selected_udids = device_udids.unwrap_or_default();
        resolve_device_ids(&mut client, platform, &selected_udids)?
    } else {
        Vec::new()
    };
    let provisioning_profile = ensure_profile(
        &mut client,
        project,
        &mut state,
        platform,
        &bundle_id,
        profile_type,
        &certificate,
        &device_ids,
    )?;

    let signing_identity = import_certificate_into_keychain(project, &certificate)?;
    save_state(project, &state)?;

    Ok(SigningMaterial {
        signing_identity,
        keychain_path: project.app.global_paths.keychain_path.clone(),
        provisioning_profile_path: provisioning_profile.path,
        entitlements_path: target
            .entitlements
            .as_ref()
            .map(|path| project.root.join(path)),
    })
}

pub fn prepare_package_signing(
    project: &ProjectContext,
    profile: &ProfileManifest,
) -> Result<PackageSigningMaterial> {
    let auth = ensure_portal_authenticated(
        &project.app,
        EnsureUserAuthRequest {
            team_id: project.manifest.team_id.clone(),
            provider_id: project.manifest.provider_id.clone(),
            prompt_for_missing: project.app.interactive,
            ..Default::default()
        },
    )?;
    let team_id = auth.user.team_id.clone().context(
        "installer signing requires an Apple Developer team selection; log in again and choose a team if prompted",
    )?;
    let mut client = PortalClient::from_session(&auth.session, team_id)?;
    let mut state = load_state(project)?;
    let certificate_type = installer_certificate_type(profile)?;
    let certificate = ensure_certificate(&mut client, project, &mut state, certificate_type)?;
    let signing_identity = import_certificate_into_keychain(project, &certificate)?;
    save_state(project, &state)?;
    Ok(PackageSigningMaterial {
        signing_identity,
        keychain_path: project.app.global_paths.keychain_path.clone(),
    })
}

pub fn sign_bundle(bundle_path: &Path, material: &SigningMaterial) -> Result<()> {
    let embedded_profile = bundle_path.join("embedded.mobileprovision");
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
    if let Some(entitlements) = &material.entitlements_path {
        command.args(["--entitlements"]);
        command.arg(entitlements);
    }
    command.arg(bundle_path);
    crate::util::run_command(&mut command)
}

fn ensure_bundle_id(
    client: &mut PortalClient,
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
) -> Result<PortalAppId> {
    if let Some(bundle_id) =
        client.find_app_by_bundle_id(&target.bundle_id, matches!(platform, ApplePlatform::Macos))?
    {
        return Ok(bundle_id);
    }

    client.create_app(
        &format!("@orbit/{}", project.manifest.name),
        &target.bundle_id,
        matches!(platform, ApplePlatform::Macos),
    )
}

fn sync_capabilities(
    client: &mut PortalClient,
    provisioning: &ProvisioningClient,
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    bundle_id: &PortalAppId,
) -> Result<()> {
    let Some(entitlements_path) = &target.entitlements else {
        return Ok(());
    };
    let provisioning_bundle = provisioning
        .find_bundle_id(&bundle_id.identifier)?
        .with_context(|| {
            format!(
                "bundle identifier `{}` exists in the Developer Portal but could not be loaded from Apple's provisioning API",
                bundle_id.identifier
            )
        })?;
    let plan = capability_sync_plan_from_entitlements(
        &project.root.join(entitlements_path),
        &provisioning_bundle.capabilities,
    )?;
    if plan.updates.is_empty() {
        return Ok(());
    }

    let app_group_ids = resolve_app_group_ids(
        client,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.app_groups.as_ref()
        }),
    )?;
    let merchant_ids = resolve_merchant_ids(
        client,
        platform,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.merchant_ids.as_ref()
        }),
    )?;
    let cloud_container_ids = resolve_cloud_container_ids(
        client,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.cloud_containers.as_ref()
        }),
    )?;
    let updates = plan
        .updates
        .iter()
        .map(|update| {
            Ok(ProvisioningCapabilityUpdate {
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
    provisioning.update_bundle_capabilities(&provisioning_bundle, &updates)?;
    Ok(())
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

fn resolve_app_group_ids(
    client: &mut PortalClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = client.list_app_groups()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_group) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_group.id.clone()
        } else {
            let name = identifier_name("App Group", &identifier);
            client.create_app_group(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn resolve_merchant_ids(
    client: &mut PortalClient,
    platform: ApplePlatform,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = client.list_merchants(platform == ApplePlatform::Macos)?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_merchant) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_merchant.id.clone()
        } else {
            let name = identifier_name("Merchant ID", &identifier);
            client
                .create_merchant(&name, &identifier, platform == ApplePlatform::Macos)?
                .id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn resolve_cloud_container_ids(
    client: &mut PortalClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = client.list_cloud_containers()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_container) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_container.id.clone()
        } else {
            let name = identifier_name("iCloud Container", &identifier);
            client.create_cloud_container(&name, &identifier)?.id
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

fn ensure_certificate(
    client: &mut PortalClient,
    project: &ProjectContext,
    state: &mut SigningState,
    certificate_type: &str,
) -> Result<ManagedCertificate> {
    let remote_certificates = client.list_certificates(
        &[certificate_type],
        is_macos_certificate_type(certificate_type),
    )?;
    for remote in &remote_certificates {
        if let Some(local) = state
            .certificates
            .iter()
            .find(|certificate| certificate.id == remote.id && certificate.p12_path.exists())
        {
            return Ok(local.clone());
        }
    }

    let certificates_dir = project.app.global_paths.data_dir.join("certificates");
    ensure_dir(&certificates_dir)?;
    let slug = crate::util::timestamp_slug();
    let private_key_path = certificates_dir.join(format!("{slug}.key.pem"));
    let csr_path = certificates_dir.join(format!("{slug}.csr.pem"));
    let certificate_der_path = certificates_dir.join(format!("{slug}.cer"));
    let p12_path = certificates_dir.join(format!("{slug}.p12"));

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
    let remote = client.create_certificate(
        certificate_type,
        &csr_pem,
        is_macos_certificate_type(certificate_type),
    )?;
    let certificate_bytes = client.download_certificate(
        &remote.id,
        certificate_type,
        is_macos_certificate_type(certificate_type),
    )?;
    fs::write(&certificate_der_path, &certificate_bytes)
        .with_context(|| format!("failed to write {}", certificate_der_path.display()))?;

    let p12_password = uuid::Uuid::new_v4().to_string();
    let mut openssl_pkcs12 = Command::new("openssl");
    openssl_pkcs12.args([
        "pkcs12",
        "-export",
        "-inkey",
        private_key_path
            .to_str()
            .context("private key path contains invalid UTF-8")?,
        "-in",
        certificate_der_path
            .to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-inform",
        "DER",
        "-out",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-passout",
        &format!("pass:{p12_password}"),
    ]);
    crate::util::run_command(&mut openssl_pkcs12)?;

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
        display_name: remote.name.clone(),
        private_key_path,
        certificate_der_path,
        p12_path,
        p12_password_account: password_account,
    };
    state.certificates.push(certificate.clone());
    Ok(certificate)
}

fn ensure_profile(
    client: &mut PortalClient,
    project: &ProjectContext,
    state: &mut SigningState,
    platform: ApplePlatform,
    bundle_id: &PortalAppId,
    profile_type: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
) -> Result<ManagedProfile> {
    let portal_platform = portal_profile_platform(platform);
    let profiles = client.list_profiles(portal_platform)?;
    let bundle_identifier = &bundle_id.identifier;
    let mut remote_profile_ids = HashSet::new();
    let mut stale_orbit_profiles = Vec::new();

    for profile in profiles {
        remote_profile_ids.insert(profile.id.clone());
        let Some(app) = &profile.app else {
            continue;
        };
        if app.id != bundle_id.id {
            continue;
        }

        let certificate_links = profile
            .certificates
            .iter()
            .map(|certificate| certificate.id.clone())
            .collect::<Vec<_>>();
        let device_links = profile
            .devices
            .iter()
            .map(|device| device.id.clone())
            .collect::<Vec<_>>();

        let matches_certificate = certificate_links.contains(&certificate.id);
        let matches_devices = canonical_ids(&device_links) == canonical_ids(device_ids);

        if matches_certificate && matches_devices {
            let managed = persist_profile(
                client,
                project,
                state,
                portal_platform,
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
            .delete_profile(portal_platform, &profile_id)
            .with_context(|| format!("failed to repair provisioning profile `{profile_id}`"))?;
        state.profiles.retain(|profile| profile.id != profile_id);
    }
    cleanup_stale_profile_state(state, bundle_identifier, profile_type, &remote_profile_ids);

    let remote = client.create_profile(
        portal_platform,
        &format!(
            "*[orbit] {} {} {}",
            bundle_identifier,
            profile_type,
            crate::util::timestamp_slug()
        ),
        profile_type,
        &bundle_id.id,
        &[certificate.id.clone()],
        device_ids,
    )?;
    persist_profile(
        client,
        project,
        state,
        portal_platform,
        profile_type,
        bundle_identifier,
        certificate,
        device_ids,
        remote,
    )
}

fn persist_profile(
    client: &mut PortalClient,
    project: &ProjectContext,
    state: &mut SigningState,
    platform: PortalProfilePlatform,
    profile_type: &str,
    bundle_identifier: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
    remote: PortalProvisioningProfile,
) -> Result<ManagedProfile> {
    let profiles_dir = project.app.global_paths.data_dir.join("profiles");
    ensure_dir(&profiles_dir)?;
    let profile_bytes = client.download_profile(platform, &remote.id)?;
    let profile_path = profiles_dir.join(format!("{}-{}.mobileprovision", remote.id, profile_type));
    fs::write(&profile_path, profile_bytes)
        .with_context(|| format!("failed to write {}", profile_path.display()))?;

    state.profiles.retain(|profile| profile.id != remote.id);
    let profile = ManagedProfile {
        id: remote.id,
        profile_type: profile_type.to_owned(),
        bundle_id: bundle_identifier.to_owned(),
        path: profile_path,
        uuid: remote.uuid.clone(),
        certificate_ids: vec![certificate.id.clone()],
        device_ids: device_ids.to_vec(),
    };
    state.profiles.push(profile.clone());
    Ok(profile)
}

fn resolve_device_ids(
    client: &mut PortalClient,
    platform: ApplePlatform,
    udids: &[String],
) -> Result<Vec<String>> {
    let class = device_class_for_platform(platform);
    if udids.is_empty() {
        return Ok(client
            .list_devices(class, false)?
            .into_iter()
            .map(|device| device.id)
            .collect());
    }

    let mut device_ids = Vec::new();
    for udid in udids {
        let device = client
            .find_device_by_udid(udid, class)?
            .with_context(|| format!("device `{udid}` is not registered with Apple"))?;
        device_ids.push(device.id);
    }
    Ok(device_ids)
}

fn select_device_udids(
    project: &ProjectContext,
    distribution: DistributionKind,
    platform: ApplePlatform,
) -> Result<Vec<String>> {
    let cache = crate::apple::device::refresh_cache(&project.app)?;
    let devices = cache
        .devices
        .into_iter()
        .filter(|device| device_matches_platform(&device.platform, platform))
        .collect::<Vec<_>>();
    if devices.is_empty() {
        bail!(
            "no registered Apple devices found for {platform}; run `orbit apple device register` first"
        );
    }

    if !project.app.interactive {
        return Ok(devices.into_iter().map(|device| device.udid).collect());
    }

    let labels = devices
        .iter()
        .map(|device| format!("{} ({})", device.name, device.udid))
        .collect::<Vec<_>>();
    if matches!(distribution, DistributionKind::AdHoc) {
        let defaults = vec![true; labels.len()];
        let selections = prompt_multi_select(
            "Select devices to include in the ad-hoc provisioning profile",
            &labels,
            Some(&defaults),
        )?;
        if selections.is_empty() {
            bail!("select at least one device for an ad-hoc provisioning profile");
        }
        return Ok(selections
            .into_iter()
            .map(|index| devices[index].udid.clone())
            .collect());
    }

    let index = prompt_select("Select a device to provision", &labels)?;
    Ok(vec![devices[index].udid.clone()])
}

fn device_class_for_platform(platform: ApplePlatform) -> PortalDeviceClass {
    match platform {
        ApplePlatform::Ios | ApplePlatform::Visionos => PortalDeviceClass::Iphone,
        ApplePlatform::Tvos => PortalDeviceClass::Tvos,
        ApplePlatform::Watchos => PortalDeviceClass::Watch,
        ApplePlatform::Macos => PortalDeviceClass::Mac,
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

fn portal_profile_platform(platform: ApplePlatform) -> PortalProfilePlatform {
    match platform {
        ApplePlatform::Ios => PortalProfilePlatform::Ios,
        ApplePlatform::Tvos => PortalProfilePlatform::Tvos,
        ApplePlatform::Watchos => PortalProfilePlatform::Watchos,
        ApplePlatform::Visionos => PortalProfilePlatform::Visionos,
        ApplePlatform::Macos => PortalProfilePlatform::Macos,
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

fn identifier_name(prefix: &str, identifier: &str) -> String {
    format!("{prefix} {identifier}")
}

fn is_macos_certificate_type(certificate_type: &str) -> bool {
    matches!(
        certificate_type,
        "749Y1QAGU7" | "HXZEUKP0FP" | "2PQI8IDXNH" | "OYVN2GW35E" | "W0EURJRMC5"
    )
}

fn read_certificate_serial(path: &Path) -> Result<String> {
    let mut command = Command::new("openssl");
    command.args([
        "x509",
        "-inform",
        "DER",
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

fn load_state(project: &ProjectContext) -> Result<SigningState> {
    Ok(read_json_file_if_exists(&project.app.global_paths.signing_state_path)?.unwrap_or_default())
}

fn save_state(project: &ProjectContext, state: &SigningState) -> Result<()> {
    write_json_file(&project.app.global_paths.signing_state_path, state)
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

fn import_certificate_into_keychain(
    project: &ProjectContext,
    certificate: &ManagedCertificate,
) -> Result<String> {
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

    let p12_password = load_p12_password(&certificate.p12_password_account)?;
    let mut import = Command::new("security");
    import.args([
        "import",
        certificate
            .p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-k",
        keychain_str,
        "-P",
        &p12_password,
        "-T",
        "/usr/bin/codesign",
        "-T",
        "/usr/bin/security",
    ]);
    let _ = crate::util::run_command(&mut import);

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
    let _ = crate::util::run_command(&mut partition);

    let mut find_identity = Command::new("security");
    find_identity.args(["find-identity", "-v", "-p", "codesigning", keychain_str]);
    let output = crate::util::command_output(&mut find_identity)?;
    let serial = certificate.serial_number.to_lowercase();
    for line in output.lines() {
        if line.to_lowercase().contains(&serial) {
            if let Some(start) = line.find('"') {
                if let Some(end) = line[start + 1..].find('"') {
                    return Ok(line[start + 1..start + 1 + end].to_owned());
                }
            }
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if parts.len() >= 2 {
                return Ok(parts[1].trim_matches('"').to_owned());
            }
        }
    }

    if let Some(display_name) = &certificate.display_name {
        return Ok(display_name.clone());
    }
    bail!(
        "failed to resolve imported signing identity for certificate {}",
        certificate.id
    )
}
