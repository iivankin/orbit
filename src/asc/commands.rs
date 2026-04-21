use std::collections::BTreeMap;

use anyhow::{Result, bail, ensure};
use asc_sync::{
    asc::AscClient,
    auth_store, build_settings, bundle, bundle_team, device, notarize, revoke, submit,
    sync::{Change, ChangeKind, Mode, SyncEngine, Workspace},
    system,
};

use crate::apple::build::receipt::{BuildReceipt, list_receipts, load_receipt};
use crate::apple::runtime::{apple_platform_from_cli, distribution_from_cli};
use crate::cli::{
    AscArgs, AscAuthCommand, AscCommand, AscDeviceCommand, AscRevokeTarget, AscSigningCommand, Cli,
    Command,
};
use crate::context::{AppContext, ProjectContext};
use crate::manifest::{DistributionKind, ProfileManifest};
use crate::util::{print_success, prompt_confirm, prompt_select};

use super::config;

pub(crate) fn execute(app: &AppContext, cli: &Cli) -> Result<()> {
    let Command::Asc(asc_args) = &cli.command else {
        unreachable!("asc::execute only handles `orbi asc` commands");
    };
    if matches!(
        &asc_args.command,
        AscCommand::Auth {
            command: AscAuthCommand::Import,
        }
    ) {
        return auth_store::import_auth_interactively();
    }
    let project = app.load_project(cli.manifest.as_deref())?;
    execute_project_command(&project, asc_args)
}

pub(crate) fn execute_project_command(project: &ProjectContext, args: &AscArgs) -> Result<()> {
    match &args.command {
        AscCommand::Auth { .. } => {
            unreachable!("`asc auth import` is handled before project loading")
        }
        AscCommand::Init => run_init(project),
        AscCommand::Validate => run_validate(project),
        AscCommand::Plan => run_sync_command(project, Mode::Plan),
        AscCommand::Apply => run_sync_command(project, Mode::Apply),
        AscCommand::Revoke(args) => {
            let embedded = config::materialize(project)?;
            revoke::run_with_workspace(args.target.into(), &embedded.workspace, &embedded.parsed)
        }
        AscCommand::Submit(args) => {
            let embedded = config::materialize(project)?;
            submit::run_with_config(&embedded.parsed, &args.file, args.bundle_id.as_deref())
        }
        AscCommand::Notarize(args) => {
            let embedded = config::materialize(project)?;
            notarize::run_with_config(&embedded.parsed, &args.file)
        }
        AscCommand::Device { command } => execute_device_command(project, command),
        AscCommand::Signing { command } => execute_signing_command(project, command),
    }
}

fn run_init(project: &ProjectContext) -> Result<()> {
    ensure!(
        project.app.interactive,
        "`orbi asc init` requires an interactive terminal"
    );
    ensure!(
        config::load_raw(project)?.is_none(),
        "`orbi asc init` requires a manifest without an `asc` section"
    );

    let asc = crate::commands::init::collect_asc_manifest_for_project(project)?;
    let manifest_path = config::initialize_asc(project, asc)?;
    print_success(format!("Wrote ASC config to {}", manifest_path.display()));
    println!("Next commands:");
    println!("  orbi asc apply");
    Ok(())
}

pub(crate) fn submit_artifact(
    project: &ProjectContext,
    args: &crate::cli::SubmitArgs,
) -> Result<()> {
    let receipt = resolve_submit_receipt(project, args)?;
    let embedded = config::materialize(project)?;

    match receipt.distribution {
        DistributionKind::AppStore | DistributionKind::MacAppStore => {
            let bundle_id = logical_bundle_id_for_receipt(&embedded.parsed, &receipt)?;
            submit::run_with_config(
                &embedded.parsed,
                &receipt.artifact_path,
                bundle_id.as_deref(),
            )
        }
        DistributionKind::DeveloperId => {
            notarize::run_with_config(&embedded.parsed, &receipt.artifact_path)
        }
        other => bail!(
            "receipt `{}` is not submit-eligible through Orbi ASC workflows for `{}`",
            receipt.id,
            other.as_str()
        ),
    }
}

