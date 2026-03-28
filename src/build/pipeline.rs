use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use plist::{Dictionary, Value};
use serde::Deserialize;
use tempfile::{NamedTempFile, tempdir};

use crate::build::receipt::{BuildReceipt, list_receipts, write_receipt};
use crate::build::toolchain::{DestinationKind, Toolchain};
use crate::cli::{BuildArgs, RunArgs, SubmitArgs};
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, DistributionKind, ExtensionManifest, IosDeviceFamily, IosInterfaceOrientation,
    IosTargetManifest, ProfileManifest, SwiftPackageDependency, TargetKind, TargetManifest,
    XcframeworkDependency,
};
use crate::util::{
    CliSpinner, collect_files_with_extensions, command_output, command_output_allow_failure,
    copy_dir_recursive, copy_file, ensure_dir, ensure_parent_dir, prompt_select, resolve_path,
    run_command,
};

#[derive(Debug, Clone)]
struct BuildRequest {
    target_name: String,
    platform: ApplePlatform,
    profile_name: String,
    destination: DestinationKind,
    output: Option<PathBuf>,
    provisioning_udids: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct BuiltTarget {
    target_name: String,
    target_kind: TargetKind,
    bundle_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ProductLayout {
    product_path: PathBuf,
    binary_path: PathBuf,
    module_output_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
struct ExternalLinkInputs {
    module_search_paths: Vec<PathBuf>,
    framework_search_paths: Vec<PathBuf>,
    library_search_paths: Vec<PathBuf>,
    link_frameworks: Vec<String>,
    weak_frameworks: Vec<String>,
    link_libraries: Vec<String>,
    embedded_payloads: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub receipt: BuildReceipt,
    pub receipt_path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default)]
struct ResourceWorkSummary {
    asset_catalogs: usize,
    interface_resources: usize,
    strings_files: usize,
    core_data_models: usize,
    copied_resources: usize,
}

impl ResourceWorkSummary {
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.asset_catalogs > 0 {
            parts.push(format!("{} asset catalog(s)", self.asset_catalogs));
        }
        if self.interface_resources > 0 {
            parts.push(format!("{} interface file(s)", self.interface_resources));
        }
        if self.strings_files > 0 {
            parts.push(format!("{} strings file(s)", self.strings_files));
        }
        if self.core_data_models > 0 {
            parts.push(format!("{} Core Data model(s)", self.core_data_models));
        }
        if self.copied_resources > 0 {
            parts.push(format!("{} copied resource(s)", self.copied_resources));
        }
        if parts.is_empty() {
            return "no resource work".to_owned();
        }
        parts.join(", ")
    }
}

fn build_progress_step<T, F, G>(
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

fn build_requires_signing(profile: &ProfileManifest, destination: DestinationKind) -> bool {
    destination == DestinationKind::Device
        || !matches!(profile.distribution, DistributionKind::Development)
}

pub fn build_artifact(project: &ProjectContext, args: &BuildArgs) -> Result<()> {
    let target = resolve_requested_target(project, args.target.as_deref())?;
    let platform = project.manifest.resolve_platform_for_target(target, None)?;
    let profile_name = resolve_profile_name(
        project,
        platform,
        args.profile.as_deref(),
        None,
        "Select a build profile",
    )?;
    let profile = project.manifest.profile_for(platform, &profile_name)?;
    let request = BuildRequest {
        target_name: target.name.clone(),
        platform,
        profile_name,
        destination: resolve_destination(project, platform, args.simulator, args.device, profile)?,
        output: args.output.clone(),
        provisioning_udids: None,
    };
    if build_requires_signing(profile, request.destination) {
        crate::apple::auth::ensure_project_authenticated(project)?;
    }

    let spinner = CliSpinner::new(format!(
        "Building {} for {} ({})",
        request.target_name,
        request.profile_name,
        request.destination.as_str()
    ));
    let outcome = match build_project(project, &request) {
        Ok(outcome) => outcome,
        Err(error) => {
            spinner.finish_clear();
            return Err(error);
        }
    };
    spinner.finish_success(format!(
        "Built {} for {}.",
        outcome.receipt.target, outcome.receipt.profile
    ));
    println!("artifact: {}", outcome.receipt.artifact_path.display());
    println!("receipt: {}", outcome.receipt_path.display());
    Ok(())
}

pub fn run_on_destination(project: &ProjectContext, args: &RunArgs) -> Result<()> {
    let target = resolve_requested_target(project, args.target.as_deref())?;
    let platform = project.manifest.resolve_platform_for_target(target, None)?;
    validate_run_platform(platform)?;
    let profile_name = resolve_profile_name(
        project,
        platform,
        args.profile.as_deref(),
        Some("development"),
        "Select a run profile",
    )?;
    let profile = project.manifest.profile_for(platform, &profile_name)?;
    validate_run_distribution(profile)?;
    let destination = resolve_destination(project, platform, args.simulator, args.device, profile)?;
    if args.device_id.is_some() && destination != DestinationKind::Device {
        bail!("--device-id can only be used together with a physical-device run");
    }
    let selected_device =
        if destination == DestinationKind::Device && platform != ApplePlatform::Macos {
            Some(select_physical_device(
                project,
                args.device_id.as_deref(),
                platform,
            )?)
        } else {
            None
        };
    let request = BuildRequest {
        target_name: target.name.clone(),
        platform,
        profile_name,
        destination,
        output: None,
        provisioning_udids: selected_device
            .as_ref()
            .map(|device| vec![device.hardware_properties.udid.clone()]),
    };
    if build_requires_signing(profile, request.destination) {
        crate::apple::auth::ensure_project_authenticated(project)?;
    }

    let spinner = CliSpinner::new(format!(
        "Building {} for {} ({})",
        request.target_name,
        request.profile_name,
        request.destination.as_str()
    ));
    let outcome = match build_project(project, &request) {
        Ok(outcome) => outcome,
        Err(error) => {
            spinner.finish_clear();
            return Err(error);
        }
    };
    spinner.finish_success(format!(
        "Built {} for {}.",
        outcome.receipt.target, outcome.receipt.profile
    ));
    match (
        outcome.receipt.platform,
        outcome.receipt.destination.as_str(),
        args.debug,
    ) {
        (ApplePlatform::Macos, _, false) => run_on_macos(&outcome.receipt),
        (ApplePlatform::Macos, _, true) => debug_on_macos(&outcome.receipt),
        (_, "simulator", false) => run_on_simulator(project, &outcome.receipt),
        (_, "simulator", true) => debug_on_simulator(project, &outcome.receipt),
        (_, "device", false) => run_on_device(
            selected_device
                .as_ref()
                .context("device run requested without a selected physical device")?,
            &outcome.receipt,
        ),
        (_, "device", true) => debug_on_device(
            project,
            selected_device
                .as_ref()
                .context("device run requested without a selected physical device")?,
            &outcome.receipt,
        ),
        (_, other, _) => bail!("unsupported run destination `{other}`"),
    }
}

pub fn submit_artifact(project: &ProjectContext, args: &SubmitArgs) -> Result<()> {
    let receipt = resolve_submit_receipt(project, args)?;

    crate::apple::auth::best_effort_app_store_authenticate(project)?;

    match receipt.platform {
        ApplePlatform::Ios
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => submit_with_altool(project, &receipt, args.wait),
        ApplePlatform::Macos => match receipt.distribution {
            DistributionKind::DeveloperId => submit_with_notarytool(project, &receipt, args.wait),
            DistributionKind::MacAppStore => submit_with_altool(project, &receipt, args.wait),
            other => bail!("macOS submit is not supported for {:?} builds", other),
        },
    }
}

fn build_project(project: &ProjectContext, request: &BuildRequest) -> Result<BuildOutcome> {
    let root_target = project
        .manifest
        .resolve_target(Some(&request.target_name))?;
    let platform = request.platform;
    let platform_manifest = project
        .manifest
        .platforms
        .get(&platform)
        .context("platform configuration missing from manifest")?;
    let profile = project
        .manifest
        .profile_for(platform, &request.profile_name)?;

    let toolchain = Toolchain::resolve(
        platform,
        platform_manifest.deployment_target.as_str(),
        request.destination,
    )?;

    let build_root = project
        .project_paths
        .build_dir
        .join(platform.to_string())
        .join(&request.profile_name)
        .join(toolchain.destination.as_str());
    ensure_dir(&build_root)?;

    let ordered_targets = project.manifest.topological_targets(&root_target.name)?;
    let mut built_targets = HashMap::new();
    let signing_required = build_requires_signing(profile, request.destination);
    for target in &ordered_targets {
        let built = compile_target(
            project,
            &toolchain,
            target,
            &build_root,
            &request.profile_name,
            profile,
        )?;
        built_targets.insert(target.name.clone(), built);
    }

    for target in &ordered_targets {
        if target.kind.is_bundle() {
            let built_targets_snapshot = built_targets.clone();
            let built_target = built_targets
                .get_mut(&target.name)
                .with_context(|| format!("missing built target `{}`", target.name))?;
            embed_dependencies(project, target, &built_targets_snapshot, built_target)?;
        }

        if signing_required && target.kind.is_bundle() {
            let built_target = built_targets
                .get(&target.name)
                .with_context(|| format!("missing built target `{}`", target.name))?;
            let material = crate::apple::signing::prepare_signing(
                project,
                target,
                platform,
                profile,
                request.provisioning_udids.clone(),
            )?;
            crate::apple::signing::sign_bundle(&built_target.bundle_path, &material)?;
        }
    }

    let root_target_built = built_targets
        .get(&root_target.name)
        .context("root target did not build")?;
    let artifact_path = export_artifact(
        project,
        platform,
        root_target_built,
        &build_root,
        request.output.as_deref(),
        profile,
    )?;

    let mut receipt = BuildReceipt::new(
        &root_target.name,
        platform,
        &request.profile_name,
        profile.distribution,
        request.destination.as_str(),
        &root_target.bundle_id,
        root_target_built.bundle_path.clone(),
        artifact_path,
    );
    if !matches!(root_target.kind, TargetKind::App | TargetKind::WatchApp) {
        receipt.submit_eligible = false;
    }
    let receipt_path = write_receipt(&project.project_paths.receipts_dir, &receipt)?;

    Ok(BuildOutcome {
        receipt,
        receipt_path,
    })
}

fn compile_target(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    build_root: &Path,
    profile_name: &str,
    profile: &ProfileManifest,
) -> Result<BuiltTarget> {
    let target_dir = build_root.join(&target.name);
    let intermediates_dir = target_dir.join("intermediates");
    let product = product_layout(&target_dir, &intermediates_dir, target, toolchain);
    ensure_dir(&intermediates_dir)?;
    if target.kind.is_bundle() {
        ensure_dir(&product.product_path)?;
    } else {
        ensure_parent_dir(&product.product_path)?;
    }

    let package_outputs = if target.swift_packages.is_empty() {
        Vec::new()
    } else {
        build_progress_step(
            format!("Compiling Swift packages for target `{}`", target.name),
            |outputs: &Vec<PackageBuildOutput>| {
                format!(
                    "Compiled {} Swift package product(s) for target `{}`.",
                    outputs.len(),
                    target.name
                )
            },
            || compile_swift_packages(project, toolchain, profile, &intermediates_dir, target),
        )?
    };
    let external_link_inputs =
        resolve_external_link_inputs(project, toolchain, &intermediates_dir, target)?;
    let c_objects = build_progress_step(
        format!("Compiling C-family sources for target `{}`", target.name),
        |objects: &Vec<PathBuf>| {
            if objects.is_empty() {
                format!(
                    "No C-family sources were compiled for target `{}`.",
                    target.name
                )
            } else {
                format!(
                    "Compiled {} C-family object file(s) for target `{}`.",
                    objects.len(),
                    target.name
                )
            }
        },
        || compile_c_family_sources(project, toolchain, profile, &intermediates_dir, target),
    )?;
    let swift_sources = resolve_target_sources(project, target, &["swift"])?;

    if !swift_sources.is_empty() {
        build_progress_step(
            format!("Compiling Swift target `{}`", target.name),
            |_| {
                format!(
                    "Compiled {} Swift source file(s) for target `{}`.",
                    swift_sources.len(),
                    target.name
                )
            },
            || {
                compile_swift_target(
                    toolchain,
                    profile,
                    target.kind,
                    &intermediates_dir,
                    &swift_sources,
                    &package_outputs,
                    &external_link_inputs,
                    &c_objects,
                    &target.name,
                    &product.binary_path,
                    product.module_output_path.as_deref(),
                )
            },
        )?;
    } else if !c_objects.is_empty() {
        build_progress_step(
            format!("Linking native target `{}`", target.name),
            |_| {
                format!(
                    "Linked {} object file(s) into target `{}`.",
                    c_objects.len(),
                    target.name
                )
            },
            || {
                link_native_target(
                    toolchain,
                    profile,
                    target.kind,
                    &external_link_inputs,
                    &c_objects,
                    &product.binary_path,
                )
            },
        )?;
    } else {
        bail!(
            "target `{}` did not resolve any compilable sources",
            target.name
        );
    }

    if target.kind.is_bundle() {
        relocate_bundle_debug_artifacts(&target_dir, &product.product_path, &product.binary_path)?;
    }

    if needs_info_plist(target.kind) {
        build_progress_step(
            format!("Writing Info.plist for target `{}`", target.name),
            |_| format!("Wrote Info.plist for target `{}`.", target.name),
            || {
                write_info_plist(
                    project,
                    toolchain,
                    target,
                    &product.product_path,
                    profile_name,
                )
            },
        )?;
    }
    if target.kind.is_bundle() {
        if !target.resources.is_empty() {
            build_progress_step(
                format!("Processing resources for target `{}`", target.name),
                |summary: &ResourceWorkSummary| {
                    format!(
                        "Processed resources for target `{}`: {}.",
                        target.name,
                        summary.describe()
                    )
                },
                || process_resources(project, toolchain, target, &product.product_path),
            )?;
        }
        if !external_link_inputs.embedded_payloads.is_empty() {
            build_progress_step(
                format!("Embedding external payloads for target `{}`", target.name),
                |_| {
                    format!(
                        "Embedded {} external payload(s) for target `{}`.",
                        external_link_inputs.embedded_payloads.len(),
                        target.name
                    )
                },
                || embed_external_payloads(&external_link_inputs, &product.product_path),
            )?;
        }
    }

    Ok(BuiltTarget {
        target_name: target.name.clone(),
        target_kind: target.kind,
        bundle_path: product.product_path,
    })
}

fn relocate_bundle_debug_artifacts(
    target_dir: &Path,
    bundle_root: &Path,
    binary_path: &Path,
) -> Result<()> {
    let sidecar_dsym = binary_path.with_extension("dSYM");
    if !sidecar_dsym.exists() || !sidecar_dsym.starts_with(bundle_root) {
        return Ok(());
    }

    let destination = target_dir.join(
        sidecar_dsym
            .file_name()
            .context("bundle debug artifact is missing a file name")?,
    );
    remove_existing_path(&destination)?;
    fs::rename(&sidecar_dsym, &destination).with_context(|| {
        format!(
            "failed to move debug symbols from {} to {}",
            sidecar_dsym.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn resolve_requested_target<'a>(
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
    let index = prompt_select("Select a target", &labels)?;
    Ok(candidates.remove(index))
}

fn resolve_profile_name(
    project: &ProjectContext,
    platform: ApplePlatform,
    requested_profile: Option<&str>,
    default_profile: Option<&str>,
    prompt: &str,
) -> Result<String> {
    if let Some(requested_profile) = requested_profile {
        let _ = project.manifest.profile_for(platform, requested_profile)?;
        return Ok(requested_profile.to_owned());
    }

    if let Some(default_profile) = default_profile {
        if project
            .manifest
            .profile_for(platform, default_profile)
            .is_ok()
        {
            return Ok(default_profile.to_owned());
        }
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

    let index = prompt_select(prompt, &profiles)?;
    Ok(profiles[index].clone())
}

fn resolve_destination(
    project: &ProjectContext,
    platform: ApplePlatform,
    simulator: bool,
    device: bool,
    profile: &ProfileManifest,
) -> Result<DestinationKind> {
    if simulator && device {
        bail!("--simulator and --device cannot be used together");
    }
    if platform == ApplePlatform::Macos {
        if simulator {
            bail!("macOS builds do not support `--simulator`");
        }
        return Ok(DestinationKind::Device);
    }
    if device {
        return Ok(DestinationKind::Device);
    }
    if simulator {
        return Ok(DestinationKind::Simulator);
    }
    if matches!(profile.distribution, DistributionKind::Development) && project.app.interactive {
        let options = ["Simulator", "Physical device"];
        let index = prompt_select("Select a destination", &options)?;
        return Ok(match index {
            0 => DestinationKind::Simulator,
            _ => DestinationKind::Device,
        });
    }
    Ok(default_destination_for_profile(platform, profile))
}

fn default_destination_for_profile(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> DestinationKind {
    if platform == ApplePlatform::Macos {
        return DestinationKind::Device;
    }

    match profile.distribution {
        DistributionKind::Development => DestinationKind::Simulator,
        DistributionKind::AdHoc
        | DistributionKind::AppStore
        | DistributionKind::DeveloperId
        | DistributionKind::MacAppStore => DestinationKind::Device,
    }
}

fn validate_run_distribution(profile: &ProfileManifest) -> Result<()> {
    match profile.distribution {
        DistributionKind::Development | DistributionKind::AdHoc => Ok(()),
        DistributionKind::AppStore
        | DistributionKind::DeveloperId
        | DistributionKind::MacAppStore => {
            bail!("`orbit run` only supports development or ad-hoc profiles")
        }
    }
}

fn validate_run_platform(platform: ApplePlatform) -> Result<()> {
    match platform {
        ApplePlatform::Ios
        | ApplePlatform::Macos
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => Ok(()),
    }
}

fn resolve_submit_receipt(project: &ProjectContext, args: &SubmitArgs) -> Result<BuildReceipt> {
    if let Some(receipt_path) = &args.receipt {
        let receipt = crate::build::receipt::load_receipt(receipt_path)?;
        if !receipt.submit_eligible {
            bail!(
                "receipt `{}` is not submit-eligible because it was built for `{:?}` distribution",
                receipt.id,
                receipt.distribution
            );
        }
        return Ok(receipt);
    }

    let mut receipts = list_receipts(
        &project.project_paths.receipts_dir,
        args.target.as_deref(),
        args.profile.as_deref(),
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

fn receipt_label(receipt: &BuildReceipt) -> String {
    format!(
        "{} | {} | {} | {} | {}",
        receipt.id,
        receipt.target,
        receipt.profile,
        receipt.destination,
        receipt.artifact_path.display()
    )
}

fn resolve_target_sources(
    project: &ProjectContext,
    target: &TargetManifest,
    extensions: &[&str],
) -> Result<Vec<PathBuf>> {
    let mut sources = Vec::new();
    for root in project.manifest.shared_source_roots() {
        let path = resolve_path(&project.root, &root);
        if path.exists() {
            sources.extend(collect_files_with_extensions(&path, extensions)?);
        }
    }
    for root in &target.sources {
        let path = resolve_path(&project.root, root);
        sources.extend(collect_files_with_extensions(&path, extensions)?);
    }
    sources.sort();
    sources.dedup();
    Ok(sources)
}

fn compile_c_family_sources(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    target: &TargetManifest,
) -> Result<Vec<PathBuf>> {
    let mut object_files = Vec::new();
    let specs = [
        ("c", false),
        ("m", false),
        ("mm", true),
        ("cpp", true),
        ("cc", true),
        ("cxx", true),
    ];

    for (extension, is_cpp) in specs {
        for source in resolve_target_sources(project, target, &[extension])? {
            let object_name = source
                .file_name()
                .and_then(OsStr::to_str)
                .map(|value| format!("{value}.o"))
                .context("failed to derive object file name")?;
            let object_path = intermediates_dir.join(object_name);
            let mut command = toolchain.clang(is_cpp);
            command.arg("-target").arg(&toolchain.target_triple);
            command.arg("-isysroot").arg(&toolchain.sdk_path);
            command.arg("-c").arg(&source);
            command.arg("-o").arg(&object_path);
            if matches!(profile.configuration.as_str(), "debug") {
                command.arg("-g");
            } else {
                command.arg("-O2");
            }
            run_command(&mut command)?;
            object_files.push(object_path);
        }
    }

    Ok(object_files)
}

fn compile_swift_target(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    target_kind: TargetKind,
    _intermediates_dir: &Path,
    swift_sources: &[PathBuf],
    package_outputs: &[PackageBuildOutput],
    external_link_inputs: &ExternalLinkInputs,
    object_files: &[PathBuf],
    module_name: &str,
    product_path: &Path,
    module_output_path: Option<&Path>,
) -> Result<()> {
    let mut command = toolchain.swiftc();
    command.arg("-parse-as-library");
    command.arg("-target").arg(&toolchain.target_triple);
    command.arg("-module-name").arg(module_name);
    if matches!(profile.configuration.as_str(), "debug") {
        command.args(["-Onone", "-g"]);
    } else {
        command.arg("-O");
    }
    match target_kind {
        TargetKind::StaticLibrary => {
            command.arg("-emit-library");
            command.arg("-static");
        }
        TargetKind::DynamicLibrary | TargetKind::Framework => {
            command.arg("-emit-library");
        }
        _ => {}
    }
    if matches!(
        target_kind,
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Framework
    ) {
        command.arg("-emit-module");
        if let Some(module_output_path) = module_output_path {
            ensure_parent_dir(module_output_path)?;
            command.arg("-emit-module-path").arg(module_output_path);
        }
    }
    command.arg("-o").arg(product_path);
    if matches!(
        target_kind,
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension
    ) {
        // Extension bundles do not define `main`; the system loader enters through NSExtensionMain.
        command.args(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"]);
    }
    for package in package_outputs {
        command.arg("-I").arg(&package.module_dir);
        command.arg("-L").arg(&package.library_dir);
        for library in &package.link_libraries {
            command.arg("-l").arg(library);
        }
    }
    apply_external_link_inputs(&mut command, external_link_inputs);
    for object_file in object_files {
        command.arg(object_file);
    }
    for source in swift_sources {
        command.arg(source);
    }
    run_command(&mut command)
}

fn link_native_target(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    target_kind: TargetKind,
    external_link_inputs: &ExternalLinkInputs,
    object_files: &[PathBuf],
    product_path: &Path,
) -> Result<()> {
    match target_kind {
        TargetKind::StaticLibrary => {
            let mut command = toolchain.libtool();
            command.arg("-static");
            command.arg("-o").arg(product_path);
            for object_file in object_files {
                command.arg(object_file);
            }
            run_command(&mut command)
        }
        TargetKind::DynamicLibrary | TargetKind::Framework => {
            let mut command = toolchain.clang(false);
            command.arg("-target").arg(&toolchain.target_triple);
            command.arg("-isysroot").arg(&toolchain.sdk_path);
            command.arg("-dynamiclib");
            command.arg("-o").arg(product_path);
            if matches!(profile.configuration.as_str(), "debug") {
                command.arg("-g");
            } else {
                command.arg("-O2");
            }
            apply_external_link_inputs(&mut command, external_link_inputs);
            for object_file in object_files {
                command.arg(object_file);
            }
            run_command(&mut command)
        }
        _ => {
            let mut command = toolchain.clang(false);
            command.arg("-target").arg(&toolchain.target_triple);
            command.arg("-isysroot").arg(&toolchain.sdk_path);
            command.arg("-o").arg(product_path);
            if matches!(profile.configuration.as_str(), "debug") {
                command.arg("-g");
            } else {
                command.arg("-O2");
            }
            apply_external_link_inputs(&mut command, external_link_inputs);
            for object_file in object_files {
                command.arg(object_file);
            }
            run_command(&mut command)
        }
    }
}

fn bundle_directory_name(target: &TargetManifest) -> String {
    match target.kind {
        TargetKind::App | TargetKind::WatchApp => format!("{}.app", target.name),
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
            format!("{}.appex", target.name)
        }
        TargetKind::Framework => format!("{}.framework", target.name),
        TargetKind::StaticLibrary => format!("lib{}.a", target.name),
        TargetKind::DynamicLibrary => format!("lib{}.dylib", target.name),
        TargetKind::Executable => target.name.clone(),
    }
}

fn product_layout(
    target_dir: &Path,
    intermediates_dir: &Path,
    target: &TargetManifest,
    toolchain: &Toolchain,
) -> ProductLayout {
    let product_path = target_dir.join(bundle_directory_name(target));
    let module_output_path = match target.kind {
        TargetKind::Framework => Some(
            product_path
                .join("Modules")
                .join(format!("{}.swiftmodule", target.name))
                .join(format!("{}.swiftmodule", toolchain.target_triple)),
        ),
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary => {
            Some(intermediates_dir.join(format!("{}.swiftmodule", target.name)))
        }
        _ => None,
    };
    let binary_path = match target.kind {
        TargetKind::App
        | TargetKind::AppExtension
        | TargetKind::WatchApp
        | TargetKind::WatchExtension
        | TargetKind::WidgetExtension
        | TargetKind::Framework => product_path.join(&target.name),
        _ => product_path.clone(),
    };
    ProductLayout {
        product_path,
        binary_path,
        module_output_path,
    }
}

fn needs_info_plist(target_kind: TargetKind) -> bool {
    target_kind.is_bundle()
}

fn write_info_plist(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    bundle_root: &Path,
    _profile_name: &str,
) -> Result<()> {
    let mut plist = Dictionary::new();
    plist.insert(
        "CFBundleIdentifier".to_owned(),
        Value::String(target.bundle_id.clone()),
    );
    plist.insert(
        "CFBundleExecutable".to_owned(),
        Value::String(target.name.clone()),
    );
    plist.insert(
        "CFBundleName".to_owned(),
        Value::String(target.name.clone()),
    );
    plist.insert(
        "CFBundleDisplayName".to_owned(),
        Value::String(
            target
                .display_name
                .clone()
                .unwrap_or_else(|| project.manifest.name.clone()),
        ),
    );
    plist.insert(
        "CFBundleShortVersionString".to_owned(),
        Value::String(project.manifest.version.clone()),
    );
    plist.insert(
        "CFBundleVersion".to_owned(),
        Value::String(
            target
                .build_number
                .clone()
                .unwrap_or_else(|| project.manifest.version.clone()),
        ),
    );
    plist.insert(
        "CFBundleInfoDictionaryVersion".to_owned(),
        Value::String("6.0".to_owned()),
    );
    plist.insert(
        "CFBundleDevelopmentRegion".to_owned(),
        Value::String("en".to_owned()),
    );
    plist.insert(
        "CFBundleSupportedPlatforms".to_owned(),
        Value::Array(vec![Value::String(
            toolchain.info_plist_supported_platform().to_owned(),
        )]),
    );

    match target.kind {
        TargetKind::App => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("APPL".to_owned()),
            );
            if matches!(toolchain.platform, ApplePlatform::Ios) {
                plist.insert("LSRequiresIPhoneOS".to_owned(), Value::Boolean(true));
                add_ios_app_plist_defaults(
                    &mut plist,
                    target,
                    toolchain.info_plist_supported_platform() == "iPhoneOS",
                )?;
            }
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
        }
        TargetKind::WatchApp => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("APPL".to_owned()),
            );
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
            plist.insert("WKWatchKitApp".to_owned(), Value::Boolean(true));
            if let Some(companion_bundle_id) =
                parent_bundle_id(project, &target.name, TargetKind::App)
            {
                plist.insert(
                    "WKCompanionAppBundleIdentifier".to_owned(),
                    Value::String(companion_bundle_id),
                );
            }
        }
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("XPC!".to_owned()),
            );
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
            let mut extension = extension_plist(
                target
                    .extension
                    .as_ref()
                    .context("extension configuration missing")?,
            )?;
            if matches!(target.kind, TargetKind::WatchExtension) {
                let watch_bundle_id = parent_bundle_id(project, &target.name, TargetKind::WatchApp)
                    .context("watch extension must be hosted by a watch app target")?;
                merge_extension_attributes(
                    &mut extension,
                    Dictionary::from_iter([(
                        "WKAppBundleIdentifier".to_owned(),
                        Value::String(watch_bundle_id),
                    )]),
                );
            }
            plist.insert("NSExtension".to_owned(), Value::Dictionary(extension));
        }
        TargetKind::Framework => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("FMWK".to_owned()),
            );
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
        }
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Executable => {
            bail!("non-bundle targets do not write Info.plist files")
        }
    }

    apply_info_plist_overrides(&mut plist, &target.info_plist)?;

    let path = bundle_root.join("Info.plist");
    ensure_parent_dir(&path)?;
    Value::Dictionary(plist)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    write_bundle_pkg_info(target.kind, bundle_root)
}

fn add_ios_app_plist_defaults(
    plist: &mut Dictionary,
    target: &TargetManifest,
    is_device_build: bool,
) -> Result<()> {
    let families = resolved_ios_device_families(target.ios.as_ref());
    plist.insert(
        "UIDeviceFamily".to_owned(),
        Value::Array(
            families
                .iter()
                .map(|family| Value::Integer(ios_device_family_code(*family).into()))
                .collect(),
        ),
    );
    let required_capabilities = target
        .ios
        .as_ref()
        .and_then(|ios| ios.required_device_capabilities.as_ref());
    if is_device_build || required_capabilities.is_some() {
        plist.insert(
            "UIRequiredDeviceCapabilities".to_owned(),
            Value::Array(
                required_capabilities
                    .map(|capabilities| {
                        capabilities
                            .iter()
                            .cloned()
                            .map(Value::String)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|| vec![Value::String("arm64".to_owned())]),
            ),
        );
    }
    plist.insert(
        "UIApplicationSupportsIndirectInputEvents".to_owned(),
        Value::Boolean(true),
    );
    plist.insert(
        "UILaunchScreen".to_owned(),
        Value::Dictionary(Dictionary::from_iter([(
            "UILaunchScreen".to_owned(),
            Value::Dictionary(launch_screen_dictionary(target.ios.as_ref())?),
        )])),
    );
    plist.insert(
        "UIStatusBarStyle".to_owned(),
        Value::String("UIStatusBarStyleDefault".to_owned()),
    );
    if families.contains(&IosDeviceFamily::Iphone) {
        plist.insert(
            "UISupportedInterfaceOrientations~iphone".to_owned(),
            Value::Array(resolved_ios_orientations(
                target.ios.as_ref().and_then(|ios| {
                    ios.supported_orientations
                        .as_ref()
                        .and_then(|orientations| orientations.iphone.as_ref())
                }),
                &[
                    IosInterfaceOrientation::Portrait,
                    IosInterfaceOrientation::LandscapeLeft,
                    IosInterfaceOrientation::LandscapeRight,
                ],
            )),
        );
    }
    if families.contains(&IosDeviceFamily::Ipad) {
        plist.insert(
            "UISupportedInterfaceOrientations~ipad".to_owned(),
            Value::Array(resolved_ios_orientations(
                target.ios.as_ref().and_then(|ios| {
                    ios.supported_orientations
                        .as_ref()
                        .and_then(|orientations| orientations.ipad.as_ref())
                }),
                &[
                    IosInterfaceOrientation::Portrait,
                    IosInterfaceOrientation::PortraitUpsideDown,
                    IosInterfaceOrientation::LandscapeLeft,
                    IosInterfaceOrientation::LandscapeRight,
                ],
            )),
        );
    }
    Ok(())
}

fn apply_info_plist_overrides(
    plist: &mut Dictionary,
    overrides: &BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    for (key, value) in overrides {
        plist.insert(key.clone(), json_to_plist(value)?);
    }
    Ok(())
}

fn resolved_ios_device_families(config: Option<&IosTargetManifest>) -> Vec<IosDeviceFamily> {
    config
        .and_then(|ios| ios.device_families.clone())
        .unwrap_or_else(|| vec![IosDeviceFamily::Iphone, IosDeviceFamily::Ipad])
}

fn ios_device_family_code(family: IosDeviceFamily) -> i64 {
    match family {
        IosDeviceFamily::Iphone => 1,
        IosDeviceFamily::Ipad => 2,
    }
}

fn launch_screen_dictionary(config: Option<&IosTargetManifest>) -> Result<Dictionary> {
    let mut dictionary = Dictionary::new();
    let Some(launch_screen) = config.and_then(|ios| ios.launch_screen.as_ref()) else {
        return Ok(dictionary);
    };
    for (key, value) in launch_screen {
        dictionary.insert(key.clone(), json_to_plist(value)?);
    }
    Ok(dictionary)
}

fn resolved_ios_orientations(
    configured: Option<&Vec<IosInterfaceOrientation>>,
    defaults: &[IosInterfaceOrientation],
) -> Vec<Value> {
    configured
        .map(|orientations| orientations.as_slice())
        .unwrap_or(defaults)
        .iter()
        .map(|orientation| Value::String(ios_orientation_name(*orientation).to_owned()))
        .collect()
}

fn ios_orientation_name(orientation: IosInterfaceOrientation) -> &'static str {
    match orientation {
        IosInterfaceOrientation::Portrait => "UIInterfaceOrientationPortrait",
        IosInterfaceOrientation::PortraitUpsideDown => "UIInterfaceOrientationPortraitUpsideDown",
        IosInterfaceOrientation::LandscapeLeft => "UIInterfaceOrientationLandscapeLeft",
        IosInterfaceOrientation::LandscapeRight => "UIInterfaceOrientationLandscapeRight",
    }
}

fn write_bundle_pkg_info(target_kind: TargetKind, bundle_root: &Path) -> Result<()> {
    let contents = match target_kind {
        TargetKind::App | TargetKind::WatchApp => Some("APPL????"),
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
            Some("XPC!????")
        }
        TargetKind::Framework => Some("FMWK????"),
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Executable => None,
    };

