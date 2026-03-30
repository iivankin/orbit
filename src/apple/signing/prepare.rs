use super::*;

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

fn asc_device_matches_platform(device_platform: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios => device_platform == "IOS",
        ApplePlatform::Tvos => device_platform == "TVOS",
        ApplePlatform::Visionos => device_platform == "VISIONOS",
        ApplePlatform::Watchos => device_platform == "WATCHOS",
        ApplePlatform::Macos => device_platform == "MAC_OS",
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
        remote_profile_ids.contains(&profile.id)
    });
}