pub(crate) fn revoke_for_clean(project: &ProjectContext) -> Result<()> {
    let Some(_) = config::load_raw(project)? else {
        println!("skipped ASC cleanup because orbi.json has no `asc` section");
        return Ok(());
    };
    let embedded = config::materialize(project)?;
    revoke::run_with_workspace(
        asc_sync::cli::RevokeTarget::All,
        &embedded.workspace,
        &embedded.parsed,
    )
}

fn execute_device_command(project: &ProjectContext, command: &AscDeviceCommand) -> Result<()> {
    let mut embedded = config::materialize(project)?;
    let apply = match command {
        AscDeviceCommand::Add(args) => args.apply,
        AscDeviceCommand::AddLocal(args) => args.apply,
    };
    let device = match command {
        AscDeviceCommand::Add(args) => device::add_with_config(
            &embedded.parsed,
            Some(&embedded.workspace),
            &device::DeviceAddRequest {
                name: args.name.clone(),
                logical_id: args.id.clone(),
                family: args.family.map(asc_sync::config::DeviceFamily::from),
                apply: args.apply,
                timeout_seconds: args.timeout_seconds,
            },
        )?,
        AscDeviceCommand::AddLocal(args) => device::add_local_with_config(
            &embedded.parsed,
            Some(&embedded.workspace),
            &device::DeviceAddLocalRequest {
                name: args.name.clone(),
                logical_id: args.id.clone(),
                current_mac: args.current_mac,
                family: args.family.map(asc_sync::config::DeviceFamily::from),
                udid: args.udid.clone(),
                apply: args.apply,
            },
        )?,
    };
    config::upsert_device(
        &mut embedded.raw,
        &device.logical_id,
        &device.display_name,
        device.family,
        &device.udid,
    )?;
    config::persist_from_materialized(project, embedded.raw)?;
    let manifest_path = config::active_manifest_path(project)?;
    if apply {
        println!(
            "Registered device {} ({}) in ASC, wrote it into {}, and updated developer state in the signing bundle.",
            device.display_name,
            device.udid,
            manifest_path.display()
        );
        println!("Re-run `orbi asc apply` to refresh development/ad-hoc profiles.");
    } else {
        println!(
            "Wrote device {} ({}) into {}.",
            device.display_name,
            device.udid,
            manifest_path.display()
        );
        println!("Run `orbi asc apply` when you want ASC registration and updated profiles.");
    }
    Ok(())
}

fn execute_signing_command(project: &ProjectContext, command: &AscSigningCommand) -> Result<()> {
    match command {
        AscSigningCommand::Import => run_signing_import(project),
        AscSigningCommand::PrintBuildSettings => run_signing_print_build_settings(project),
        AscSigningCommand::Merge(args) => {
            let embedded = config::materialize(project)?;
            bundle::merge_signing_bundle(
                &embedded.workspace.bundle_path,
                &args.base,
                &args.ours,
                &args.theirs,
            )?;
            println!(
                "Merged signing bundle into {}",
                embedded.workspace.bundle_path.display()
            );
            Ok(())
        }
    }
}

fn run_validate(project: &ProjectContext) -> Result<()> {
    let embedded = config::materialize(project)?;
    embedded.parsed.validate()?;
    validate_signing_bundle(&embedded.workspace, &embedded.parsed)?;
    println!("config is valid");
    Ok(())
}