    let Some(contents) = contents else {
        return Ok(());
    };

    let path = bundle_root.join("PkgInfo");
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))
}

fn extension_plist(config: &ExtensionManifest) -> Result<Dictionary> {
    let mut extension = Dictionary::new();
    for (key, value) in &config.extra {
        extension.insert(key.clone(), json_to_plist(value)?);
    }
    extension.insert(
        "NSExtensionPointIdentifier".to_owned(),
        Value::String(config.point_identifier.clone()),
    );
    extension.insert(
        "NSExtensionPrincipalClass".to_owned(),
        Value::String(config.principal_class.clone()),
    );
    Ok(extension)
}

fn json_to_plist(value: &serde_json::Value) -> Result<Value> {
    Ok(match value {
        serde_json::Value::Null => bail!("null values are not supported in extension plist extras"),
        serde_json::Value::Bool(value) => Value::Boolean(*value),
        serde_json::Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Value::Integer(integer.into())
            } else if let Some(float) = value.as_f64() {
                Value::Real(float)
            } else {
                bail!("JSON number `{value}` is not representable in a plist");
            }
        }
        serde_json::Value::String(value) => Value::String(value.clone()),
        serde_json::Value::Array(values) => Value::Array(
            values
                .iter()
                .map(json_to_plist)
                .collect::<Result<Vec<_>>>()?,
        ),
        serde_json::Value::Object(values) => Value::Dictionary(Dictionary::from_iter(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_plist(value)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
    })
}

