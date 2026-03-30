use super::*;

struct SigningSelection<'a> {
    target: &'a TargetManifest,
    platform: ApplePlatform,
    profile: ProfileManifest,
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
    let profile_kind = profile_type(selection.platform, &selection.profile)?;
    let managed_profile = state
        .profiles
        .iter()
        .rev()
        .find(|candidate| {
            candidate.bundle_id == selection.target.bundle_id
                && candidate.profile_type == profile_kind
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