fn run_signing_import(project: &ProjectContext) -> Result<()> {
    let embedded = config::materialize(project)?;
    let workspace = &embedded.workspace;
    let prepared_bundle = bundle_team::prepare_bundle_for_team(
        workspace,
        &embedded.parsed.team_id,
        bundle_team::BundleAccess::ReadOnly,
    )?;
    print_bundle_reset_notice(
        workspace,
        &embedded.parsed.team_id,
        &prepared_bundle.reset_from_team_ids,
    );

    for scope in asc_sync::scope::Scope::ALL {
        let Some(password) = prepared_bundle.passwords.get(&scope) else {
            println!("[{scope}] skipped: password unavailable");
            continue;
        };

        let mut runtime = workspace.create_runtime()?;
        let state = bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, password)?;
        let scoped_cert_names = state
            .certs
            .iter()
            .filter(|(_, certificate)| certificate_scope(&certificate.kind) == Some(scope))
            .map(|(logical_name, _)| logical_name.clone())
            .collect::<Vec<_>>();
        let scoped_profile_names = state
            .profiles
            .iter()
            .filter(|(_, profile)| profile_scope(&profile.kind) == Some(scope))
            .map(|(logical_name, _)| logical_name.clone())
            .collect::<Vec<_>>();
        if scoped_cert_names.is_empty() && scoped_profile_names.is_empty() {
            println!("[{scope}] skipped: no managed artifacts");
            continue;
        }

        let mut imported = 0usize;
        for logical_name in &scoped_cert_names {
            let pkcs12 = runtime.cert_bytes(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing PKCS#12 artifact for cert {logical_name}")
            })?;
            let p12_password = runtime.cert_password(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing PKCS#12 password for cert {logical_name}")
            })?;
            system::import_pkcs12_bytes_into_login_keychain(logical_name, pkcs12, p12_password)?;
            imported += 1;
        }
        let installed_profiles = install_profiles(&runtime, &state, scope)?;
        println!(
            "[{scope}] imported {imported} certificate(s), installed {installed_profiles} profile(s)"
        );
    }

    Ok(())
}

fn run_signing_print_build_settings(project: &ProjectContext) -> Result<()> {
    let embedded = config::materialize(project)?;
    let workspace = &embedded.workspace;
    let prepared_bundle = bundle_team::prepare_bundle_for_team(
        workspace,
        &embedded.parsed.team_id,
        bundle_team::BundleAccess::ReadOnly,
    )?;
    print_bundle_reset_notice(
        workspace,
        &embedded.parsed.team_id,
        &prepared_bundle.reset_from_team_ids,
    );

    let mut printed_any = false;
    for scope in asc_sync::scope::Scope::ALL {
        let Some(password) = prepared_bundle.passwords.get(&scope) else {
            println!("[{scope}] skipped: password unavailable");
            continue;
        };

        let mut runtime = workspace.create_runtime()?;
        let state = bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, password)?;
        let report = build_settings::collect_scope_build_settings(scope, &state);
        if report.profiles.is_empty() {
            println!("[{scope}] no managed provisioning profiles");
            continue;
        }

        printed_any = true;
        println!("[{scope}]");
        for profile in report.profiles {
            println!("profile: {}", profile.logical_name);
            println!("kind: {}", profile.kind);
            println!("bundle_id_ref: {}", profile.bundle_id_ref);
            println!("bundle_id: {}", profile.bundle_id);
            println!("uuid: {}", profile.uuid);
            if !profile.certs.is_empty() {
                println!("certs: {}", profile.certs.join(", "));
            }
            println!("CODE_SIGN_STYLE=Manual");
            println!("DEVELOPMENT_TEAM={}", profile.team_id);
            println!("PROVISIONING_PROFILE_SPECIFIER={}", profile.logical_name);
            println!("PROVISIONING_PROFILE={}", profile.uuid);
            if let Some(identity) = profile.code_sign_identity {
                println!("CODE_SIGN_IDENTITY={identity}");
            }
            println!();
        }
    }

    ensure!(printed_any, "no managed provisioning profiles found");
    Ok(())
}