fn merge_extension_attributes(extension: &mut Dictionary, attributes: Dictionary) {
    if !extension.contains_key("NSExtensionAttributes") {
        extension.insert(
            "NSExtensionAttributes".to_owned(),
            Value::Dictionary(Dictionary::new()),
        );
    }
    let existing_attributes = extension
        .get_mut("NSExtensionAttributes")
        .and_then(Value::as_dictionary_mut)
        .expect("NSExtensionAttributes must remain a dictionary");
    for (key, value) in attributes {
        existing_attributes.insert(key, value);
    }
}

fn parent_bundle_id(
    project: &ProjectContext,
    target_name: &str,
    parent_kind: TargetKind,
) -> Option<String> {
    project
        .manifest
        .targets
        .iter()
        .find(|candidate| {
            candidate.kind == parent_kind
                && candidate
                    .dependencies
                    .iter()
                    .any(|name| name == target_name)
        })
        .map(|target| target.bundle_id.clone())
}

fn process_resources(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    bundle_root: &Path,
) -> Result<ResourceWorkSummary> {
    let mut asset_catalogs = Vec::new();
    let mut interface_jobs = Vec::new();
    let mut strings_jobs = Vec::new();
    let mut core_data_jobs = Vec::new();
    let mut copy_jobs = Vec::new();

    for resource in &target.resources {
        let resource_path = resolve_path(&project.root, resource);
        if !resource_path.exists() {
            bail!(
                "resource path `{}` for target `{}` does not exist",
                resource_path.display(),
                target.name
            );
        }
        discover_resources(
            &resource_path,
            &resource_path,
            &mut asset_catalogs,
            &mut interface_jobs,
            &mut strings_jobs,
            &mut core_data_jobs,
            &mut copy_jobs,
        )?;
    }

    let summary = ResourceWorkSummary {
        asset_catalogs: asset_catalogs.len(),
        interface_resources: interface_jobs.len(),
        strings_files: strings_jobs.len(),
        core_data_models: core_data_jobs.len(),
        copied_resources: copy_jobs.len(),
    };

    if !asset_catalogs.is_empty() {
        compile_asset_catalogs(toolchain, &asset_catalogs, bundle_root)?;
    }
    for (source, relative) in interface_jobs {
        compile_interface_resource(toolchain, &source, &bundle_root.join(relative))?;
    }
    for (source, relative) in strings_jobs {
        compile_strings_resource(&source, &bundle_root.join(relative))?;
    }
    for (source, relative) in core_data_jobs {
        compile_core_data_model(&source, &bundle_root.join(relative))?;
    }

    for (source, relative) in copy_jobs {
        let destination = bundle_root.join(relative);
        if source.is_dir() {
            copy_dir_recursive(&source, &destination)?;
        } else {
            copy_file(&source, &destination)?;
        }
    }

    Ok(summary)
}

fn discover_resources(
    current: &Path,
    root: &Path,
    asset_catalogs: &mut Vec<PathBuf>,
    interface_jobs: &mut Vec<(PathBuf, PathBuf)>,
    strings_jobs: &mut Vec<(PathBuf, PathBuf)>,
    core_data_jobs: &mut Vec<(PathBuf, PathBuf)>,
    copy_jobs: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<()> {
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xcassets"))
    {
        asset_catalogs.push(current.to_path_buf());
        return Ok(());
    }
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("storyboard"))
    {
        interface_jobs.push((
            current.to_path_buf(),
            compiled_interface_relative(current, root, "storyboardc")?,
        ));
        return Ok(());
    }
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xib"))
    {
        interface_jobs.push((
            current.to_path_buf(),
            compiled_interface_relative(current, root, "nib")?,
        ));
        return Ok(());
    }
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("xcdatamodel")
                || extension.eq_ignore_ascii_case("xcdatamodeld")
        })
    {
        core_data_jobs.push((
            current.to_path_buf(),
            compiled_interface_relative(
                current,
                root,
                if current
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("xcdatamodeld"))
                {
                    "momd"
                } else {
                    "mom"
                },
            )?,
        ));
        return Ok(());
    }

    if current.is_file() {
        let relative = current
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| current.file_name().map(PathBuf::from).unwrap_or_default());
        if current
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("strings"))
        {
            strings_jobs.push((current.to_path_buf(), relative));
            return Ok(());
        }
        copy_jobs.push((current.to_path_buf(), relative));
        return Ok(());
    }

    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xcassets"))
        {
            asset_catalogs.push(path);
            continue;
        }
        if path.is_dir() {
            discover_resources(
                &path,
                root,
                asset_catalogs,
                interface_jobs,
                strings_jobs,
                core_data_jobs,
                copy_jobs,
            )?;
        } else {
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("failed to derive resource path for {}", path.display()))?
                .to_path_buf();
            if path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("strings"))
            {
                strings_jobs.push((path, relative));
                continue;
            }
            if path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("storyboard"))
            {
                interface_jobs.push((
                    path.clone(),
                    compiled_interface_relative(&path, root, "storyboardc")?,
                ));
                continue;
            }
            if path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("xib"))
            {
                interface_jobs.push((
                    path.clone(),
                    compiled_interface_relative(&path, root, "nib")?,
                ));
                continue;
            }
            copy_jobs.push((path, relative));
        }
    }
    Ok(())
}

fn compile_asset_catalogs(
    toolchain: &Toolchain,
    asset_catalogs: &[PathBuf],
    bundle_root: &Path,
) -> Result<()> {
    let partial_plist = NamedTempFile::new()?;
    let mut command = toolchain.actool_command();
    command.arg("actool");
    command.arg("--compile").arg(bundle_root);
    command
        .arg("--output-partial-info-plist")
        .arg(partial_plist.path());
    command
        .arg("--platform")
        .arg(toolchain.actool_platform_name());
    command
        .arg("--minimum-deployment-target")
        .arg(&toolchain.deployment_target);
    for device in toolchain.actool_target_device() {
        command.arg("--target-device").arg(device);
    }
    if asset_catalog_contains_named_set(asset_catalogs, "AccentColor.colorset") {
        command.arg("--accent-color").arg("AccentColor");
    }
    if asset_catalog_contains_named_set(asset_catalogs, "AppIcon.appiconset") {
        command.arg("--app-icon").arg("AppIcon");
    }
    for catalog in asset_catalogs {
        command.arg(catalog);
    }
    command_output(&mut command)?;
    merge_partial_info_plist(bundle_root, partial_plist.path())
}

fn asset_catalog_contains_named_set(asset_catalogs: &[PathBuf], expected_name: &str) -> bool {
    asset_catalogs
        .iter()
        .any(|catalog| catalog.join(expected_name).exists())
}

fn merge_partial_info_plist(bundle_root: &Path, partial_plist_path: &Path) -> Result<()> {
    if !partial_plist_path.exists() {
        return Ok(());
    }

    let info_plist_path = bundle_root.join("Info.plist");
    if !info_plist_path.exists() {
        return Ok(());
    }

    let mut info_plist = Value::from_file(&info_plist_path)
        .with_context(|| format!("failed to read {}", info_plist_path.display()))?;
    let partial = Value::from_file(partial_plist_path)
        .with_context(|| format!("failed to read {}", partial_plist_path.display()))?;
    let info_dict = info_plist
        .as_dictionary_mut()
        .context("Info.plist must be a dictionary")?;
    let partial_dict = partial
        .as_dictionary()
        .context("actool partial Info.plist must be a dictionary")?;
    for (key, value) in partial_dict {
        info_dict.insert(key.clone(), value.clone());
    }
    info_plist
        .to_file_xml(&info_plist_path)
        .with_context(|| format!("failed to write {}", info_plist_path.display()))
}

fn compile_interface_resource(
    toolchain: &Toolchain,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    ensure_parent_dir(destination)?;
    let mut command = Command::new("xcrun");
    command.args(["--sdk", toolchain.sdk_name.as_str(), "ibtool"]);
    command.arg("--compile").arg(destination);
    command
        .arg("--platform")
        .arg(toolchain.actool_platform_name());
    command
        .arg("--minimum-deployment-target")
        .arg(&toolchain.deployment_target);
    for device in toolchain.actool_target_device() {
        command.arg("--target-device").arg(device);
    }
    command.arg(source);
    command_output(&mut command).map(|_| ())
}

fn compile_strings_resource(source: &Path, destination: &Path) -> Result<()> {
    ensure_parent_dir(destination)?;
    let mut command = Command::new("plutil");
    command.args(["-convert", "binary1", "-o"]);
    command.arg(destination);
    command.arg(source);
    run_command(&mut command)
}

fn compile_core_data_model(source: &Path, destination: &Path) -> Result<()> {
    ensure_parent_dir(destination)?;
    let mut command = Command::new("xcrun");
    command.arg("momc");
    command.arg(source);
    command.arg(destination);
    run_command(&mut command)
}

fn compiled_interface_relative(
    source: &Path,
    root: &Path,
    output_extension: &str,
) -> Result<PathBuf> {
    let relative = source
        .strip_prefix(root)
        .with_context(|| format!("failed to derive resource path for {}", source.display()))?;
    let mut destination = relative.to_path_buf();
    destination.set_extension(output_extension);
    Ok(destination)
}

fn embed_dependencies(
    project: &ProjectContext,
    root_target: &TargetManifest,
    built_targets: &HashMap<String, BuiltTarget>,
    built_root_target: &mut BuiltTarget,
) -> Result<()> {
    for dependency_name in &root_target.dependencies {
        let dependency_target = project.manifest.resolve_target(Some(dependency_name))?;
        let built = built_targets
            .get(dependency_name)
            .with_context(|| format!("missing built dependency `{dependency_name}`"))?;
        let Some(destination_root) =
            embedded_dependency_root(project, root_target, dependency_target)?
        else {
            continue;
        };
        let destination = built_root_target.bundle_path.join(destination_root).join(
            built
                .bundle_path
                .file_name()
                .context("dependency bundle name missing")?,
        );
        if built.bundle_path.is_dir() {
            copy_dir_recursive(&built.bundle_path, &destination)?;
        } else {
            copy_file(&built.bundle_path, &destination)?;
        }
    }
    Ok(())
}

