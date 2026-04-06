use super::*;
use plist::{Dictionary, Value};
use std::io::Cursor;
use tempfile::NamedTempFile;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

struct GeneratedCertificatePaths {
    private_key_path: PathBuf,
    csr_path: PathBuf,
    certificate_der_path: PathBuf,
    p12_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ManagedBundleId {
    id: String,
    identifier: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DecodedProvisioningProfile {
    device_udids: Vec<String>,
    certificate_serial_numbers: Vec<String>,
}

impl From<&ProvisioningBundleId> for ManagedBundleId {
    fn from(value: &ProvisioningBundleId) -> Self {
        Self {
            id: value.id.clone(),
            identifier: value.identifier.clone(),
        }
    }
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
    let profile_kind = asc_profile_type(platform, profile)?;
    let device_ids = resolve_profile_device_ids(
        project,
        profile.distribution,
        platform,
        CurrentProfileLookup {
            state: &state,
            bundle_identifier: &bundle_id.identifier,
            profile_kind,
            certificate_id: &certificate.id,
        },
        device_udids,
        &target.name,
        |selected_udids| {
            resolve_device_ids_with_developer_services(
                project,
                &mut provisioning,
                platform,
                selected_udids,
            )
        },
    )?;
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
                profile_kind,
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
    let profile_kind = asc_profile_type(platform, profile)?;
    let device_ids = resolve_profile_device_ids(
        project,
        profile.distribution,
        platform,
        CurrentProfileLookup {
            state: &state,
            bundle_identifier: &bundle_id.attributes.identifier,
            profile_kind,
            certificate_id: &certificate.id,
        },
        device_udids,
        &target.name,
        |selected_udids| resolve_device_ids_with_api_key(&client, platform, selected_udids),
    )?;
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
                profile_kind,
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

fn generate_certificate_paths(certificates_dir: &Path) -> GeneratedCertificatePaths {
    let slug = crate::util::timestamp_slug();
    GeneratedCertificatePaths {
        private_key_path: certificates_dir.join(format!("{slug}.key.pem")),
        csr_path: certificates_dir.join(format!("{slug}.csr.pem")),
        certificate_der_path: certificates_dir.join(format!("{slug}.cer")),
        p12_path: certificates_dir.join(format!("{slug}.p12")),
    }
}

fn generate_certificate_signing_request(paths: &GeneratedCertificatePaths) -> Result<String> {
    let mut openssl_req = Command::new("openssl");
    openssl_req.args([
        "req",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        paths
            .private_key_path
            .to_str()
            .context("private key path contains invalid UTF-8")?,
        "-subj",
        "/CN=Orbit",
        "-out",
        paths
            .csr_path
            .to_str()
            .context("CSR path contains invalid UTF-8")?,
    ]);
    crate::util::run_command(&mut openssl_req)?;

    fs::read_to_string(&paths.csr_path)
        .with_context(|| format!("failed to read {}", paths.csr_path.display()))
}

fn persist_generated_certificate(
    state: &mut SigningState,
    certificate_type: &str,
    remote_id: String,
    serial_number: Option<String>,
    display_name: Option<String>,
    encoded_certificate_der: &str,
    paths: GeneratedCertificatePaths,
) -> Result<ManagedCertificate> {
    let certificate_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded_certificate_der)
        .context("failed to decode certificate content")?;
    fs::write(&paths.certificate_der_path, &certificate_bytes)
        .with_context(|| format!("failed to write {}", paths.certificate_der_path.display()))?;

    let p12_password = uuid::Uuid::new_v4().to_string();
    export_p12_from_der_certificate(
        &paths.private_key_path,
        &paths.certificate_der_path,
        &paths.p12_path,
        &p12_password,
    )?;

    let serial_number = match serial_number {
        Some(serial_number) => serial_number,
        None => read_certificate_serial(&paths.certificate_der_path)?,
    };
    let password_account = format!("{remote_id}-{serial_number}");
    store_p12_password(&password_account, &p12_password)?;

    let certificate = ManagedCertificate {
        id: remote_id,
        certificate_type: certificate_type.to_owned(),
        serial_number,
        origin: CertificateOrigin::Generated,
        display_name,
        system_keychain_path: None,
        system_signing_identity: None,
        private_key_path: paths.private_key_path,
        certificate_der_path: paths.certificate_der_path,
        p12_path: paths.p12_path,
        p12_password_account: password_account,
    };
    state.certificates.push(certificate.clone());
    Ok(certificate)
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
    let remote_certificates = filter_unexpired_certificates(
        state,
        certificate_type,
        provisioning.list_certificates(developer_services_certificate_type)?,
        |remote| remote.id.as_str(),
        |remote| remote.serial_number.as_deref(),
        |remote| remote.expiration_date.as_deref(),
    )?;
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

    let team_paths = team_signing_paths(project, &resolve_local_team_id(project)?);
    ensure_dir(&team_paths.certificates_dir)?;
    for remote in &remote_certificates {
        let Some(serial_number) = remote.serial_number.as_deref() else {
            continue;
        };
        if let Some(certificate) = recover_orphaned_certificate(
            &team_paths,
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
                private_key_path: team_paths
                    .certificates_dir
                    .join(format!("{serial_number}.managed.key")),
                certificate_der_path: team_paths
                    .certificates_dir
                    .join(format!("{serial_number}.managed.cer")),
                p12_path: team_paths
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

    let generated_paths = generate_certificate_paths(&team_paths.certificates_dir);
    let csr_pem = generate_certificate_signing_request(&generated_paths)?;
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
    let certificate = persist_generated_certificate(
        state,
        certificate_type,
        remote.id,
        remote.serial_number,
        remote.display_name,
        certificate_content,
        generated_paths,
    )?;
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
    let remote_profiles = provisioning.list_profiles(Some(profile_type))?;
    let requested_device_udids =
        resolve_device_udids_by_id_with_developer_services(provisioning, device_ids)?;
    let mut remote_profile_ids = HashSet::new();
    let mut stale_orbit_profiles = Vec::new();
    for remote in remote_profiles {
        remote_profile_ids.insert(remote.id.clone());
        if remote.bundle_id_identifier.as_deref() != Some(bundle_id.identifier.as_str()) {
            continue;
        }

        let matches_certificate = remote.certificate_ids.contains(&certificate.id);
        let matches_devices = profile_covers_requested_ids(&remote.device_ids, device_ids);
        let decoded_profile = if matches_certificate && matches_devices {
            None
        } else {
            decode_provisioning_profile_content(&remote)?
        };
        // Apple-managed development profiles can omit `devices` or `certificates`
        // relationships in Developer Services even though the signed profile
        // payload contains the effective selection. Use the profile payload as a
        // fallback source of truth before deciding the profile is stale.
        let matches_certificate = matches_certificate
            || decoded_profile.as_ref().is_some_and(|profile| {
                profile_has_certificate_serial(profile, &certificate.serial_number)
            });
        let matches_devices = matches_devices
            || decoded_profile.as_ref().is_some_and(|profile| {
                profile_covers_requested_strings_case_insensitive(
                    &profile.device_udids,
                    &requested_device_udids,
                )
            });
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

    let remote = provisioning.create_profile(
        &orbit_managed_profile_name(&bundle_id.identifier, profile_type),
        profile_type,
        &bundle_id.id,
        std::slice::from_ref(&certificate.id),
        device_ids,
    )?;
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
        certificate_ids: if remote.certificate_ids.is_empty() {
            vec![certificate.id.clone()]
        } else {
            remote.certificate_ids.clone()
        },
        device_ids: if remote.device_ids.is_empty() && !device_ids.is_empty() {
            device_ids.to_vec()
        } else {
            remote.device_ids.clone()
        },
    };
    state.profiles.push(profile.clone());
    Ok(profile)
}

fn resolve_device_udids_by_id_with_developer_services(
    provisioning: &mut ProvisioningClient,
    device_ids: &[String],
) -> Result<Vec<String>> {
    if device_ids.is_empty() {
        return Ok(Vec::new());
    }

    let devices = provisioning.list_devices()?;
    let mut udids = Vec::with_capacity(device_ids.len());
    for device_id in device_ids {
        let device = devices
            .iter()
            .find(|device| device.id == *device_id)
            .with_context(|| {
                format!(
                    "Developer Services did not return device `{device_id}` while preparing signing"
                )
            })?;
        udids.push(device.udid.clone());
    }
    Ok(udids)
}

fn decode_provisioning_profile_content(
    remote: &ProvisioningProfile,
) -> Result<Option<DecodedProvisioningProfile>> {
    let Some(profile_content) = remote.profile_content.as_deref() else {
        return Ok(None);
    };
    let profile_bytes = base64::engine::general_purpose::STANDARD
        .decode(profile_content)
        .context("failed to decode Developer Services provisioning profile content")?;
    Ok(Some(decode_provisioning_profile(&profile_bytes)?))
}

fn decode_provisioning_profile(profile_bytes: &[u8]) -> Result<DecodedProvisioningProfile> {
    let dictionary = decode_provisioning_profile_dictionary(profile_bytes)?;
    Ok(DecodedProvisioningProfile {
        device_udids: dictionary
            .get("ProvisionedDevices")
            .and_then(Value::as_array)
            .map(|devices| {
                devices
                    .iter()
                    .filter_map(Value::as_string)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        certificate_serial_numbers: provisioning_profile_certificate_serial_numbers(&dictionary)?,
    })
}

fn decode_provisioning_profile_dictionary(profile_bytes: &[u8]) -> Result<Dictionary> {
    if let Ok(value) = Value::from_reader(Cursor::new(profile_bytes))
        && let Some(dictionary) = value.into_dictionary()
    {
        return Ok(dictionary);
    }
    if let Ok(value) = Value::from_reader_xml(Cursor::new(profile_bytes))
        && let Some(dictionary) = value.into_dictionary()
    {
        return Ok(dictionary);
    }

    let temp_profile =
        NamedTempFile::new().context("failed to create temporary provisioning profile")?;
    fs::write(temp_profile.path(), profile_bytes).with_context(|| {
        format!(
            "failed to write temporary provisioning profile {}",
            temp_profile.path().display()
        )
    })?;
    let output = crate::util::command_output(
        Command::new("security")
            .args(["cms", "-D", "-i"])
            .arg(temp_profile.path()),
    )?;
    Value::from_reader_xml(output.as_bytes())
        .context("failed to decode provisioning profile CMS payload")?
        .into_dictionary()
        .context("decoded provisioning profile did not contain a top-level dictionary")
}

fn provisioning_profile_certificate_serial_numbers(dictionary: &Dictionary) -> Result<Vec<String>> {
    let Some(certificates) = dictionary
        .get("DeveloperCertificates")
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };

    let mut serial_numbers = Vec::with_capacity(certificates.len());
    for certificate in certificates {
        let Some(certificate_der) = certificate.as_data() else {
            continue;
        };
        let temp_certificate =
            NamedTempFile::new().context("failed to create temporary developer certificate")?;
        fs::write(temp_certificate.path(), certificate_der).with_context(|| {
            format!(
                "failed to write temporary developer certificate {}",
                temp_certificate.path().display()
            )
        })?;
        serial_numbers.push(normalized_serial_number(&read_certificate_serial(
            temp_certificate.path(),
        )?));
    }
    Ok(serial_numbers)
}

fn profile_has_certificate_serial(
    profile: &DecodedProvisioningProfile,
    certificate_serial_number: &str,
) -> bool {
    let serial_number = normalized_serial_number(certificate_serial_number);
    profile
        .certificate_serial_numbers
        .iter()
        .any(|candidate| candidate == &serial_number)
}

fn profile_covers_requested_strings_case_insensitive(
    actual: &[String],
    requested: &[String],
) -> bool {
    if requested.is_empty() {
        return actual.is_empty();
    }

    let actual = actual
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    requested
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .all(|value| actual.contains(&value))
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
            .filter(|device| asc_device_matches_platform(&device.platform, platform))
            .map(|device| device.id)
            .collect());
    }

    let mut device_ids = Vec::new();
    for udid in udids {
        let device = devices
            .iter()
            .find(|device| {
                device.udid.eq_ignore_ascii_case(udid)
                    && asc_device_matches_platform(&device.platform, platform)
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

fn resolve_api_key_team_id(project: &ProjectContext) -> Result<String> {
    resolve_local_team_id_if_known(project)?.context(
        "API key signing state is scoped by Apple team; set `team_id` in orbit.json or export ORBIT_APPLE_TEAM_ID for CI runs",
    )
}

fn ensure_certificate_with_api_key(
    client: &AscClient,
    project: &ProjectContext,
    state: &mut SigningState,
    certificate_type: &str,
) -> Result<ManagedCertificate> {
    let remote_certificates = filter_unexpired_certificates(
        state,
        certificate_type,
        client.list_certificates(certificate_type)?,
        |remote| remote.id.as_str(),
        |remote| remote.attributes.serial_number.as_deref(),
        |remote| remote.attributes.expiration_date.as_deref(),
    )?;
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

    let team_paths = team_signing_paths(project, &resolve_api_key_team_id(project)?);
    ensure_dir(&team_paths.certificates_dir)?;
    for remote in &remote_certificates {
        let Some(serial_number) = remote.attributes.serial_number.as_deref() else {
            continue;
        };
        if let Some(certificate) = recover_orphaned_certificate(
            &team_paths,
            state,
            certificate_type,
            &remote.id,
            serial_number,
            remote.attributes.display_name.as_deref(),
        )? {
            return Ok(certificate);
        }
    }

    let generated_paths = generate_certificate_paths(&team_paths.certificates_dir);
    let csr_pem = generate_certificate_signing_request(&generated_paths)?;
    let remote = client.create_certificate(certificate_type, &csr_pem)?;
    let certificate_content = remote
        .attributes
        .certificate_content
        .as_deref()
        .context("App Store Connect did not return the created certificate content")?;
    persist_generated_certificate(
        state,
        certificate_type,
        remote.id,
        remote.attributes.serial_number.clone(),
        remote.attributes.display_name.clone(),
        certificate_content,
        generated_paths,
    )
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
        let matches_devices = profile_covers_requested_ids(&device_links, device_ids);
        if matches_certificate && matches_devices {
            let managed = persist_asc_profile(
                project,
                state,
                profile_type,
                bundle_identifier,
                &certificate_links,
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
        &orbit_managed_profile_name(bundle_identifier, profile_type),
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
        std::slice::from_ref(&certificate.id),
        device_ids,
        remote,
    )
}

fn persist_asc_profile(
    project: &ProjectContext,
    state: &mut SigningState,
    profile_type: &str,
    bundle_identifier: &str,
    certificate_ids: &[String],
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
        certificate_ids: certificate_ids.to_vec(),
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

fn asc_device_matches_platform(device_platform: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios => device_platform == "IOS",
        ApplePlatform::Tvos => device_platform == "TVOS",
        ApplePlatform::Visionos => device_platform == "VISIONOS",
        ApplePlatform::Watchos => device_platform == "WATCHOS",
        ApplePlatform::Macos => device_platform == "MAC_OS" || device_platform == "MACOS",
    }
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
        if remote_profile_ids.contains(&profile.id) {
            return true;
        }
        let _ = delete_file_if_exists(&profile.path);
        false
    });
}

fn filter_unexpired_certificates<T, FId, FSerial, FExpiration>(
    state: &mut SigningState,
    certificate_type: &str,
    remote_certificates: Vec<T>,
    id_for: FId,
    serial_for: FSerial,
    expiration_for: FExpiration,
) -> Result<Vec<T>>
where
    FId: Fn(&T) -> &str,
    FSerial: Fn(&T) -> Option<&str>,
    FExpiration: Fn(&T) -> Option<&str>,
{
    let mut active = Vec::new();
    let mut expired_ids = HashSet::new();
    let mut expired_serials = HashSet::new();

    for remote in remote_certificates {
        if expiration_date_has_passed(expiration_for(&remote))? {
            expired_ids.insert(id_for(&remote).to_owned());
            if let Some(serial) = serial_for(&remote) {
                expired_serials.insert(normalized_serial_number(serial));
            }
            continue;
        }
        active.push(remote);
    }

    cleanup_expired_certificate_state(state, certificate_type, &expired_ids, &expired_serials);
    Ok(active)
}

fn cleanup_expired_certificate_state(
    state: &mut SigningState,
    certificate_type: &str,
    expired_ids: &HashSet<String>,
    expired_serials: &HashSet<String>,
) {
    state.certificates.retain(|certificate| {
        let is_expired = certificate.certificate_type == certificate_type
            && (expired_ids.contains(&certificate.id)
                || expired_serials.contains(&normalized_serial_number(&certificate.serial_number)));
        if !is_expired {
            return true;
        }
        let _ = delete_certificate_files(certificate);
        if !certificate.p12_password_account.is_empty() {
            let _ = delete_p12_password(&certificate.p12_password_account);
        }
        false
    });
}

fn expiration_date_has_passed(expiration_date: Option<&str>) -> Result<bool> {
    expiration_date_has_passed_at(expiration_date, OffsetDateTime::now_utc())
}

fn expiration_date_has_passed_at(
    expiration_date: Option<&str>,
    now: OffsetDateTime,
) -> Result<bool> {
    let Some(expiration_date) = expiration_date else {
        return Ok(false);
    };
    let expires_at = OffsetDateTime::parse(expiration_date, &Rfc3339)
        .with_context(|| format!("failed to parse expiration date `{expiration_date}`"))?;
    Ok(expires_at <= now)
}

fn normalized_serial_number(serial_number: &str) -> String {
    serial_number.to_ascii_lowercase()
}

fn orbit_managed_profile_name(bundle_identifier: &str, profile_type: &str) -> String {
    format!(
        "*[orbit] {} {} {}",
        bundle_identifier,
        profile_type,
        crate::util::timestamp_slug()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::process::Command;

    use plist::{Dictionary, Value};
    use tempfile::tempdir;

    use super::{
        ManagedCertificate, ManagedProfile, SigningState, cleanup_expired_certificate_state,
        cleanup_stale_profile_state, decode_provisioning_profile, expiration_date_has_passed_at,
        normalized_serial_number, orbit_managed_profile_name, read_certificate_serial,
    };
    use crate::apple::signing::CertificateOrigin;

    #[test]
    fn expiration_date_detection_treats_past_dates_as_expired() {
        let now = time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        assert!(expiration_date_has_passed_at(Some("2026-01-01T00:00:00Z"), now).unwrap());
        assert!(!expiration_date_has_passed_at(Some("2028-01-01T00:00:00Z"), now).unwrap());
        assert!(!expiration_date_has_passed_at(None, now).unwrap());
    }

    #[test]
    fn orbit_managed_profile_names_keep_orbit_prefix_and_type() {
        let name = orbit_managed_profile_name("dev.orbit.example", "MAC_APP_DEVELOPMENT");
        assert!(name.starts_with("*[orbit] dev.orbit.example MAC_APP_DEVELOPMENT "));
    }

    #[test]
    fn cleanup_stale_profile_state_removes_files_for_missing_remote_profiles() {
        let temp = tempdir().unwrap();
        let stale_path = temp.path().join("stale.mobileprovision");
        std::fs::write(&stale_path, b"profile").unwrap();

        let mut state = SigningState {
            certificates: Vec::new(),
            profiles: vec![ManagedProfile {
                id: "PROFILE123".to_owned(),
                profile_type: "IOS_APP_DEVELOPMENT".to_owned(),
                bundle_id: "dev.orbit.example".to_owned(),
                path: stale_path.clone(),
                uuid: None,
                certificate_ids: vec!["CERT123".to_owned()],
                device_ids: Vec::new(),
            }],
        };

        cleanup_stale_profile_state(
            &mut state,
            "dev.orbit.example",
            "IOS_APP_DEVELOPMENT",
            &HashSet::new(),
        );

        assert!(state.profiles.is_empty());
        assert!(!stale_path.exists());
    }

    #[test]
    fn cleanup_expired_certificate_state_removes_local_files() {
        let temp = tempdir().unwrap();
        let private_key_path = temp.path().join("cert.key");
        let certificate_der_path = temp.path().join("cert.cer");
        let p12_path = temp.path().join("cert.p12");
        std::fs::write(&private_key_path, b"key").unwrap();
        std::fs::write(&certificate_der_path, b"cert").unwrap();
        std::fs::write(&p12_path, b"p12").unwrap();

        let mut state = SigningState {
            certificates: vec![ManagedCertificate {
                id: "CERT123".to_owned(),
                certificate_type: "IOS_DEVELOPMENT".to_owned(),
                serial_number: "ABC123".to_owned(),
                origin: CertificateOrigin::Generated,
                display_name: None,
                system_keychain_path: None,
                system_signing_identity: None,
                private_key_path: private_key_path.clone(),
                certificate_der_path: certificate_der_path.clone(),
                p12_path: p12_path.clone(),
                p12_password_account: String::new(),
            }],
            profiles: Vec::new(),
        };

        cleanup_expired_certificate_state(
            &mut state,
            "IOS_DEVELOPMENT",
            &HashSet::from(["CERT123".to_owned()]),
            &HashSet::new(),
        );

        assert!(state.certificates.is_empty());
        assert!(!private_key_path.exists());
        assert!(!certificate_der_path.exists());
        assert!(!p12_path.exists());
    }

    #[test]
    fn decode_provisioning_profile_reads_plain_plist_devices() {
        let dictionary = Dictionary::from_iter([(
            "ProvisionedDevices".to_owned(),
            Value::Array(vec![
                Value::String("DEVICE-ONE".to_owned()),
                Value::String("DEVICE-TWO".to_owned()),
            ]),
        )]);
        let mut bytes = Vec::new();
        Value::Dictionary(dictionary)
            .to_writer_xml(&mut bytes)
            .unwrap();

        let decoded = decode_provisioning_profile(&bytes).unwrap();

        assert_eq!(decoded.device_udids, vec!["DEVICE-ONE", "DEVICE-TWO"]);
        assert!(decoded.certificate_serial_numbers.is_empty());
    }

    #[test]
    fn decode_provisioning_profile_reads_certificate_serials() {
        let temp = tempdir().unwrap();
        let key_path = temp.path().join("cert.key");
        let certificate_pem_path = temp.path().join("cert.pem");
        let certificate_der_path = temp.path().join("cert.cer");

        crate::util::run_command(Command::new("openssl").args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            key_path.to_str().unwrap(),
            "-out",
            certificate_pem_path.to_str().unwrap(),
            "-days",
            "1",
            "-nodes",
            "-subj",
            "/CN=Orbit Signing Fixture",
        ]))
        .unwrap();
        crate::util::run_command(Command::new("openssl").args([
            "x509",
            "-in",
            certificate_pem_path.to_str().unwrap(),
            "-outform",
            "DER",
            "-out",
            certificate_der_path.to_str().unwrap(),
        ]))
        .unwrap();

        let expected_serial =
            normalized_serial_number(&read_certificate_serial(&certificate_der_path).unwrap());
        let dictionary = Dictionary::from_iter([(
            "DeveloperCertificates".to_owned(),
            Value::Array(vec![Value::Data(
                std::fs::read(&certificate_der_path).unwrap(),
            )]),
        )]);
        let mut bytes = Vec::new();
        Value::Dictionary(dictionary)
            .to_writer_xml(&mut bytes)
            .unwrap();

        let decoded = decode_provisioning_profile(&bytes).unwrap();

        assert_eq!(decoded.certificate_serial_numbers, vec![expected_serial]);
    }
}