fn run_sync_command(project: &ProjectContext, mode: Mode) -> Result<()> {
    let embedded = config::materialize(project)?;
    let config = &embedded.parsed;
    config.validate()?;

    let team_id = config.team_id.as_str();
    let auth = auth_store::resolve_auth_context(team_id)?;
    let client = AscClient::new(auth)?;
    let workspace = &embedded.workspace;

    if workspace.bundle_path.exists() {
        let prepared_bundle = bundle_team::prepare_bundle_for_team(
            workspace,
            team_id,
            match mode {
                Mode::Plan => bundle_team::BundleAccess::ReadOnly,
                Mode::Apply => bundle_team::BundleAccess::Mutating,
            },
        )?;
        print_bundle_reset_notice(workspace, team_id, &prepared_bundle.reset_from_team_ids);

        let active_scopes = asc_sync::scope::Scope::ALL
            .into_iter()
            .filter(|scope| prepared_bundle.passwords.contains_key(scope))
            .collect::<Vec<_>>();

        for scope in asc_sync::scope::Scope::ALL {
            let Some(password) = prepared_bundle.passwords.get(&scope) else {
                println!("[{scope}] skipped: password unavailable");
                continue;
            };
            run_sync_scope(
                mode,
                scope,
                &client,
                config,
                workspace,
                team_id,
                password,
                active_scopes.len() > 1,
            )?;
        }
        return Ok(());
    }

    let present_scopes = ordered_scopes(config);
    if mode == Mode::Plan {
        for scope in &present_scopes {
            run_sync_scope_without_bundle(
                mode,
                *scope,
                &client,
                config,
                workspace,
                team_id,
                present_scopes.len() > 1,
            )?;
        }
        return Ok(());
    }

    let passwords = bundle::bootstrap_bundle(&workspace.bundle_path, team_id)?;
    print_bootstrap_passwords(workspace, &passwords, project.app.interactive)?;

    if present_scopes.is_empty() {
        println!(
            "Initialized signing bundle at {}",
            workspace.bundle_path.display()
        );
        return Ok(());
    }

    for scope in &present_scopes {
        let password = passwords
            .get(scope)
            .expect("bootstrap bundle generated passwords for all scopes");
        run_sync_scope(
            mode,
            *scope,
            &client,
            config,
            workspace,
            team_id,
            password,
            present_scopes.len() > 1,
        )?;
    }

    Ok(())
}

fn run_sync_scope_without_bundle(
    mode: Mode,
    scope: asc_sync::scope::Scope,
    client: &AscClient,
    config: &asc_sync::config::Config,
    workspace: &Workspace,
    team_id: &str,
    print_scope_header: bool,
) -> Result<()> {
    if print_scope_header {
        println!("[{scope}]");
    }

    let mut runtime = workspace.create_runtime()?;
    let mut state = asc_sync::state::State::new(team_id);
    let engine = SyncEngine::new(mode, scope, client, config, &mut runtime, &mut state);
    let summary = engine.run()?;
    print_summary(&summary.changes);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_sync_scope(
    mode: Mode,
    scope: asc_sync::scope::Scope,
    client: &AscClient,
    config: &asc_sync::config::Config,
    workspace: &Workspace,
    team_id: &str,
    bundle_password: &age::secrecy::SecretString,
    print_scope_header: bool,
) -> Result<()> {
    if print_scope_header {
        println!("[{scope}]");
    }

    let mut runtime = workspace.create_runtime()?;
    let mut state =
        bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, bundle_password)?;
    state.ensure_team(team_id)?;

    let engine = SyncEngine::new(mode, scope, client, config, &mut runtime, &mut state);
    let summary = engine.run()?;
    print_summary(&summary.changes);

    if mode == Mode::Apply {
        bundle::write_scope(
            &workspace.bundle_path,
            &runtime,
            scope,
            &state,
            bundle_password,
        )?;
        let installed_profiles = install_profiles(&runtime, &state, scope)?;
        println!(
            "{} signing bundle saved to {}",
            scope,
            workspace.bundle_path.display()
        );
        println!("[{scope}] installed {installed_profiles} profile(s)");
    }

    Ok(())
}

fn install_profiles(
    runtime: &asc_sync::sync::RuntimeWorkspace,
    state: &asc_sync::state::State,
    scope: asc_sync::scope::Scope,
) -> Result<usize> {
    let mut installed = 0usize;
    for (logical_name, profile) in &state.profiles {
        if profile_scope(&profile.kind) != Some(scope) {
            continue;
        }
        let profile_bytes = runtime.profile_bytes(logical_name).ok_or_else(|| {
            anyhow::anyhow!("missing provisioning profile artifact for profile {logical_name}")
        })?;
        system::install_profile_bytes(&profile.uuid, profile_bytes)?;
        installed += 1;
    }
    Ok(installed)
}