fn embedded_dependency_root(
    project: &ProjectContext,
    parent_target: &TargetManifest,
    child_target: &TargetManifest,
) -> Result<Option<&'static str>> {
    Ok(match (parent_target.kind, child_target.kind) {
        (
            TargetKind::App | TargetKind::WatchApp,
            TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension,
        ) => Some("PlugIns"),
        (TargetKind::App, TargetKind::WatchApp) => Some("Watch"),
        (TargetKind::App, TargetKind::App)
            if crate::apple::signing::target_is_app_clip(project, child_target)? =>
        {
            Some("AppClips")
        }
        (
            TargetKind::App
            | TargetKind::AppExtension
            | TargetKind::WatchApp
            | TargetKind::WatchExtension
            | TargetKind::WidgetExtension,
            TargetKind::Framework,
        ) => Some("Frameworks"),
        _ => None,
    })
}

fn export_artifact(
    project: &ProjectContext,
    platform: ApplePlatform,
    built_target: &BuiltTarget,
    _build_root: &Path,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
) -> Result<PathBuf> {
    if !matches!(
        built_target.target_kind,
        TargetKind::App | TargetKind::WatchApp
    ) {
        return export_non_app_artifact(project, built_target, explicit_output);
    }
    match profile.distribution {
        DistributionKind::Development => {
            if let Some(output) = explicit_output {
                let output = resolve_path(&project.root, output);
                if built_target.bundle_path != output {
                    remove_existing_path(&output)?;
                    copy_product(&built_target.bundle_path, &output)?;
                    return Ok(output);
                }
            }
            Ok(built_target.bundle_path.clone())
        }
        DistributionKind::AdHoc | DistributionKind::AppStore => {
            let artifact_name = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
                project.project_paths.artifacts_dir.join(format!(
                    "{}-{:?}.ipa",
                    built_target.target_name, profile.distribution
                ))
            });
            let artifact_path = resolve_path(&project.root, &artifact_name);
            if artifact_path.exists() {
                remove_existing_path(&artifact_path)?;
            }
            let payload_dir = tempdir()?;
            let payload_root = payload_dir.path().join("Payload");
            ensure_dir(&payload_root)?;
            let bundle_destination = payload_root.join(
                built_target
                    .bundle_path
                    .file_name()
                    .context("bundle file name missing")?,
            );
            copy_product(&built_target.bundle_path, &bundle_destination)?;
            let mut command = Command::new("ditto");
            command.args([
                "-c",
                "-k",
                "--keepParent",
                payload_root
                    .to_str()
                    .context("payload path contains invalid UTF-8")?,
                artifact_path
                    .to_str()
                    .context("artifact path contains invalid UTF-8")?,
            ]);
            run_command(&mut command)?;
            Ok(artifact_path)
        }
        DistributionKind::DeveloperId | DistributionKind::MacAppStore => {
            export_macos_artifact(project, platform, built_target, explicit_output, profile)
        }
    }
}

fn export_macos_artifact(
    project: &ProjectContext,
    platform: ApplePlatform,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
) -> Result<PathBuf> {
    if platform != ApplePlatform::Macos {
        bail!("macOS artifact export was requested for non-macOS platform `{platform}`");
    }
    let artifact_name = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
        project.project_paths.artifacts_dir.join(format!(
            "{}-{:?}.pkg",
            built_target.target_name, profile.distribution
        ))
    });
    let artifact_path = resolve_path(&project.root, &artifact_name);
    remove_existing_path(&artifact_path)?;

    let signing = crate::apple::signing::prepare_package_signing(project, profile)?;
    let mut command = Command::new("productbuild");
    command.arg("--component");
    command.arg(&built_target.bundle_path);
    command.arg("/Applications");
    command.arg("--sign").arg(&signing.signing_identity);
    command.arg("--keychain").arg(&signing.keychain_path);
    command.arg("--timestamp");
    command.arg(&artifact_path);
    run_command(&mut command)?;
    Ok(artifact_path)
}

fn export_non_app_artifact(
    project: &ProjectContext,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
) -> Result<PathBuf> {
    let output = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
        project.project_paths.artifacts_dir.join(
            built_target
                .bundle_path
                .file_name()
                .unwrap_or_else(|| OsStr::new(built_target.target_name.as_str())),
        )
    });
    let output = resolve_path(&project.root, &output);
    if output != built_target.bundle_path {
        remove_existing_path(&output)?;
        copy_product(&built_target.bundle_path, &output)?;
        return Ok(output);
    }
    Ok(built_target.bundle_path.clone())
}

fn remove_existing_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn copy_product(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        copy_dir_recursive(source, destination)
    } else {
        copy_file(source, destination)
    }
}

fn run_on_macos(receipt: &BuildReceipt) -> Result<()> {
    let executable = macos_executable_path(receipt)?;
    println!(
        "Launching {} on the local Mac. Orbit will stay attached until the process exits; press Ctrl-C to stop.",
        receipt.bundle_id
    );

    let mut command = Command::new(&executable);
    if let Some(bundle_root) = receipt.bundle_path.parent() {
        command.current_dir(bundle_root);
    }
    run_command(&mut command)
}

fn debug_on_macos(receipt: &BuildReceipt) -> Result<()> {
    let executable = macos_executable_path(receipt)?;
    println!(
        "Launching LLDB for {} on the local Mac. Orbit will stop at process entry so you can set breakpoints before continuing.",
        receipt.bundle_id
    );

    let mut command = Command::new("lldb");
    command.arg("--file").arg(&executable);
    command.arg("-o").arg("process launch --stop-at-entry");
    if let Some(bundle_root) = receipt.bundle_path.parent() {
        command.current_dir(bundle_root);
    }
    run_command(&mut command)
}

fn macos_executable_path(receipt: &BuildReceipt) -> Result<PathBuf> {
    let standard_bundle_binary = receipt
        .bundle_path
        .join("Contents")
        .join("MacOS")
        .join(&receipt.target);
    if standard_bundle_binary.exists() {
        return Ok(standard_bundle_binary);
    }

    let legacy_bundle_binary = receipt.bundle_path.join(&receipt.target);
    if legacy_bundle_binary.exists() {
        return Ok(legacy_bundle_binary);
    }

    if receipt.bundle_path.is_file() {
        return Ok(receipt.bundle_path.clone());
    }
    if receipt.artifact_path.is_file() {
        return Ok(receipt.artifact_path.clone());
    }

    bail!(
        "failed to find a runnable macOS executable inside {}",
        receipt.bundle_path.display()
    )
}

fn run_on_simulator(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    let device = prepare_simulator_installation(project, receipt)?;

    println!(
        "Launching {} on {}. Orbit will stay attached to the simulator console; press Ctrl-C to stop.",
        receipt.bundle_id, device.name
    );

    let mut launch = Command::new("xcrun");
    launch.args([
        "simctl",
        "launch",
        "--console-pty",
        &device.udid,
        &receipt.bundle_id,
    ]);
    run_command(&mut launch)?;

    Ok(())
}

fn debug_on_simulator(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    let device = prepare_simulator_installation(project, receipt)?;
    let executable = bundle_debug_executable_path(receipt)?;
    let process_name = simulator_process_name(receipt);

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB, attach, and continue the app.",
        receipt.bundle_id, device.name
    );

    let mut launch = Command::new("xcrun");
    launch.args([
        "simctl",
        "launch",
        "--wait-for-debugger",
        "--terminate-running-process",
        &device.udid,
        &receipt.bundle_id,
    ]);
    run_command(&mut launch)?;

    let mut command = Command::new("lldb");
    command.arg("--file").arg(&executable);
    command
        .arg("-o")
        .arg(format!("process attach -i -w -n {process_name}"));
    command.arg("-o").arg("process continue");
    if let Some(bundle_root) = receipt.bundle_path.parent() {
        command.current_dir(bundle_root);
    }
    run_command(&mut command)
}

fn run_on_device(device: &PhysicalDevice, receipt: &BuildReceipt) -> Result<()> {
    let installed = install_on_device(device, receipt)?;
    if receipt.platform == ApplePlatform::Ios {
        launch_ios_app_by_bundle_id(device, &receipt.bundle_id)?;
    } else {
        let remote_bundle_path = remote_app_bundle_path(&installed.installation_url)?;
        launch_device_app(device, &remote_bundle_path, false)?;
    }
    Ok(())
}

fn debug_on_device(
    project: &ProjectContext,
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
) -> Result<()> {
    if receipt.platform == ApplePlatform::Ios {
        return debug_on_ios_device(project, device, receipt);
    }

    let installed = install_on_device(device, receipt)?;

    let executable = bundle_debug_executable_path(receipt)?;
    ensure_device_is_unlocked_for_debugging(device, receipt.platform)?;
    let symbol_root = ensure_device_symbols_available(project, device, receipt.platform)?;
    ensure_device_is_unlocked_for_debugging(device, receipt.platform)?;

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB, attach, and continue the app. Use `quit` to end the session; Ctrl-C interrupts the target.",
        receipt.bundle_id, device.device_properties.name
    );

    let remote_bundle_path = remote_app_bundle_path(&installed.installation_url)?;
    let launch = launch_device_app(device, &remote_bundle_path, true)?;

    let mut command = Command::new("lldb");
    command.arg("--file").arg(&executable);
    if let Some(symbol_root) = &symbol_root {
        let symbol_root = symbol_root
            .to_str()
            .context("device symbol cache path contains invalid UTF-8")?;
        let symbol_root = lldb_quote_arg(symbol_root);
        command.arg("-o").arg(format!(
            "settings append target.exec-search-paths {symbol_root}"
        ));
        command.arg("-o").arg(format!(
            "settings append target.debug-file-search-paths {symbol_root}"
        ));
    }
    command
        .arg("-o")
        .arg(format!("device select {}", device.identifier));
    command.arg("-o").arg(format!(
        "device process attach -c -p {}",
        launch.process_identifier
    ));
    if let Some(bundle_root) = receipt.bundle_path.parent() {
        command.current_dir(bundle_root);
    }
    run_command(&mut command)
}

fn debug_on_ios_device(
    project: &ProjectContext,
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
) -> Result<()> {
    let installed = install_on_device(device, receipt)?;

    let executable = bundle_debug_executable_path(receipt)?;
    ensure_device_is_unlocked_for_debugging(device, receipt.platform)?;
    let symbol_root = ensure_device_symbols_available(project, device, receipt.platform)?;
    ensure_device_is_unlocked_for_debugging(device, receipt.platform)?;

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB and attach to the launched app. Use `quit` to end the session; Ctrl-C interrupts the target.",
        receipt.bundle_id, device.device_properties.name
    );

    let mut launch = spawn_ios_debug_launch_session(device, &receipt.bundle_id)?;
    let process = wait_for_device_process_for_installation(
        device,
        &installed.installation_url,
        Duration::from_secs(15),
        Some(&mut launch),
    )?;

    let result = run_lldb_device_attach_session(
        device,
        &executable,
        process.process_identifier,
        symbol_root.as_deref(),
    );

    let _ = launch.kill();
    let _ = launch.wait();

    result
}

fn launch_ios_app_by_bundle_id(
    device: &PhysicalDevice,
    bundle_id: &str,
) -> Result<DeviceLaunchedProcess> {
    let output_path = NamedTempFile::new()?;
    let mut launch = Command::new("xcrun");
    launch.args([
        "devicectl",
        "device",
        "process",
        "launch",
        "--device",
        &device.hardware_properties.udid,
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
        bundle_id,
    ]);
    run_command(&mut launch)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let launched: DeviceLaunchResponse = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device process launch` output")?;
    Ok(launched.result.process)
}

fn spawn_ios_debug_launch_session(device: &PhysicalDevice, bundle_id: &str) -> Result<Child> {
    let mut launch = Command::new("xcrun");
    launch.args([
        "devicectl",
        "device",
        "process",
        "launch",
        "--console",
        "--start-stopped",
        "--terminate-existing",
        "--device",
        &device.hardware_properties.udid,
        bundle_id,
    ]);
    launch.stdin(Stdio::inherit());
    launch.stdout(Stdio::inherit());
    launch.stderr(Stdio::inherit());
    launch.spawn().with_context(|| {
        format!(
            "failed to execute `{}`",
            crate::util::debug_command(&launch)
        )
    })
}

fn prepare_simulator_installation(
    project: &ProjectContext,
    receipt: &BuildReceipt,
) -> Result<SimulatorDevice> {
    let device = select_simulator_device(project, receipt.platform)?;
    if !device.is_booted() {
        let mut boot = Command::new("xcrun");
        boot.args(["simctl", "boot", &device.udid]);
        run_command(&mut boot)?;
    }

    let mut bootstatus = Command::new("xcrun");
    bootstatus.args(["simctl", "bootstatus", &device.udid, "-b"]);
    run_command(&mut bootstatus)?;

    let mut open_simulator = Command::new("open");
    open_simulator.args([
        "-a",
        "Simulator",
        "--args",
        "-CurrentDeviceUDID",
        &device.udid,
    ]);
    run_command(&mut open_simulator)?;

    let mut install = Command::new("xcrun");
    install.args([
        "simctl",
        "install",
        &device.udid,
        receipt
            .bundle_path
            .to_str()
            .context("bundle path contains invalid UTF-8")?,
    ]);
    run_command(&mut install)?;

    Ok(device)
}

fn install_on_device(
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
) -> Result<InstalledApplication> {
    let output_path = NamedTempFile::new()?;
    let mut install = Command::new("xcrun");
    install.args([
        "devicectl",
        "device",
        "install",
        "app",
        "--device",
        &device.hardware_properties.udid,
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
        receipt
            .bundle_path
            .to_str()
            .context("bundle path contains invalid UTF-8")?,
    ]);
    run_command(&mut install)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let installed: DeviceInstallResponse = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device install app` output")?;
    installed
        .result
        .installed_applications
        .into_iter()
        .next()
        .context("`devicectl device install app` did not report an installed application")
}

fn remote_app_bundle_path(installation_url: &str) -> Result<String> {
    let path = installation_url
        .strip_prefix("file://")
        .unwrap_or(installation_url)
        .trim_end_matches('/');
    if path.is_empty() {
        bail!(
            "installed application URL `{installation_url}` did not include a remote bundle path"
        );
    }
    Ok(path.to_owned())
}

fn launch_device_app(
    device: &PhysicalDevice,
    remote_bundle_path: &str,
    start_stopped: bool,
) -> Result<DeviceLaunchedProcess> {
    let output_path = NamedTempFile::new()?;
    let mut launch = Command::new("xcrun");
    launch.args(["devicectl", "device", "process", "launch"]);
    if start_stopped {
        launch.arg("--start-stopped");
    }
    launch.args([
        "--terminate-existing",
        "--device",
        &device.hardware_properties.udid,
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
        remote_bundle_path,
    ]);
    run_command(&mut launch)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let launched: DeviceLaunchResponse = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device process launch` output")?;
    Ok(launched.result.process)
}

fn list_device_processes(device: &PhysicalDevice) -> Result<Vec<DeviceRunningProcess>> {
    let output_path = NamedTempFile::new()?;
    let mut command = Command::new("xcrun");
    command.args([
        "devicectl",
        "device",
        "info",
        "processes",
        "--device",
        &device.hardware_properties.udid,
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
    ]);
    run_command(&mut command)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let processes: DeviceProcessesResponse = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device info processes` output")?;
    Ok(processes.result.running_processes)
}

fn wait_for_device_process_for_installation(
    device: &PhysicalDevice,
    installation_url: &str,
    timeout: Duration,
    mut launch_child: Option<&mut Child>,
) -> Result<DeviceRunningProcess> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(process) =
            find_running_process_for_installation(&list_device_processes(device)?, installation_url)
        {
            return Ok(process.clone());
        }

        if let Some(child) = launch_child.as_deref_mut() {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    if let Some(signal) = status.signal() {
                        bail!(
                            "`devicectl device process launch --console --start-stopped` exited from signal {signal} before Orbit could attach LLDB"
                        );
                    }
                    bail!(
                        "`devicectl device process launch --console --start-stopped` exited with {status} before Orbit could attach LLDB"
                    );
                }
            }
        }

        thread::sleep(Duration::from_millis(250));
    }

    bail!(
        "failed to identify the launched `{}` process on device {} ({})",
        bundle_name_from_installation_url(installation_url),
        device.device_properties.name,
        device.hardware_properties.udid
    )
}

fn find_running_process_for_installation<'a>(
    processes: &'a [DeviceRunningProcess],
    installation_url: &str,
) -> Option<&'a DeviceRunningProcess> {
    processes.iter().find(|process| {
        process
            .executable
            .as_deref()
            .is_some_and(|executable| executable.starts_with(installation_url))
    })
}

fn bundle_name_from_installation_url(installation_url: &str) -> String {
    installation_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(installation_url)
        .trim_end_matches(".app")
        .to_owned()
}

fn run_lldb_device_attach_session(
    device: &PhysicalDevice,
    executable: &Path,
    process_identifier: u64,
    symbol_root: Option<&Path>,
) -> Result<()> {
    let script = NamedTempFile::new()?;
    fs::write(
        script.path(),
        lldb_expect_attach_script(symbol_root)?.as_bytes(),
    )
    .with_context(|| format!("failed to write {}", script.path().display()))?;

    let mut command = Command::new("expect");
    command.arg("-f").arg(script.path());
    command.arg(&device.hardware_properties.udid);
    command.arg(process_identifier.to_string());
    command.arg(executable);
    run_command(&mut command)
}

fn lldb_expect_attach_script(symbol_root: Option<&Path>) -> Result<String> {
    let expect_symbol_root = symbol_root
        .map(|path| {
            path.to_str()
                .context("device symbol cache path contains invalid UTF-8")
                .map(tcl_quote_arg)
        })
        .transpose()?
        .unwrap_or_default();
    let symbol_setup = if expect_symbol_root.is_empty() {
        String::new()
    } else {
        format!(
            r#"send -- "settings append target.exec-search-paths \"{symbol_root}\"\r"
wait_for_prompt
send -- "settings append target.debug-file-search-paths \"{symbol_root}\"\r"
wait_for_prompt
"#,
            symbol_root = expect_symbol_root
        )
    };
    Ok(format!(
        r#"set timeout 60

proc wait_for_prompt {{}} {{
    expect {{
        -re {{\(lldb\)}} {{ return }}
        timeout {{ send_user "timed out waiting for LLDB prompt\n"; exit 1 }}
        eof {{ send_user "LLDB exited before it became interactive\n"; exit 1 }}
    }}
}}

proc wait_for_log {{pattern message}} {{
    expect {{
        -re $pattern {{ return }}
        timeout {{ send_user "$message\n"; exit 1 }}
        eof {{ send_user "LLDB exited unexpectedly\n"; exit 1 }}
    }}
}}

set udid [lindex $argv 0]
set pid [lindex $argv 1]
set exe [lindex $argv 2]

spawn lldb $exe
wait_for_prompt
{symbol_setup}send -- "device select $udid\r"
wait_for_prompt
send -- "device process attach --pid $pid\r"
wait_for_log [format {{Process %s stopped}} $pid] [format {{timed out waiting for LLDB to attach to pid %s}} $pid]
wait_for_prompt
send -- "process continue\r"
wait_for_log [format {{Process %s resuming}} $pid] [format {{timed out waiting for LLDB to resume pid %s}} $pid]
wait_for_prompt
interact
"#,
        symbol_setup = symbol_setup
    ))
}