fn certificate_scope(kind: &str) -> Option<asc_sync::scope::Scope> {
    match kind {
        "DEVELOPMENT" => Some(asc_sync::scope::Scope::Developer),
        "DISTRIBUTION" | "DEVELOPER_ID_APPLICATION" | "DEVELOPER_ID_APPLICATION_G2" => {
            Some(asc_sync::scope::Scope::Release)
        }
        _ => None,
    }
}

fn profile_scope(kind: &str) -> Option<asc_sync::scope::Scope> {
    match kind {
        "IOS_APP_DEVELOPMENT"
        | "IOS_APP_ADHOC"
        | "MAC_APP_DEVELOPMENT"
        | "MAC_CATALYST_APP_DEVELOPMENT" => Some(asc_sync::scope::Scope::Developer),
        "IOS_APP_STORE"
        | "MAC_APP_STORE"
        | "MAC_APP_DIRECT"
        | "MAC_CATALYST_APP_STORE"
        | "MAC_CATALYST_APP_DIRECT" => Some(asc_sync::scope::Scope::Release),
        _ => None,
    }
}

fn print_summary(changes: &[Change]) {
    if changes.is_empty() {
        println!("No changes.");
        return;
    }

    for change in changes {
        println!(
            "{:<7} {:<40} {}",
            render_change_kind(&change.kind),
            change.subject,
            change.detail
        );
    }
}

fn ordered_scopes(config: &asc_sync::config::Config) -> Vec<asc_sync::scope::Scope> {
    let present = config.present_scopes();
    asc_sync::scope::Scope::ALL
        .into_iter()
        .filter(|scope| present.contains(scope))
        .collect()
}

fn print_bootstrap_passwords(
    workspace: &Workspace,
    passwords: &BTreeMap<asc_sync::scope::Scope, age::secrecy::SecretString>,
    interactive: bool,
) -> Result<()> {
    use age::secrecy::ExposeSecret;

    println!(
        "Generated bundle passwords for {}:",
        workspace.bundle_path.display()
    );
    for scope in asc_sync::scope::Scope::ALL {
        let password = passwords
            .get(&scope)
            .expect("bootstrap passwords contain both scopes");
        println!("{scope}: {}", password.expose_secret());
    }
    println!("Passwords were saved to the local asc-sync cache (~/.asc-sync/bundle-passwords/).");
    println!(
        "Save these passwords for sharing with your team and CI; they are required to unlock signing.ascbundle on other machines."
    );
    if interactive {
        ensure!(
            prompt_confirm(
                "Have you saved the developer and release bundle passwords?",
                false,
            )?,
            "save the generated bundle passwords before continuing; they are required to unlock signing.ascbundle on other machines"
        );
    }
    Ok(())
}

fn print_bundle_reset_notice(workspace: &Workspace, team_id: &str, previous_team_ids: &[String]) {
    if previous_team_ids.is_empty() {
        return;
    }

    println!(
        "Reset {} from team(s) {} to {}.",
        workspace.bundle_path.display(),
        previous_team_ids.join(", "),
        team_id
    );
}

fn validate_signing_bundle(workspace: &Workspace, config: &asc_sync::config::Config) -> Result<()> {
    if !workspace.bundle_path.exists() {
        return Ok(());
    }

    let state = bundle::load_state(&workspace.bundle_path)?;
    state.ensure_team(&config.team_id)?;
    let required_scopes = signing_scopes_in_state(&state);
    if required_scopes.is_empty() {
        return Ok(());
    }

    let unlocked = bundle::resolve_existing_passwords(&workspace.bundle_path, &required_scopes)?;
    for scope in &required_scopes {
        ensure!(
            unlocked.contains_key(scope),
            "missing {scope} bundle password; cannot validate {scope} signing artifacts"
        );
    }

    for scope in required_scopes {
        let password = unlocked
            .get(&scope)
            .expect("required scope password is present");
        let mut runtime = workspace.create_runtime()?;
        let state = bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, password)?;

        for (logical_name, certificate) in &state.certs {
            if managed_certificate_scope(&certificate.kind) != Some(scope) {
                continue;
            }
            let pkcs12 = runtime.cert_bytes(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing PKCS#12 artifact for cert {logical_name}")
            })?;
            let p12_password = runtime.cert_password(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing PKCS#12 password for cert {logical_name}")
            })?;
            ensure!(
                !system::pkcs12_is_expired(pkcs12, p12_password)?,
                "certificate {logical_name} is expired"
            );
        }

        for (logical_name, profile) in &state.profiles {
            if managed_profile_scope(&profile.kind) != Some(scope) {
                continue;
            }
            let profile_bytes = runtime.profile_bytes(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing provisioning profile artifact for profile {logical_name}")
            })?;
            ensure!(
                !system::provisioning_profile_is_expired(profile_bytes)?,
                "provisioning profile {logical_name} ({}) is expired",
                profile.uuid
            );
        }
    }

    Ok(())
}