fn tcl_quote_arg(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn bundle_debug_executable_path(receipt: &BuildReceipt) -> Result<PathBuf> {
    let path = receipt.bundle_path.join(&receipt.target);
    if path.exists() {
        return Ok(path);
    }
    macos_executable_path(receipt)
}

fn simulator_process_name(receipt: &BuildReceipt) -> &str {
    receipt.target.as_str()
}

fn select_simulator_device(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<SimulatorDevice> {
    let output = command_output(Command::new("xcrun").args([
        "simctl",
        "list",
        "devices",
        "available",
        "--json",
    ]))?;
    let devices: SimctlList = serde_json::from_str(&output)?;
    let mut flattened = devices
        .devices
        .into_iter()
        .filter(|(runtime, _)| simulator_runtime_matches_platform(runtime, platform))
        .flat_map(|(_, devices)| devices)
        .collect::<Vec<_>>();
    flattened.sort_by(|left, right| {
        right
            .is_booted()
            .cmp(&left.is_booted())
            .then_with(|| left.name.cmp(&right.name))
    });

    if flattened.is_empty() {
        bail!("no available {platform} simulators were found");
    }

    let display = flattened
        .iter()
        .map(|device| format!("{} ({})", device.name, device.state))
        .collect::<Vec<_>>();
    let index = if project.app.interactive {
        prompt_select("Select a simulator", &display)?
    } else {
        0
    };
    Ok(flattened.remove(index))
}

fn submit_with_altool(project: &ProjectContext, receipt: &BuildReceipt, wait: bool) -> Result<()> {
    ensure_submit_app_record(project, receipt)?;
    run_altool_command(project, receipt, true, false)?;

    let mut command = build_altool_command(project, receipt, false, wait)?;
    let result = run_command(&mut command);
    Ok(result?)
}

fn build_altool_command(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    validate_only: bool,
    wait: bool,
) -> Result<Command> {
    let auth = crate::apple::auth::resolve_submit_auth(project)?;
    let mut command = Command::new("xcrun");
    command.arg("altool");
    command.arg(if validate_only {
        "--validate-app"
    } else {
        "--upload-package"
    });
    command.arg(&receipt.artifact_path);
    if wait && !validate_only {
        command.arg("--wait");
    }

    match auth {
        crate::apple::auth::SubmitAuth::ApiKey {
            key_id,
            issuer_id,
            api_key_path,
        } => {
            let file_name = api_key_path
                .file_name()
                .context("API key path is missing a file name")?;
            let private_keys_dir = project.app.global_paths.cache_dir.join("private_keys");
            ensure_dir(&private_keys_dir)?;
            copy_file(&api_key_path, &private_keys_dir.join(file_name))?;
            command.arg("--api-key").arg(key_id);
            command.arg("--api-issuer").arg(issuer_id);
            command.env("API_PRIVATE_KEYS_DIR", &private_keys_dir);
        }
        crate::apple::auth::SubmitAuth::AppleId {
            apple_id,
            password,
            team_id: _,
            provider_id,
        } => {
            command.arg("--username").arg(apple_id);
            command.arg("--password").arg("@env:ORBIT_ALTOOL_PASSWORD");
            command.env("ORBIT_ALTOOL_PASSWORD", password);
            if let Some(provider_id) = provider_id {
                command.arg("--provider-public-id").arg(provider_id);
            }
        }
    }

    Ok(command)
}

fn run_altool_command(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    validate_only: bool,
    wait: bool,
) -> Result<()> {
    let mut command = build_altool_command(project, receipt, validate_only, wait)?;
    run_command(&mut command)
}

fn submit_with_notarytool(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    wait: bool,
) -> Result<()> {
    let auth = crate::apple::auth::resolve_submit_auth(project)?;
    let mut command = Command::new("xcrun");
    command.arg("notarytool");
    command.arg("submit");
    command.arg(&receipt.artifact_path);
    command.arg("--output-format").arg("json");
    if wait {
        command.arg("--wait");
    }

    match auth {
        crate::apple::auth::SubmitAuth::ApiKey {
            key_id,
            issuer_id,
            api_key_path,
        } => {
            command.arg("--key").arg(api_key_path);
            command.arg("--key-id").arg(key_id);
            command.arg("--issuer").arg(issuer_id);
        }
        crate::apple::auth::SubmitAuth::AppleId {
            apple_id,
            password,
            team_id,
            provider_id: _,
        } => {
            let team_id = team_id.context(
                "notarization with Apple ID requires a configured Apple Developer team ID",
            )?;
            command.arg("--apple-id").arg(apple_id);
            command.arg("--password").arg("@env:ORBIT_NOTARY_PASSWORD");
            command.arg("--team-id").arg(team_id);
            command.env("ORBIT_NOTARY_PASSWORD", password);
        }
    }

    let output = command_output(&mut command)?;
    if wait {
        let response: NotarySubmitResponse =
            serde_json::from_str(&output).context("failed to parse notarytool submit response")?;
        if !response.status.eq_ignore_ascii_case("accepted")
            && !response.status.eq_ignore_ascii_case("success")
        {
            bail!(
                "notarytool completed with status `{}` for submission {}",
                response.status,
                response.id
            );
        }
        let mut staple = Command::new("xcrun");
        staple.arg("stapler");
        staple.arg("staple");
        staple.arg(&receipt.artifact_path);
        run_command(&mut staple)?;
    }
    Ok(())
}

fn ensure_submit_app_record(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    if !matches!(
        receipt.distribution,
        DistributionKind::AppStore | DistributionKind::MacAppStore
    ) {
        return Ok(());
    }
    let Some(api_key_auth) = crate::apple::auth::resolve_api_key_auth(&project.app)? else {
        return Ok(());
    };
    let client = crate::apple::asc_api::AscClient::new(api_key_auth)?;
    let bundle_id = client
        .find_bundle_id(&receipt.bundle_id)?
        .with_context(|| {
            format!(
                "missing App Store Connect bundle ID for `{}`",
                receipt.bundle_id
            )
        })?;
    if client.find_app_by_bundle_id(&bundle_id.data.id)?.is_some() {
        return Ok(());
    }

    let app_name = receipt.target.clone();
    let sku = app_store_sku(&receipt.bundle_id);
    let _ = client.create_app_record(&app_name, &sku, "en-US", &bundle_id.data.id)?;
    Ok(())
}

fn app_store_sku(bundle_id: &str) -> String {
    let mut sku = bundle_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    sku.truncate(255);
    sku
}

#[derive(Debug, Clone, Deserialize)]
struct NotarySubmitResponse {
    id: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SwiftPackageManifest {
    name: String,
    products: Vec<SwiftPackageProduct>,
    targets: Vec<SwiftPackageTarget>,
}

#[derive(Debug, Clone, Deserialize)]
struct SwiftPackageProduct {
    name: String,
    targets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SwiftPackageTarget {
    name: String,
    path: Option<String>,
    #[serde(default)]
    dependencies: Vec<SwiftPackageTargetDependency>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum SwiftPackageTargetDependency {
    ByName {
        #[serde(rename = "byName")]
        by_name: (String, Option<serde_json::Value>),
    },
    Target {
        target: (String, Option<serde_json::Value>),
    },
    Product {
        product: (
            String,
            Option<String>,
            Option<Vec<serde_json::Value>>,
            Option<serde_json::Value>,
        ),
    },
}

#[derive(Debug, Clone)]
struct PackageBuildOutput {
    module_dir: PathBuf,
    library_dir: PathBuf,
    link_libraries: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct XcframeworkInfoPlist {
    #[serde(rename = "AvailableLibraries")]
    available_libraries: Vec<XcframeworkLibrary>,
}

#[derive(Debug, Clone, Deserialize)]
struct XcframeworkLibrary {
    #[serde(rename = "LibraryIdentifier")]
    library_identifier: String,
    #[serde(rename = "LibraryPath")]
    library_path: String,
    #[serde(rename = "HeadersPath")]
    headers_path: Option<String>,
    #[serde(rename = "SupportedPlatform")]
    supported_platform: String,
    #[serde(rename = "SupportedPlatformVariant")]
    supported_platform_variant: Option<String>,
    #[serde(rename = "SupportedArchitectures")]
    supported_architectures: Vec<String>,
}

fn compile_swift_packages(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    target: &TargetManifest,
) -> Result<Vec<PackageBuildOutput>> {
    let mut outputs = Vec::new();

    for dependency in &target.swift_packages {
        outputs.push(build_progress_step(
            format!(
                "Compiling Swift package product `{}` for target `{}`",
                dependency.product, target.name
            ),
            |_| {
                format!(
                    "Compiled Swift package product `{}` for target `{}`.",
                    dependency.product, target.name
                )
            },
            || compile_swift_package(project, toolchain, profile, intermediates_dir, dependency),
        )?);
    }

    Ok(outputs)
}

fn resolve_external_link_inputs(
    project: &ProjectContext,
    toolchain: &Toolchain,
    intermediates_dir: &Path,
    target: &TargetManifest,
) -> Result<ExternalLinkInputs> {
    let mut inputs = ExternalLinkInputs {
        link_frameworks: target.frameworks.clone(),
        weak_frameworks: target.weak_frameworks.clone(),
        link_libraries: target.system_libraries.clone(),
        ..ExternalLinkInputs::default()
    };

    for dependency in &target.xcframeworks {
        merge_external_link_inputs(
            &mut inputs,
            resolve_xcframework_dependency(project, toolchain, intermediates_dir, dependency)?,
        );
    }

    dedup_vec(&mut inputs.module_search_paths);
    dedup_vec(&mut inputs.framework_search_paths);
    dedup_vec(&mut inputs.library_search_paths);
    dedup_vec(&mut inputs.link_frameworks);
    dedup_vec(&mut inputs.weak_frameworks);
    dedup_vec(&mut inputs.link_libraries);
    dedup_vec(&mut inputs.embedded_payloads);

    Ok(inputs)
}

fn resolve_xcframework_dependency(
    project: &ProjectContext,
    toolchain: &Toolchain,
    _intermediates_dir: &Path,
    dependency: &XcframeworkDependency,
) -> Result<ExternalLinkInputs> {
    let xcframework_root = resolve_path(&project.root, &dependency.path);
    let info_path = xcframework_root.join("Info.plist");
    let info: XcframeworkInfoPlist = plist::from_file(&info_path)
        .with_context(|| format!("failed to parse {}", info_path.display()))?;
    let library =
        select_xcframework_library(toolchain, &info.available_libraries).with_context(|| {
            format!(
                "failed to select XCFramework slice for {}",
                xcframework_root.display()
            )
        })?;
    let slice_root = xcframework_root.join(&library.library_identifier);
    let library_path = slice_root.join(&library.library_path);
    let mut inputs = ExternalLinkInputs::default();

    if let Some(headers_path) = &library.headers_path {
        let headers_root = slice_root.join(headers_path);
        inputs.module_search_paths.push(headers_root);
    }

    let explicit_name = dependency.library.as_ref().map(|name| name.as_str());
    let file_name = library_path
        .file_name()
        .and_then(OsStr::to_str)
        .context("XCFramework library path is missing a file name")?;
    if file_name.ends_with(".framework") {
        let framework_name = explicit_name
            .map(ToOwned::to_owned)
            .or_else(|| {
                Path::new(file_name)
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .map(ToOwned::to_owned)
            })
            .context("failed to derive XCFramework framework name")?;
        inputs.framework_search_paths.push(
            library_path
                .parent()
                .context("framework path is missing a parent")?
                .to_path_buf(),
        );
        inputs.link_frameworks.push(framework_name);
        if dependency.embed {
            inputs.embedded_payloads.push(library_path);
        }
    } else {
        let library_name = explicit_name
            .map(ToOwned::to_owned)
            .or_else(|| {
                file_name
                    .strip_prefix("lib")
                    .and_then(|value| {
                        value
                            .strip_suffix(".a")
                            .or_else(|| value.strip_suffix(".dylib"))
                    })
                    .map(ToOwned::to_owned)
            })
            .context("failed to derive XCFramework library name")?;
        inputs.library_search_paths.push(
            library_path
                .parent()
                .context("library path is missing a parent")?
                .to_path_buf(),
        );
        inputs.link_libraries.push(library_name);
        if dependency.embed && file_name.ends_with(".dylib") {
            inputs.embedded_payloads.push(library_path);
        }
    }

    Ok(inputs)
}

fn select_xcframework_library<'a>(
    toolchain: &Toolchain,
    available_libraries: &'a [XcframeworkLibrary],
) -> Option<&'a XcframeworkLibrary> {
    let platform = match toolchain.platform {
        ApplePlatform::Ios => "ios",
        ApplePlatform::Macos => "macos",
        ApplePlatform::Tvos => "tvos",
        ApplePlatform::Visionos => "xros",
        ApplePlatform::Watchos => "watchos",
    };
    let variant = match toolchain.destination {
        DestinationKind::Simulator => Some("simulator"),
        DestinationKind::Device => None,
    };

    available_libraries.iter().find(|library| {
        library.supported_platform == platform
            && library.supported_platform_variant.as_deref() == variant
            && library
                .supported_architectures
                .iter()
                .any(|architecture| architecture == &toolchain.architecture)
    })
}

fn merge_external_link_inputs(target: &mut ExternalLinkInputs, source: ExternalLinkInputs) {
    target
        .module_search_paths
        .extend(source.module_search_paths);
    target
        .framework_search_paths
        .extend(source.framework_search_paths);
    target
        .library_search_paths
        .extend(source.library_search_paths);
    target.link_frameworks.extend(source.link_frameworks);
    target.weak_frameworks.extend(source.weak_frameworks);
    target.link_libraries.extend(source.link_libraries);
    target.embedded_payloads.extend(source.embedded_payloads);
}

fn apply_external_link_inputs(command: &mut Command, inputs: &ExternalLinkInputs) {
    for path in &inputs.module_search_paths {
        command.arg("-I").arg(path);
    }
    for path in &inputs.framework_search_paths {
        command.arg("-F").arg(path);
    }
    for path in &inputs.library_search_paths {
        command.arg("-L").arg(path);
    }
    for framework in &inputs.link_frameworks {
        command.arg("-framework").arg(framework);
    }
    for framework in &inputs.weak_frameworks {
        command.arg("-weak_framework").arg(framework);
    }
    for library in &inputs.link_libraries {
        command.arg("-l").arg(library);
    }
}

fn embed_external_payloads(inputs: &ExternalLinkInputs, bundle_root: &Path) -> Result<()> {
    if inputs.embedded_payloads.is_empty() {
        return Ok(());
    }

    let frameworks_root = bundle_root.join("Frameworks");
    ensure_dir(&frameworks_root)?;
    for payload in &inputs.embedded_payloads {
        let file_name = payload
            .file_name()
            .context("embedded payload path is missing a file name")?;
        let destination = frameworks_root.join(file_name);
        if payload.is_dir() {
            copy_dir_recursive(payload, &destination)?;
        } else {
            copy_file(payload, &destination)?;
        }
    }
    Ok(())
}

fn dedup_vec<T>(values: &mut Vec<T>)
where
    T: Ord,
{
    values.sort();
    values.dedup();
}

fn compile_swift_package(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    dependency: &SwiftPackageDependency,
) -> Result<PackageBuildOutput> {
    let package_root = resolve_path(&project.root, &dependency.path);
    let description = command_output(
        Command::new("swift")
            .args(["package", "--package-path"])
            .arg(&package_root)
            .arg("dump-package"),
    )?;
    let package: SwiftPackageManifest = serde_json::from_str(&description).with_context(|| {
        format!(
            "failed to parse Swift package description for {}",
            package_root.display()
        )
    })?;

    let product = package
        .products
        .iter()
        .find(|product| product.name == dependency.product)
        .with_context(|| {
            format!(
                "Swift package `{}` does not export product `{}`",
                package_root.display(),
                dependency.product
            )
        })?;

    let built_target_names = ordered_package_targets(&package, &product.targets)?;
    let module_dir = intermediates_dir
        .join("swiftpackages")
        .join(&dependency.product)
        .join("modules");
    let library_dir = intermediates_dir
        .join("swiftpackages")
        .join(&dependency.product)
        .join("libs");
    ensure_dir(&module_dir)?;
    ensure_dir(&library_dir)?;

    let targets_by_name = package
        .targets
        .iter()
        .map(|target| (target.name.as_str(), target))
        .collect::<HashMap<_, _>>();
    let mut built_libraries = Vec::new();

    for target_name in &built_target_names {
        let package_target = targets_by_name
            .get(target_name.as_str())
            .copied()
            .with_context(|| format!("missing Swift package target `{target_name}`"))?;
        let source_root = package_target
            .path
            .as_ref()
            .map(|path| package_root.join(path))
            .unwrap_or_else(|| package_root.join("Sources").join(target_name));
        let swift_sources = collect_files_with_extensions(&source_root, &["swift"])?;
        if swift_sources.is_empty() {
            bail!(
                "Swift package target `{target_name}` under `{}` does not contain any Swift sources",
                source_root.display()
            );
        }

        let library_name = swift_package_library_name(&package.name, target_name);
        let module_path = module_dir.join(format!("{target_name}.swiftmodule"));
        let library_path = library_dir.join(format!("lib{library_name}.a"));
        let mut command = toolchain.swiftc();
        command.arg("-parse-as-library");
        command.arg("-target").arg(&toolchain.target_triple);
        command.arg("-emit-library");
        command.arg("-static");
        command.arg("-emit-module");
        command.arg("-module-name").arg(target_name);
        command.arg("-o").arg(&library_path);
        command.arg("-emit-module-path").arg(&module_path);
        if matches!(profile.configuration.as_str(), "debug") {
            command.args(["-Onone", "-g"]);
        } else {
            command.arg("-O");
        }
        command.arg("-I").arg(&module_dir);
        command.arg("-L").arg(&library_dir);
        for dependency_name in package_target_local_dependencies(package_target, &targets_by_name)?
        {
            command
                .arg("-l")
                .arg(swift_package_library_name(&package.name, &dependency_name));
        }
        for source in swift_sources {
            command.arg(source);
        }
        build_progress_step(
            format!(
                "Compiling Swift package target `{}` from `{}`",
                target_name, dependency.product
            ),
            |_| {
                format!(
                    "Compiled Swift package target `{}` from `{}`.",
                    target_name, dependency.product
                )
            },
            || run_command(&mut command),
        )?;
        built_libraries.push(library_name);
    }

    Ok(PackageBuildOutput {
        module_dir,
        library_dir,
        link_libraries: built_libraries,
    })
}

fn ordered_package_targets(
    package: &SwiftPackageManifest,
    root_targets: &[String],
) -> Result<Vec<String>> {
    let targets_by_name = package
        .targets
        .iter()
        .map(|target| (target.name.as_str(), target))
        .collect::<HashMap<_, _>>();
    let mut ordered = Vec::new();
    let mut visiting = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();

    fn visit(
        target_name: &str,
        targets_by_name: &HashMap<&str, &SwiftPackageTarget>,
        ordered: &mut Vec<String>,
        visiting: &mut std::collections::BTreeSet<String>,
        visited: &mut std::collections::BTreeSet<String>,
    ) -> Result<()> {
        if visited.contains(target_name) {
            return Ok(());
        }
        if !visiting.insert(target_name.to_owned()) {
            bail!("Swift package target dependency cycle detected at `{target_name}`");
        }

        let target = targets_by_name
            .get(target_name)
            .copied()
            .with_context(|| format!("missing Swift package target `{target_name}`"))?;
        validate_package_target_kind(target)?;
        for dependency_name in package_target_local_dependencies(target, targets_by_name)? {
            visit(
                &dependency_name,
                targets_by_name,
                ordered,
                visiting,
                visited,
            )?;
        }

        visiting.remove(target_name);
        visited.insert(target_name.to_owned());
        ordered.push(target_name.to_owned());
        Ok(())
    }

    for target_name in root_targets {
        visit(
            target_name,
            &targets_by_name,
            &mut ordered,
            &mut visiting,
            &mut visited,
        )?;
    }

    Ok(ordered)
}

fn validate_package_target_kind(target: &SwiftPackageTarget) -> Result<()> {
    match target.kind.as_deref().unwrap_or("regular") {
        "regular" => Ok(()),
        other => bail!(
            "Swift package target `{}` has unsupported kind `{other}`",
            target.name
        ),
    }
}

fn package_target_local_dependencies(
    target: &SwiftPackageTarget,
    targets_by_name: &HashMap<&str, &SwiftPackageTarget>,
) -> Result<Vec<String>> {
    let mut dependencies = Vec::new();
    for dependency in &target.dependencies {
        match dependency {
            SwiftPackageTargetDependency::ByName { by_name } => {
                if targets_by_name.contains_key(by_name.0.as_str()) {
                    dependencies.push(by_name.0.clone());
                }
            }
            SwiftPackageTargetDependency::Target { target } => {
                dependencies.push(target.0.clone());
            }
            SwiftPackageTargetDependency::Product { product } => {
                bail!(
                    "Swift package target `{}` depends on external product `{}`; Orbit only supports local package target graphs for now",
                    target.name,
                    product.0
                );
            }
        }
    }
    dependencies.sort();
    dependencies.dedup();
    Ok(dependencies)
}

fn swift_package_library_name(package_name: &str, target_name: &str) -> String {
    format!(
        "{}_{}",
        sanitize_library_name_component(package_name),
        sanitize_library_name_component(target_name)
    )
}

fn sanitize_library_name_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
struct SimctlList {
    devices: BTreeMap<String, Vec<SimulatorDevice>>,
}

#[derive(Debug, Clone, Deserialize)]
struct SimulatorDevice {
    udid: String,
    name: String,
    state: String,
}

impl SimulatorDevice {
    fn is_booted(&self) -> bool {
        self.state.eq_ignore_ascii_case("Booted")
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCtlList {
    result: DeviceCtlResult,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCtlResult {
    devices: Vec<PhysicalDevice>,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalDevice {
    identifier: String,
    #[serde(rename = "deviceProperties")]
    device_properties: PhysicalDeviceProperties,
    #[serde(rename = "hardwareProperties")]
    hardware_properties: PhysicalHardwareProperties,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalDeviceProperties {
    name: String,
    #[serde(rename = "osBuildUpdate")]
    os_build_update: Option<String>,
    #[serde(rename = "osVersionNumber")]
    os_version_number: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalHardwareProperties {
    #[serde(rename = "cpuType")]
    cpu_type: PhysicalCpuType,
    platform: String,
    #[serde(rename = "productType")]
    product_type: Option<String>,
    udid: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalCpuType {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceInstallResponse {
    result: DeviceInstallResult,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceInstallResult {
    #[serde(rename = "installedApplications")]
    installed_applications: Vec<InstalledApplication>,
}

#[derive(Debug, Clone, Deserialize)]
struct InstalledApplication {
    #[serde(rename = "installationURL")]
    installation_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceLaunchResponse {
    result: DeviceLaunchResult,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceLaunchResult {
    process: DeviceLaunchedProcess,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceLaunchedProcess {
    #[serde(rename = "processIdentifier")]
    process_identifier: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceProcessesResponse {
    result: DeviceProcessesResult,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceProcessesResult {
    #[serde(rename = "runningProcesses", default)]
    running_processes: Vec<DeviceRunningProcess>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceRunningProcess {
    executable: Option<String>,
    #[serde(rename = "processIdentifier")]
    process_identifier: u64,
}

fn select_physical_device(
    project: &ProjectContext,
    requested_identifier: Option<&str>,
    platform: ApplePlatform,
) -> Result<PhysicalDevice> {
    let mut physical = list_devicectl_devices(platform)?;

    if let Some(identifier) = requested_identifier {
        return physical
            .into_iter()
            .find(|device| {
                device.identifier == identifier
                    || device.hardware_properties.udid == identifier
                    || device.device_properties.name == identifier
            })
            .with_context(|| format!("no connected {platform} device matched `{identifier}`"));
    }

    if physical.is_empty() {
        bail!("no connected {platform} devices were found through `devicectl`");
    }

    if !project.app.interactive || physical.len() == 1 {
        return Ok(physical.remove(0));
    }

    let labels = physical
        .iter()
        .map(|device| {
            format!(
                "{} ({})",
                device.device_properties.name, device.hardware_properties.udid
            )
        })
        .collect::<Vec<_>>();
    let index = prompt_select("Select a physical device", &labels)?;
    Ok(physical.remove(index))
}

fn list_devicectl_devices(platform: ApplePlatform) -> Result<Vec<PhysicalDevice>> {
    let output_path = NamedTempFile::new()?;
    let mut list = Command::new("xcrun");
    list.args([
        "devicectl",
        "list",
        "devices",
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
    ]);
    run_command(&mut list)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let devices: DeviceCtlList = serde_json::from_str(&contents)?;
    Ok(devices
        .result
        .devices
        .into_iter()
        .filter(|device| physical_device_matches_platform(device, platform))
        .collect::<Vec<_>>())
}

fn ensure_device_symbols_available(
    project: &ProjectContext,
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<Option<PathBuf>> {
    let symbol_root = resolve_device_symbol_root(project, device, platform);
    if device_symbol_root_ready(&symbol_root) {
        return Ok(Some(symbol_root));
    }

    ensure_device_is_unlocked_for_symbol_download(device, platform)?;

    let spinner = CliSpinner::new(format!("Caching device symbols for {platform}"));
    match prepare_device_support_symbols(device, platform) {
        Ok(()) => {
            let symbol_root = resolve_device_symbol_root(project, device, platform);
            if device_symbol_root_ready(&symbol_root) {
                spinner.finish_success(format!("Prepared device symbols for {platform}."));
                Ok(Some(symbol_root))
            } else {
                spinner.finish_warning(format!(
                    "Orbit prepared device support for {platform}, but no usable symbol root was found. LLDB will fall back to reading symbols from the device."
                ));
                Ok(None)
            }
        }
        Err(error) => {
            if error_mentions_locked_device(&error.to_string()) {
                spinner.finish_clear();
                return Err(error);
            }
            spinner.finish_warning(format!(
                "Orbit could not cache device symbols for {platform}: {error}. LLDB will fall back to reading symbols from the device."
            ));
            Ok(None)
        }
    }
}

fn prepare_device_support_symbols(device: &PhysicalDevice, platform: ApplePlatform) -> Result<()> {
    let os_version = device
        .device_properties
        .os_version_number
        .as_deref()
        .context("device is missing an OS version in `devicectl list devices` output")?;
    let model_code = device
        .hardware_properties
        .product_type
        .as_deref()
        .context("device is missing a product type in `devicectl list devices` output")?;

    let mut command = Command::new("xcodebuild");
    command.args([
        "-prepareDeviceSupport",
        "-platform",
        devicectl_platform_name(platform),
        "-osVersion",
        os_version,
        "-modelCode",
        model_code,
        "-architecture",
        &device.hardware_properties.cpu_type.name,
    ]);
    let debug = crate::util::debug_command(&command);
    let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
    let output = combine_command_output(&stdout, &stderr);
    if error_mentions_locked_device(&output) {
        bail!(locked_device_symbol_download_message(device));
    }
    if !success {
        bail!("`{debug}` failed\nstdout:\n{}\nstderr:\n{}", stdout, stderr);
    }

    Ok(())
}

fn ensure_device_is_unlocked_for_symbol_download(
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<()> {
    ensure_device_is_unlocked(
        device,
        platform,
        locked_device_symbol_download_message(device),
    )
}

fn ensure_device_is_unlocked_for_debugging(
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<()> {
    ensure_device_is_unlocked(device, platform, locked_device_debug_message(device))
}

fn ensure_device_is_unlocked(
    device: &PhysicalDevice,
    platform: ApplePlatform,
    failure_message: String,
) -> Result<()> {
    if platform == ApplePlatform::Macos {
        return Ok(());
    }

    let output_path = NamedTempFile::new()?;
    let mut command = Command::new("xcrun");
    command.args([
        "devicectl",
        "device",
        "info",
        "lockState",
        "--device",
        &device.hardware_properties.udid,
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
    ]);
    let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
    let output = combine_command_output(&stdout, &stderr);
    if error_mentions_locked_device(&output) {
        bail!(failure_message);
    }
    if !success || !output_path.path().exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let details: serde_json::Value = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device info lockState` output")?;
    if device_is_locked_from_details(&details).unwrap_or(false) {
        bail!(failure_message);
    }

    Ok(())
}

fn resolve_device_symbol_root(
    project: &ProjectContext,
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> PathBuf {
    let support_root = device_support_root(project, platform);
    let candidates = device_support_label_candidates(device)
        .into_iter()
        .map(|label| support_root.join(label).join("Symbols"))
        .collect::<Vec<_>>();
    candidates
        .iter()
        .find(|candidate| device_symbol_root_ready(candidate))
        .cloned()
        .unwrap_or_else(|| {
            candidates.into_iter().next().unwrap_or_else(|| {
                support_root
                    .join(format!(
                        "Orbit {}",
                        sanitize_device_support_component(&device.hardware_properties.udid)
                    ))
                    .join("Symbols")
            })
        })
}

fn device_support_label_from_device(device: &PhysicalDevice) -> Option<String> {
    match (
        device.device_properties.os_version_number.as_deref(),
        device.device_properties.os_build_update.as_deref(),
    ) {
        (Some(version), Some(build)) if version != build => Some(format!("{version} ({build})")),
        (Some(version), _) => Some(version.to_owned()),
        (_, Some(build)) => Some(build.to_owned()),
        _ => None,
    }
}

fn device_support_root(project: &ProjectContext, platform: ApplePlatform) -> PathBuf {
    dirs::home_dir()
        .map(|home| {
            home.join("Library")
                .join("Developer")
                .join("Xcode")
                .join(device_support_directory(platform))
        })
        .unwrap_or_else(|| {
            project
                .app
                .global_paths
                .cache_dir
                .join("device-support")
                .join(platform.to_string())
        })
}

fn device_support_label_candidates(device: &PhysicalDevice) -> Vec<String> {
    let mut labels = Vec::new();
    if let Some(label) = device_support_model_label_from_device(device) {
        labels.push(label);
    }
    if let Some(label) = device_support_label_from_device(device) {
        labels.push(label);
    }
    if labels.is_empty() {
        labels.push(format!(
            "Orbit {}",
            sanitize_device_support_component(&device.hardware_properties.udid)
        ));
    }
    labels
}

fn device_support_model_label_from_device(device: &PhysicalDevice) -> Option<String> {
    let model = device.hardware_properties.product_type.as_deref()?;
    let base = device_support_label_from_device(device)?;
    Some(format!("{model} {base}"))
}

fn json_value_label(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Array(values) => values.iter().find_map(json_value_label),
        serde_json::Value::Object(map) => {
            let major = map.get("major").and_then(serde_json::Value::as_u64);
            let minor = map.get("minor").and_then(serde_json::Value::as_u64);
            if let (Some(major), Some(minor)) = (major, minor) {
                let patch = map.get("patch").and_then(serde_json::Value::as_u64);
                return Some(match patch {
                    Some(patch) => format!("{major}.{minor}.{patch}"),
                    None => format!("{major}.{minor}"),
                });
            }

            for key in [
                "description",
                "stringValue",
                "value",
                "buildVersion",
                "productBuildVersion",
                "build",
                "trainName",
                "name",
            ] {
                if let Some(label) = map.get(key).and_then(json_value_label) {
                    return Some(label);
                }
            }

            map.values().find_map(json_value_label)
        }
        serde_json::Value::Bool(_) | serde_json::Value::Null => None,
    }
}

fn device_is_locked_from_details(details: &serde_json::Value) -> Option<bool> {
    match details {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_ascii_lowercase();
                if matches!(key.as_str(), "passcoderequired" | "ispasscoderequired") {
                    if let Some(value) = value.as_bool() {
                        return Some(value);
                    }
                }
                if matches!(key.as_str(), "islocked" | "locked") {
                    if let Some(value) = value.as_bool() {
                        return Some(value);
                    }
                    if let Some(value) = json_value_label(value)
                        .as_deref()
                        .and_then(parse_lock_state_label)
                    {
                        return Some(value);
                    }
                }
                if key.contains("lockstate") {
                    if let Some(value) = parse_lock_state_value(value) {
                        return Some(value);
                    }
                }
            }

            map.values().find_map(device_is_locked_from_details)
        }
        serde_json::Value::Array(values) => values.iter().find_map(device_is_locked_from_details),
        _ => None,
    }
}

fn parse_lock_state_value(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::String(value) => parse_lock_state_label(value),
        serde_json::Value::Object(map) => {
            for key in ["name", "description", "stringValue", "value"] {
                if let Some(value) = map.get(key).and_then(json_value_label) {
                    if let Some(value) = parse_lock_state_label(&value) {
                        return Some(value);
                    }
                }
            }
            map.values().find_map(parse_lock_state_value)
        }
        serde_json::Value::Array(values) => values.iter().find_map(parse_lock_state_value),
        serde_json::Value::Number(_) | serde_json::Value::Null => None,
    }
}

fn parse_lock_state_label(value: &str) -> Option<bool> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("unlocked") {
        return Some(false);
    }
    if normalized.contains("locked") {
        return Some(true);
    }
    None
}

fn device_support_directory(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Ios => "iOS DeviceSupport",
        ApplePlatform::Macos => "macOS DeviceSupport",
        ApplePlatform::Tvos => "tvOS DeviceSupport",
        ApplePlatform::Visionos => "visionOS DeviceSupport",
        ApplePlatform::Watchos => "watchOS DeviceSupport",
    }
}

fn sanitize_device_support_component(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect()
}

fn device_symbol_cache_dir(symbol_root: &Path) -> PathBuf {
    symbol_root
        .join("System")
        .join("Library")
        .join("Caches")
        .join("com.apple.dyld")
}

fn device_symbol_root_ready(symbol_root: &Path) -> bool {
    if symbol_root.join("usr").join("lib").join("dyld").exists() {
        return true;
    }
    count_device_symbol_cache_files(symbol_root) > 0
}

fn count_device_symbol_cache_files(symbol_root: &Path) -> usize {
    let cache_dir = device_symbol_cache_dir(symbol_root);
    cache_dir
        .read_dir()
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("dyld_shared_cache_"))
        })
        .count()
}

fn combine_command_output(stdout: &str, stderr: &str) -> String {
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_owned(),
        (true, false) => stderr.to_owned(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

fn error_mentions_locked_device(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("device is locked")
        || normalized.contains("device needs to be unlocked")
        || normalized.contains("unlock the device and try again")
        || normalized.contains("operation failed since the device is locked")
}

fn locked_device_symbol_download_message(device: &PhysicalDevice) -> String {
    format!(
        "device symbol download requires an unlocked device. Unlock {} ({}) and try again.",
        device.device_properties.name, device.hardware_properties.udid
    )
}

fn locked_device_debug_message(device: &PhysicalDevice) -> String {
    format!(
        "device debugging requires an unlocked device. Unlock {} ({}) and try again.",
        device.device_properties.name, device.hardware_properties.udid
    )
}

fn lldb_quote_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn devicectl_platform_name(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Ios => "iOS",
        ApplePlatform::Macos => "macOS",
        ApplePlatform::Tvos => "tvOS",
        ApplePlatform::Visionos => "visionOS",
        ApplePlatform::Watchos => "watchOS",
    }
}

fn simulator_runtime_matches_platform(runtime_identifier: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios => runtime_identifier.contains(".SimRuntime.iOS-"),
        ApplePlatform::Tvos => runtime_identifier.contains(".SimRuntime.tvOS-"),
        ApplePlatform::Visionos => {
            runtime_identifier.contains(".SimRuntime.xrOS-")
                || runtime_identifier.contains(".SimRuntime.visionOS-")
        }
        ApplePlatform::Watchos => runtime_identifier.contains(".SimRuntime.watchOS-"),
        ApplePlatform::Macos => runtime_identifier.contains(".SimRuntime.macOS-"),
    }
}

fn physical_device_matches_platform(device: &PhysicalDevice, platform: ApplePlatform) -> bool {
    let platform_name = device.hardware_properties.platform.as_str();
    match platform {
        ApplePlatform::Ios => platform_name.eq_ignore_ascii_case("iOS"),
        ApplePlatform::Tvos => platform_name.eq_ignore_ascii_case("tvOS"),
        ApplePlatform::Visionos => {
            platform_name.eq_ignore_ascii_case("visionOS")
                || platform_name.eq_ignore_ascii_case("xrOS")
        }
        ApplePlatform::Watchos => platform_name.eq_ignore_ascii_case("watchOS"),
        ApplePlatform::Macos => platform_name.eq_ignore_ascii_case("macOS"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use plist::{Dictionary, Value};
    use serde_json::json;
    use tempfile::TempDir;

    use super::{
        ApplePlatform, DestinationKind, DeviceRunningProcess, ExtensionManifest,
        SwiftPackageManifest, SwiftPackageProduct, SwiftPackageTarget,
        SwiftPackageTargetDependency, TargetKind, Toolchain, XcframeworkLibrary,
        device_is_locked_from_details, device_support_label_from_device, embedded_dependency_root,
        error_mentions_locked_device, extension_plist, find_running_process_for_installation,
        json_to_plist, lldb_expect_attach_script, merge_extension_attributes,
        merge_partial_info_plist, ordered_package_targets, relocate_bundle_debug_artifacts,
        select_xcframework_library, write_info_plist,
    };
    use crate::context::{AppContext, GlobalPaths, ProjectContext, ProjectPaths};
    use crate::manifest::{
        IosDeviceFamily, IosInterfaceOrientation, IosSupportedOrientationsManifest,
        IosTargetManifest, Manifest,
    };

    fn fixture(path: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    fn project_for_fixture(path: &str) -> (TempDir, ProjectContext) {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = fixture(path);
        let root = manifest_path.parent().unwrap().to_path_buf();
        let manifest = Manifest::load(&manifest_path).unwrap();
        let data_dir = temp.path().join("data");
        let cache_dir = temp.path().join("cache");
        let orbit_dir = temp.path().join("orbit");
        let build_dir = orbit_dir.join("build");
        let artifacts_dir = orbit_dir.join("artifacts");
        let receipts_dir = orbit_dir.join("receipts");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        std::fs::create_dir_all(&receipts_dir).unwrap();

        let project = ProjectContext {
            app: AppContext {
                cwd: root.clone(),
                interactive: false,
                global_paths: GlobalPaths {
                    data_dir: data_dir.clone(),
                    cache_dir,
                    auth_state_path: data_dir.join("auth.json"),
                    device_cache_path: data_dir.join("devices.json"),
                    keychain_path: data_dir.join("orbit.keychain-db"),
                },
            },
            root,
            manifest_path,
            manifest,
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
    fn derives_device_support_label_from_selected_device_metadata() {
        let device = super::PhysicalDevice {
            identifier: "device-id".to_owned(),
            device_properties: super::PhysicalDeviceProperties {
                name: "Example iPhone".to_owned(),
                os_build_update: Some("22C161".to_owned()),
                os_version_number: Some("18.2.1".to_owned()),
            },
            hardware_properties: super::PhysicalHardwareProperties {
                cpu_type: super::PhysicalCpuType {
                    name: "arm64e".to_owned(),
                },
                platform: "iOS".to_owned(),
                product_type: Some("iPhone16,2".to_owned()),
                udid: "00000000-0000000000000000".to_owned(),
            },
        };

        assert_eq!(
            device_support_label_from_device(&device).as_deref(),
            Some("18.2.1 (22C161)")
        );
    }

    #[test]
    fn detects_locked_device_from_devicectl_lock_state_details() {
        let details = json!({
            "result": {
                "device": {
                    "lockState": {
                        "name": "locked"
                    }
                }
            }
        });

        assert_eq!(device_is_locked_from_details(&details), Some(true));
    }

    #[test]
    fn detects_unlocked_device_from_devicectl_lock_state_details() {
        let details = json!({
            "result": {
                "device": {
                    "connectionProperties": {
                        "lockState": "unlocked"
                    }
                }
            }
        });

        assert_eq!(device_is_locked_from_details(&details), Some(false));
    }

    #[test]
    fn detects_locked_device_from_passcode_required_field() {
        let details = json!({
            "result": {
                "deviceIdentifier": "F1E218C7-32D3-5E36-BD5D-BC0CA366504B",
                "passcodeRequired": true,
                "unlockedSinceBoot": true
            }
        });

        assert_eq!(device_is_locked_from_details(&details), Some(true));
    }

    #[test]
    fn recognizes_locked_device_errors_from_tool_output() {
        assert!(error_mentions_locked_device(
            "The operation failed since the device is locked. Unlock the device and try again."
        ));
        assert!(error_mentions_locked_device("Device needs to be unlocked."));
        assert!(!error_mentions_locked_device(
            "Failed to connect to remote service."
        ));
    }

    #[test]
    fn writes_ios_app_defaults_without_scene_manifest_inference() {
        let (temp, project) = project_for_fixture("examples/ios-simulator-app/orbit.json");
        let target = project
            .manifest
            .resolve_target(Some("ExampleIOSApp"))
            .unwrap()
            .clone();
        let bundle_root = temp.path().join("ExampleIOSApp.app");
        std::fs::create_dir_all(&bundle_root).unwrap();
        let toolchain = Toolchain {
            platform: ApplePlatform::Ios,
            destination: DestinationKind::Device,
            sdk_name: "iphoneos".to_owned(),
            sdk_path: PathBuf::from("/tmp/iphoneos.sdk"),
            deployment_target: "18.0".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "arm64-apple-ios18.0".to_owned(),
        };

        write_info_plist(&project, &toolchain, &target, &bundle_root, "development").unwrap();

        let plist = Value::from_file(bundle_root.join("Info.plist")).unwrap();
        let dict = plist.as_dictionary().unwrap();
        let device_family = dict
            .get("UIDeviceFamily")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(
            device_family
                .iter()
                .filter_map(Value::as_signed_integer)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            dict.get("CFBundleDevelopmentRegion")
                .and_then(Value::as_string),
            Some("en")
        );
        assert!(!dict.contains_key("UIApplicationSceneManifest"));
        assert_eq!(
            dict.get("UIRequiredDeviceCapabilities")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_string)
                        .collect::<Vec<_>>()
                }),
            Some(vec!["arm64"])
        );
        assert_eq!(
            dict.get("UILaunchScreen")
                .and_then(Value::as_dictionary)
                .and_then(|launch_screen| launch_screen.get("UILaunchScreen"))
                .and_then(Value::as_dictionary)
                .map(Dictionary::is_empty),
            Some(true)
        );
        assert!(dict.contains_key("UISupportedInterfaceOrientations~iphone"));
        assert_eq!(
            dict.get("UIApplicationSupportsIndirectInputEvents")
                .and_then(Value::as_boolean),
            Some(true)
        );
        assert_eq!(
            std::fs::read_to_string(bundle_root.join("PkgInfo")).unwrap(),
            "APPL????"
        );
    }

    #[test]
    fn applies_manifest_driven_ios_plist_metadata() {
        let (temp, project) = project_for_fixture("examples/ios-simulator-app/orbit.json");
        let mut target = project
            .manifest
            .resolve_target(Some("ExampleIOSApp"))
            .unwrap()
            .clone();
        target.display_name = Some("Orbit Example".to_owned());
        target.build_number = Some("42".to_owned());
        target.info_plist.insert(
            "NSCameraUsageDescription".to_owned(),
            json!("Camera access is required."),
        );
        target.info_plist.insert(
            "UIStatusBarStyle".to_owned(),
            json!("UIStatusBarStyleLightContent"),
        );
        target.ios = Some(IosTargetManifest {
            device_families: Some(vec![IosDeviceFamily::Iphone]),
            supported_orientations: Some(IosSupportedOrientationsManifest {
                iphone: Some(vec![IosInterfaceOrientation::Portrait]),
                ipad: Some(vec![IosInterfaceOrientation::LandscapeLeft]),
            }),
            required_device_capabilities: Some(vec!["arm64".to_owned(), "metal".to_owned()]),
            launch_screen: Some(BTreeMap::from([(
                "UIColorName".to_owned(),
                json!("LaunchBackground"),
            )])),
        });

        let bundle_root = temp.path().join("ExampleIOSApp.app");
        std::fs::create_dir_all(&bundle_root).unwrap();
        let toolchain = Toolchain {
            platform: ApplePlatform::Ios,
            destination: DestinationKind::Device,
            sdk_name: "iphoneos".to_owned(),
            sdk_path: PathBuf::from("/tmp/iphoneos.sdk"),
            deployment_target: "18.0".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "arm64-apple-ios18.0".to_owned(),
        };

        write_info_plist(&project, &toolchain, &target, &bundle_root, "development").unwrap();

        let plist = Value::from_file(bundle_root.join("Info.plist")).unwrap();
        let dict = plist.as_dictionary().unwrap();
        assert_eq!(
            dict.get("CFBundleDisplayName").and_then(Value::as_string),
            Some("Orbit Example")
        );
        assert_eq!(
            dict.get("CFBundleVersion").and_then(Value::as_string),
            Some("42")
        );
        assert_eq!(
            dict.get("UIDeviceFamily")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_signed_integer)
                        .collect::<Vec<_>>()
                }),
            Some(vec![1])
        );
        assert_eq!(
            dict.get("UIRequiredDeviceCapabilities")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_string)
                        .collect::<Vec<_>>()
                }),
            Some(vec!["arm64", "metal"])
        );
        assert_eq!(
            dict.get("UISupportedInterfaceOrientations~iphone")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_string)
                        .collect::<Vec<_>>()
                }),
            Some(vec!["UIInterfaceOrientationPortrait"])
        );
        assert!(!dict.contains_key("UISupportedInterfaceOrientations~ipad"));
        assert_eq!(
            dict.get("UILaunchScreen")
                .and_then(Value::as_dictionary)
                .and_then(|launch_screen| launch_screen.get("UILaunchScreen"))
                .and_then(Value::as_dictionary)
                .and_then(|launch_screen| launch_screen.get("UIColorName"))
                .and_then(Value::as_string),
            Some("LaunchBackground")
        );
        assert_eq!(
            dict.get("NSCameraUsageDescription")
                .and_then(Value::as_string),
            Some("Camera access is required.")
        );
        assert_eq!(
            dict.get("UIStatusBarStyle").and_then(Value::as_string),
            Some("UIStatusBarStyleLightContent")
        );
    }

    #[test]
    fn relocates_bundle_dsym_out_of_app_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let target_dir = temp.path().join("ExampleIOSApp");
        let bundle_root = target_dir.join("ExampleIOSApp.app");
        std::fs::create_dir_all(&bundle_root).unwrap();
        let binary_path = bundle_root.join("ExampleIOSApp");
        std::fs::write(&binary_path, b"binary").unwrap();
        let bundle_dsym = binary_path.with_extension("dSYM");
        std::fs::create_dir_all(bundle_dsym.join("Contents")).unwrap();

        relocate_bundle_debug_artifacts(&target_dir, &bundle_root, &binary_path).unwrap();

        assert!(!bundle_dsym.exists());
        assert!(target_dir.join("ExampleIOSApp.dSYM").exists());
    }

    #[test]
    fn finds_running_process_for_installation_url() {
        let processes = vec![
            DeviceRunningProcess {
                executable: Some("file:///private/var/containers/Bundle/Application/OTHER/Other.app/Other".to_owned()),
                process_identifier: 41,
            },
            DeviceRunningProcess {
                executable: Some("file:///private/var/containers/Bundle/Application/EXAMPLE/ExampleIOSApp.app/ExampleIOSApp".to_owned()),
                process_identifier: 99,
            },
        ];

        let process = find_running_process_for_installation(
            &processes,
            "file:///private/var/containers/Bundle/Application/EXAMPLE/ExampleIOSApp.app/",
        )
        .expect("expected matching process");

        assert_eq!(process.process_identifier, 99);
    }

    #[test]
    fn lldb_expect_script_waits_for_attach_before_continue() {
        let script =
            lldb_expect_attach_script(Some(Path::new("/tmp/iOS DeviceSupport/Symbols"))).unwrap();

        assert!(script.contains("device process attach --pid $pid"));
        assert!(script.contains("wait_for_log [format {Process %s stopped} $pid]"));
        assert!(script.contains("send -- \"process continue\\r\""));
        assert!(script.contains("settings append target.exec-search-paths"));
        assert!(script.contains("interact"));
    }

    #[test]
    fn merges_actool_partial_info_plist_into_bundle_info() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("Example.app");
        std::fs::create_dir_all(&bundle_root).unwrap();
        Value::Dictionary(Dictionary::from_iter([(
            "CFBundleIdentifier".to_owned(),
            Value::String("dev.orbit.example".to_owned()),
        )]))
        .to_file_xml(bundle_root.join("Info.plist"))
        .unwrap();
        let partial_path = temp.path().join("partial.plist");
        Value::Dictionary(Dictionary::from_iter([
            (
                "NSAccentColorName".to_owned(),
                Value::String("AccentColor".to_owned()),
            ),
            (
                "CFBundleIcons".to_owned(),
                Value::Dictionary(Dictionary::from_iter([(
                    "CFBundlePrimaryIcon".to_owned(),
                    Value::Dictionary(Dictionary::new()),
                )])),
            ),
        ]))
        .to_file_xml(&partial_path)
        .unwrap();

        merge_partial_info_plist(&bundle_root, &partial_path).unwrap();

        let merged = Value::from_file(bundle_root.join("Info.plist")).unwrap();
        let dict = merged.as_dictionary().unwrap();
        assert_eq!(
            dict.get("NSAccentColorName").and_then(Value::as_string),
            Some("AccentColor")
        );
        assert!(
            dict.get("CFBundleIcons")
                .and_then(Value::as_dictionary)
                .is_some()
        );
    }

    #[test]
    fn embeds_watch_children_into_expected_subdirectories() {
        let (_temp, project) = project_for_fixture("examples/ios-watch-app/orbit.json");
        let app = project
            .manifest
            .resolve_target(Some("ExampleCompanionApp"))
            .unwrap();
        let watch_app = project
            .manifest
            .resolve_target(Some("ExampleWatchApp"))
            .unwrap();
        let watch_extension = project
            .manifest
            .resolve_target(Some("ExampleWatchExtension"))
            .unwrap();
        assert_eq!(
            embedded_dependency_root(&project, app, watch_app).unwrap(),
            Some("Watch")
        );
        assert_eq!(
            embedded_dependency_root(&project, watch_app, watch_extension).unwrap(),
            Some("PlugIns")
        );
        assert_eq!(
            embedded_dependency_root(&project, watch_app, watch_app).unwrap(),
            None
        );
        let framework = crate::manifest::TargetManifest {
            name: "OrbitFramework".to_owned(),
            kind: TargetKind::Framework,
            bundle_id: "dev.orbit.framework".to_owned(),
            display_name: None,
            build_number: None,
            platforms: vec![ApplePlatform::Watchos],
            sources: vec!["Sources/Framework".into()],
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
        };
        assert_eq!(
            embedded_dependency_root(&project, watch_app, &framework).unwrap(),
            Some("Frameworks")
        );
    }

    #[test]
    fn embeds_app_clips_into_appclips_directory() {
        let (_temp, project) = project_for_fixture("examples/ios-app-clip/orbit.json");
        let app = project.manifest.resolve_target(Some("ExampleApp")).unwrap();
        let clip = project
            .manifest
            .resolve_target(Some("ExampleClip"))
            .unwrap();

        assert_eq!(
            embedded_dependency_root(&project, app, clip).unwrap(),
            Some("AppClips")
        );
    }

    #[test]
    fn preserves_extra_extension_entries() {
        let extension = ExtensionManifest {
            point_identifier: "com.apple.widgetkit-extension".to_owned(),
            principal_class: "WidgetPrincipal".to_owned(),
            extra: BTreeMap::from([(
                "NSExtensionAttributes".to_owned(),
                json!({
                    "WKBackgroundModes": ["workout-processing"]
                }),
            )]),
        };
        let mut plist = extension_plist(&extension).unwrap();
        merge_extension_attributes(
            &mut plist,
            Dictionary::from_iter([(
                "WKAppBundleIdentifier".to_owned(),
                plist::Value::String("dev.orbit.examples.watch.watchkitapp".to_owned()),
            )]),
        );

        let attributes = plist
            .get("NSExtensionAttributes")
            .and_then(plist::Value::as_dictionary)
            .unwrap();
        assert_eq!(
            attributes
                .get("WKBackgroundModes")
                .and_then(plist::Value::as_array)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            attributes
                .get("WKAppBundleIdentifier")
                .and_then(plist::Value::as_string)
                .unwrap(),
            "dev.orbit.examples.watch.watchkitapp"
        );
    }

    #[test]
    fn converts_nested_json_values_into_plist_values() {
        let value = json_to_plist(&json!({
            "Enabled": true,
            "Count": 3,
            "Items": ["one", "two"]
        }))
        .unwrap();
        let dictionary = value.as_dictionary().unwrap();
        assert_eq!(
            dictionary
                .get("Enabled")
                .and_then(plist::Value::as_boolean)
                .unwrap(),
            true
        );
        assert_eq!(
            dictionary
                .get("Items")
                .and_then(plist::Value::as_array)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn selects_matching_xcframework_slice_for_target_platform() {
        let toolchain = Toolchain {
            platform: ApplePlatform::Ios,
            destination: DestinationKind::Simulator,
            sdk_name: "iphonesimulator".to_owned(),
            sdk_path: "/tmp/sdk".into(),
            deployment_target: "18.0".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "arm64-apple-ios18.0-simulator".to_owned(),
        };
        let slices = vec![
            XcframeworkLibrary {
                library_identifier: "ios-arm64".to_owned(),
                library_path: "Orbit.framework".to_owned(),
                headers_path: None,
                supported_platform: "ios".to_owned(),
                supported_platform_variant: None,
                supported_architectures: vec!["arm64".to_owned()],
            },
            XcframeworkLibrary {
                library_identifier: "ios-arm64_x86_64-simulator".to_owned(),
                library_path: "Orbit.framework".to_owned(),
                headers_path: None,
                supported_platform: "ios".to_owned(),
                supported_platform_variant: Some("simulator".to_owned()),
                supported_architectures: vec!["arm64".to_owned(), "x86_64".to_owned()],
            },
        ];

        let selected = select_xcframework_library(&toolchain, &slices).unwrap();
        assert_eq!(selected.library_identifier, "ios-arm64_x86_64-simulator");
    }

    #[test]
    fn orders_swift_package_targets_by_local_dependencies() {
        let package = SwiftPackageManifest {
            name: "FeaturePackage".to_owned(),
            products: vec![SwiftPackageProduct {
                name: "Feature".to_owned(),
                targets: vec!["Feature".to_owned()],
            }],
            targets: vec![
                SwiftPackageTarget {
                    name: "Core".to_owned(),
                    path: None,
                    dependencies: Vec::new(),
                    kind: Some("regular".to_owned()),
                },
                SwiftPackageTarget {
                    name: "Feature".to_owned(),
                    path: None,
                    dependencies: vec![SwiftPackageTargetDependency::ByName {
                        by_name: ("Core".to_owned(), None),
                    }],
                    kind: Some("regular".to_owned()),
                },
            ],
        };

        let ordered = ordered_package_targets(&package, &["Feature".to_owned()]).unwrap();
        assert_eq!(ordered, vec!["Core".to_owned(), "Feature".to_owned()]);
    }
}