fn signing_scopes_in_state(state: &asc_sync::state::State) -> Vec<asc_sync::scope::Scope> {
    asc_sync::scope::Scope::ALL
        .into_iter()
        .filter(|scope| {
            state
                .certs
                .values()
                .any(|certificate| managed_certificate_scope(&certificate.kind) == Some(*scope))
                || state
                    .profiles
                    .values()
                    .any(|profile| managed_profile_scope(&profile.kind) == Some(*scope))
        })
        .collect()
}

fn managed_certificate_scope(kind: &str) -> Option<asc_sync::scope::Scope> {
    match kind {
        "DEVELOPMENT" => Some(asc_sync::scope::Scope::Developer),
        "DISTRIBUTION" | "DEVELOPER_ID_APPLICATION" | "DEVELOPER_ID_APPLICATION_G2" => {
            Some(asc_sync::scope::Scope::Release)
        }
        _ => None,
    }
}

fn managed_profile_scope(kind: &str) -> Option<asc_sync::scope::Scope> {
    match kind {
        "IOS_APP_DEVELOPMENT"
        | "IOS_APP_ADHOC"
        | "MAC_APP_DEVELOPMENT"
        | "MAC_CATALYST_APP_DEVELOPMENT" => Some(asc_sync::scope::Scope::Developer),
        "IOS_APP_STORE"
        | "MAC_APP_STORE"
        | "MAC_APP_DIRECT"
        | "MAC_CATALYST_APP_STORE"
        | "MAC_CATALYST_APP_DIRECT" => Some(asc_sync::scope::Scope::Release),
        _ => None,
    }
}

fn render_change_kind(kind: &ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Create => "create",
        ChangeKind::Update => "update",
        ChangeKind::Replace => "replace",
        ChangeKind::Delete => "delete",
    }
}

fn resolve_submit_receipt(
    project: &ProjectContext,
    args: &crate::cli::SubmitArgs,
) -> Result<BuildReceipt> {
    let requested_platform = args.platform.map(apple_platform_from_cli);
    let requested_distribution = distribution_from_cli(args.distribution);

    if let Some(receipt_path) = &args.receipt {
        let receipt = load_receipt(receipt_path)?;
        if !receipt.submit_eligible {
            bail!(
                "receipt `{}` is not submit-eligible because it was built for `{:?}` distribution",
                receipt.id,
                receipt.distribution
            );
        }
        if requested_platform.is_some_and(|platform| receipt.platform != platform) {
            bail!(
                "receipt `{}` targets platform `{}`, not the requested `{}`",
                receipt.id,
                receipt.platform,
                requested_platform
                    .map(|platform| platform.to_string())
                    .unwrap_or_default()
            );
        }
        if requested_distribution.is_some_and(|distribution| receipt.distribution != distribution) {
            bail!(
                "receipt `{}` uses distribution `{}`, not the requested `{}`",
                receipt.id,
                receipt.distribution.as_str(),
                requested_distribution
                    .map(DistributionKind::as_str)
                    .unwrap_or_default()
            );
        }
        return Ok(receipt);
    }

    let mut receipts = list_receipts(
        &project.project_paths.receipts_dir,
        requested_platform,
        requested_distribution,
    )?;
    receipts.retain(|receipt| receipt.submit_eligible);
    receipts.sort_by(|left, right| right.created_at_unix.cmp(&left.created_at_unix));
    if receipts.is_empty() {
        bail!("could not find a submit-eligible build receipt");
    }
    if receipts.len() == 1 || !project.app.interactive {
        return Ok(receipts.remove(0));
    }

    let labels = receipts.iter().map(receipt_label).collect::<Vec<_>>();
    let index = prompt_select("Select a build receipt to submit", &labels)?;
    Ok(receipts.remove(index))
}

fn logical_bundle_id_for_receipt(
    config: &asc_sync::config::Config,
    receipt: &BuildReceipt,
) -> Result<Option<String>> {
    let matches = config
        .bundle_ids
        .iter()
        .filter(|(_, spec)| spec.bundle_id == receipt.bundle_id)
        .map(|(logical_name, _)| logical_name.clone())
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => {
            ensure!(
                config.bundle_ids.len() <= 1,
                "bundle `{}` is not declared in `asc.bundle_ids`; add a matching entry or pass `orbi asc submit --bundle-id ...` explicitly",
                receipt.bundle_id
            );
            Ok(None)
        }
        [logical_name] => Ok(Some(logical_name.clone())),
        _ => bail!(
            "multiple `asc.bundle_ids` entries point at {}; submit through `orbi asc submit --bundle-id ...`",
            receipt.bundle_id
        ),
    }
}

fn receipt_label(receipt: &BuildReceipt) -> String {
    format!(
        "{} | {} | {} | {} | {}",
        receipt.id,
        receipt.target,
        profile_description(&ProfileManifest::new(
            receipt.configuration,
            receipt.distribution
        )),
        receipt.destination,
        receipt.artifact_path.display()
    )
}

fn profile_description(profile: &ProfileManifest) -> String {
    format!(
        "{} {}",
        profile.distribution.as_str(),
        profile.configuration.as_str()
    )
}

impl From<crate::cli::AscDeviceFamily> for asc_sync::config::DeviceFamily {
    fn from(value: crate::cli::AscDeviceFamily) -> Self {
        match value {
            crate::cli::AscDeviceFamily::Ios => Self::Ios,
            crate::cli::AscDeviceFamily::Ipados => Self::Ipados,
            crate::cli::AscDeviceFamily::Watchos => Self::Watchos,
            crate::cli::AscDeviceFamily::Tvos => Self::Tvos,
            crate::cli::AscDeviceFamily::Visionos => Self::Visionos,
            crate::cli::AscDeviceFamily::Macos => Self::Macos,
        }
    }
}

impl From<AscRevokeTarget> for asc_sync::cli::RevokeTarget {
    fn from(value: AscRevokeTarget) -> Self {
        match value {
            AscRevokeTarget::Dev => Self::Dev,
            AscRevokeTarget::Release => Self::Release,
            AscRevokeTarget::All => Self::All,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{certificate_scope, execute, managed_certificate_scope};
    use crate::cli::{AscArgs, AscAuthCommand, AscCommand, Cli, Command};
    use crate::context::{AppContext, GlobalPaths};

    #[test]
    fn auth_import_does_not_require_loading_a_project() {
        let temp = tempfile::tempdir().unwrap();
        let app = AppContext {
            cwd: temp.path().to_path_buf(),
            interactive: false,
            verbose: false,
            manifest_env: None,
            global_paths: GlobalPaths {
                data_dir: temp.path().join("data"),
                cache_dir: temp.path().join("cache"),
                schema_dir: temp.path().join("schemas"),
            },
        };
        let cli = Cli {
            manifest: Some(PathBuf::from("missing-orbi.json")),
            env: None,
            non_interactive: true,
            verbose: false,
            command: Command::Asc(Box::new(AscArgs {
                command: AscCommand::Auth {
                    command: AscAuthCommand::Import,
                },
            })),
        };

        let error = execute(&app, &cli).unwrap_err().to_string();
        assert!(error.contains("auth import requires an interactive terminal"));
        assert!(!error.contains("could not find `orbi.json`"));
        assert!(!error.contains("failed to canonicalize"));
    }

    #[test]
    fn installer_certificates_are_ignored_after_disk_image_cutover() {
        assert_eq!(certificate_scope("MAC_INSTALLER_DISTRIBUTION"), None);
        assert_eq!(
            managed_certificate_scope("MAC_INSTALLER_DISTRIBUTION"),
            None
        );
    }
}
