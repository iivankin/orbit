mod artifacts;
mod cache;
mod compile;
#[path = "pipeline/plist.rs"]
mod info_plist;
mod resources;
mod runtime;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use self::artifacts::{export_artifact, remove_existing_path};
use self::compile::{CompileOutputMode, CompileOutputOptions, compile_target, embed_dependencies};
use self::runtime::{
    debug_on_device, debug_on_macos, debug_on_simulator, run_on_device, run_on_macos,
    run_on_simulator, select_physical_device, validate_run_platform,
};
pub(crate) use self::runtime::{macos_executable_path, prepare_macos_trace_launch_executable};
use crate::apple::build::receipt::{BuildReceipt, BuildReceiptInput, write_receipt};
use crate::apple::build::toolchain::{DestinationKind, Toolchain};
use crate::apple::build::verify::{should_verify_developer_id_artifact, verify_post_build};
use crate::apple::hooks::{HookContext, HookKind, run_project_hooks};
use crate::apple::runtime::{
    apple_platform_from_cli, build_target_for_platform, distribution_from_cli, profile_for_build,
    profile_for_run, resolve_build_distribution, resolve_platform,
};
use crate::cli::{BuildArgs, RunArgs};
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, BuildConfiguration, DistributionKind, ExtensionManifest, IosDeviceFamily,
    IosInterfaceOrientation, IosTargetManifest, ProfileManifest, TargetKind, TargetManifest,
};
use crate::util::{
    CliSpinner, copy_dir_recursive, copy_file, ensure_dir, ensure_parent_dir, prompt_select,
    resolve_path, run_command,
};
use anyhow::{Context, Result, bail};
use plist::{Dictionary, Value};

#[derive(Debug, Clone)]
struct BuildRequest {
    target_name: String,
    platform: ApplePlatform,
    profile: ProfileManifest,
    destination: DestinationKind,
    output: Option<PathBuf>,
    provisioning_udids: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct BuiltTarget {
    target_name: String,
    target_kind: TargetKind,
    target_dir: PathBuf,
    bundle_path: PathBuf,
    binary_path: PathBuf,
    module_output_path: Option<PathBuf>,
    code_phase_fingerprint: String,
    bundle_phase_fingerprint: String,
}

#[derive(Debug, Clone)]
struct ProductLayout {
    product_path: PathBuf,
    binary_path: PathBuf,
    module_output_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct ArchitectureBuild {
    toolchain: Toolchain,
    build_root: PathBuf,
    built_targets: HashMap<String, BuiltTarget>,
}

#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub receipt: BuildReceipt,
    pub receipt_path: PathBuf,
}

#[derive(Clone, Copy)]
enum BuildOutputMode {
    UserFacing,
    Silent,
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

fn profile_description(profile: &ProfileManifest) -> String {
    format!(
        "{} {}",
        profile.distribution.as_str(),
        profile.configuration.as_str()
    )
}

fn request_requires_signing(request: &BuildRequest) -> bool {
    build_requires_signing(&request.profile, request.destination)
}

pub fn build_artifact(project: &ProjectContext, args: &BuildArgs) -> Result<()> {
    let platform = resolve_platform(
        project,
        args.platform.map(apple_platform_from_cli),
        "Select a platform to build",
    )?;
    let target = build_target_for_platform(project, platform)?;
    let distribution =
        resolve_build_distribution(project, platform, distribution_from_cli(args.distribution))?;
    let profile = profile_for_build(distribution, args.release);
    let request = BuildRequest {
        target_name: target.name.clone(),
        platform,
        profile,
        destination: resolve_destination(
            project,
            platform,
            args.simulator,
            args.device,
            distribution,
        )?,
        output: args.output.clone(),
        provisioning_udids: None,
    };
    if request_requires_signing(&request) {
        crate::apple::auth::ensure_project_authenticated(project)?;
    }

    let outcome = build_project(project, &request, BuildOutputMode::UserFacing)?;
    crate::util::print_success(format!(
        "Built {} for {}.",
        outcome.receipt.target,
        profile_description(&request.profile)
    ));
    if should_verify_developer_id_artifact(&outcome.receipt) {
        build_progress_step(
            format!(
                "Verifying Developer ID artifact {}",
                outcome.receipt.artifact_path.display()
            ),
            String::clone,
            || verify_post_build(&outcome.receipt),
        )?;
    }
    println!("artifact: {}", outcome.receipt.artifact_path.display());
    println!("receipt: {}", outcome.receipt_path.display());
    Ok(())
}

pub fn build_for_testing_destination(
    project: &ProjectContext,
    platform: ApplePlatform,
    destination: DestinationKind,
) -> Result<BuildOutcome> {
    build_for_testing_destination_with_output(
        project,
        platform,
        destination,
        BuildOutputMode::UserFacing,
    )
}

pub fn build_for_testing_destination_silent(
    project: &ProjectContext,
    platform: ApplePlatform,
    destination: DestinationKind,
) -> Result<BuildOutcome> {
    build_for_testing_destination_with_output(
        project,
        platform,
        destination,
        BuildOutputMode::Silent,
    )
}

fn build_for_testing_destination_with_output(
    project: &ProjectContext,
    platform: ApplePlatform,
    destination: DestinationKind,
    output_mode: BuildOutputMode,
) -> Result<BuildOutcome> {
    let target = build_target_for_platform(project, platform)?;
    let profile = profile_for_run();
    let request = BuildRequest {
        target_name: target.name.clone(),
        platform,
        profile,
        destination,
        output: None,
        provisioning_udids: None,
    };
    if request_requires_signing(&request) {
        crate::apple::auth::ensure_project_authenticated(project)?;
    }
    build_project(project, &request, output_mode)
}

pub fn prepare_for_ide(
    project: &ProjectContext,
    platform: ApplePlatform,
    target_names: &[String],
    destination: DestinationKind,
    index_store_path: &Path,
) -> Result<()> {
    let platform_manifest = project
        .resolved_manifest
        .platforms
        .get(&platform)
        .context("platform configuration missing from manifest")?;
    let profile = ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development);
    let toolchain = Toolchain::resolve(
        platform,
        platform_manifest.deployment_target.as_str(),
        destination,
        project.selected_xcode.as_ref(),
    )?;
    let build_root = project
        .project_paths
        .build_dir
        .join(platform.to_string())
        .join("ide")
        .join(toolchain.destination.as_str());
    ensure_dir(&build_root)?;

    let ordered_targets = ide_prepare_targets(project, platform, target_names)?;
    for target in &ordered_targets {
        compile_target(
            project,
            &toolchain,
            target,
            &build_root,
            &profile,
            CompileOutputOptions {
                index_store_path: Some(index_store_path),
                mode: CompileOutputMode::Silent,
                log_prefix: None,
            },
        )?;
    }
    Ok(())
}

pub fn run_on_destination(project: &ProjectContext, args: &RunArgs) -> Result<()> {
    let platform = resolve_platform(
        project,
        args.platform.map(apple_platform_from_cli),
        "Select a platform to run",
    )?;
    let target = build_target_for_platform(project, platform)?;
    validate_run_platform(platform)?;
    let profile = profile_for_run();
    let destination = resolve_destination(
        project,
        platform,
        args.simulator,
        args.device,
        profile.distribution,
    )?;
    if args.trace.is_some() && args.debug {
        bail!("`orbit run --trace` does not support `--debug`");
    }
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
        profile,
        destination,
        output: None,
        provisioning_udids: selected_device
            .as_ref()
            .map(|device| vec![device.provisioning_udid().to_owned()]),
    };
    if request_requires_signing(&request) {
        crate::apple::auth::ensure_project_authenticated(project)?;
    }

    let outcome = build_project(project, &request, BuildOutputMode::UserFacing)?;
    crate::util::print_success(format!(
        "Built {} for {}.",
        outcome.receipt.target,
        profile_description(&request.profile)
    ));
    run_project_hooks(
        project,
        HookKind::BeforeRun,
        &HookContext {
            target_name: Some(outcome.receipt.target.as_str()),
            platform: Some(outcome.receipt.platform),
            distribution: Some(outcome.receipt.distribution),
            configuration: Some(outcome.receipt.configuration),
            destination: Some(outcome.receipt.destination.as_str()),
            bundle_path: Some(&outcome.receipt.bundle_path),
            artifact_path: Some(&outcome.receipt.artifact_path),
            receipt_path: Some(&outcome.receipt_path),
        },
    )?;
    match (
        outcome.receipt.platform,
        outcome.receipt.destination.as_str(),
        args.debug,
    ) {
        (ApplePlatform::Macos, _, false) => run_on_macos(project, &outcome.receipt, args.trace),
        (ApplePlatform::Macos, _, true) => debug_on_macos(project, &outcome.receipt),
        (_, "simulator", false) => run_on_simulator(project, &outcome.receipt, args.trace),
        (_, "simulator", true) => debug_on_simulator(project, &outcome.receipt),
        (_, "device", false) => run_on_device(
            project,
            selected_device
                .as_ref()
                .context("device run requested without a selected physical device")?,
            &outcome.receipt,
            args.trace,
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

fn ide_prepare_targets<'a>(
    project: &'a ProjectContext,
    platform: ApplePlatform,
    target_names: &[String],
) -> Result<Vec<&'a TargetManifest>> {
    let requested_names = if target_names.is_empty() {
        vec![
            project
                .resolved_manifest
                .default_build_target_for_platform(platform)?
                .name
                .clone(),
        ]
    } else {
        target_names.to_vec()
    };

    let mut ordered_targets = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for target_name in requested_names {
        let resolved_target = project
            .resolved_manifest
            .resolve_target(Some(&target_name))?;
        for target in project
            .resolved_manifest
            .topological_targets(resolved_target.name.as_str())?
        {
            if seen.insert(target.name.clone()) {
                ordered_targets.push(target);
            }
        }
    }
    Ok(ordered_targets)
}

fn build_project(
    project: &ProjectContext,
    request: &BuildRequest,
    output_mode: BuildOutputMode,
) -> Result<BuildOutcome> {
    let root_target = project
        .resolved_manifest
        .resolve_target(Some(&request.target_name))?;
    run_project_hooks(
        project,
        HookKind::BeforeBuild,
        &HookContext {
            target_name: Some(root_target.name.as_str()),
            platform: Some(request.platform),
            distribution: Some(request.profile.distribution),
            configuration: Some(request.profile.configuration),
            destination: Some(request.destination.as_str()),
            ..HookContext::default()
        },
    )?;
    let platform = request.platform;
    let platform_manifest = project
        .resolved_manifest
        .platforms
        .get(&platform)
        .context("platform configuration missing from manifest")?;
    let profile = &request.profile;

    let build_root = project
        .project_paths
        .build_dir
        .join(platform.to_string())
        .join(profile.variant_name())
        .join(request.destination.as_str());
    ensure_dir(&build_root)?;

    let ordered_targets = project
        .resolved_manifest
        .topological_targets(&root_target.name)?;
    let signing_required = request_requires_signing(request);
    let built_targets = if should_build_universal_macos(platform, platform_manifest) {
        build_universal_macos_target_graph(
            project,
            platform_manifest,
            request,
            &ordered_targets,
            &build_root,
            profile,
            output_mode,
        )?
    } else {
        let toolchain = Toolchain::resolve(
            platform,
            platform_manifest.deployment_target.as_str(),
            request.destination,
            project.selected_xcode.as_ref(),
        )?;
        compile_target_graph(
            project,
            platform,
            &toolchain,
            &ordered_targets,
            &build_root,
            profile,
            CompileOutputOptions {
                index_store_path: None,
                mode: compile_output_mode(output_mode),
                log_prefix: None,
            },
        )?
    };
    let signing_fingerprints = if signing_required {
        sign_target_graph(
            project,
            request,
            &ordered_targets,
            &built_targets,
            profile,
            platform,
        )?
    } else {
        HashMap::new()
    };

    let root_target_built = built_targets
        .get(&root_target.name)
        .context("root target did not build")?;
    let artifact_path = export_artifact(
        project,
        platform,
        root_target_built,
        request.output.as_deref(),
        profile,
        signing_fingerprints
            .get(&root_target.name)
            .map(String::as_str),
    )?;

    let mut receipt = BuildReceipt::new(BuildReceiptInput {
        target: root_target.name.clone(),
        platform,
        configuration: profile.configuration,
        distribution: profile.distribution,
        destination: request.destination.as_str().to_owned(),
        bundle_id: root_target.bundle_id.clone(),
        bundle_path: root_target_built.bundle_path.clone(),
        artifact_path,
    });
    if !matches!(root_target.kind, TargetKind::App | TargetKind::WatchApp) {
        receipt.submit_eligible = false;
    }
    let receipt_path = write_receipt(&project.project_paths.receipts_dir, &receipt)?;
    if signing_required {
        run_project_hooks(
            project,
            HookKind::AfterSign,
            &HookContext {
                target_name: Some(receipt.target.as_str()),
                platform: Some(receipt.platform),
                distribution: Some(receipt.distribution),
                configuration: Some(receipt.configuration),
                destination: Some(receipt.destination.as_str()),
                bundle_path: Some(&receipt.bundle_path),
                artifact_path: Some(&receipt.artifact_path),
                receipt_path: Some(&receipt_path),
            },
        )?;
    }

    Ok(BuildOutcome {
        receipt,
        receipt_path,
    })
}

fn should_build_universal_macos(
    platform: ApplePlatform,
    platform_manifest: &crate::manifest::PlatformManifest,
) -> bool {
    platform == ApplePlatform::Macos && platform_manifest.universal_binary
}

fn compile_target_graph(
    project: &ProjectContext,
    platform: ApplePlatform,
    toolchain: &Toolchain,
    ordered_targets: &[&TargetManifest],
    build_root: &Path,
    profile: &ProfileManifest,
    output: CompileOutputOptions<'_>,
) -> Result<HashMap<String, BuiltTarget>> {
    let mut built_targets = HashMap::new();
    for target in ordered_targets {
        let built = compile_target(project, toolchain, target, build_root, profile, output)?;
        built_targets.insert(target.name.clone(), built);
    }
    let bundle_content_fingerprints =
        compute_bundle_content_fingerprints(ordered_targets, &built_targets)?;

    for target in ordered_targets {
        if !target.kind.is_bundle() {
            continue;
        }
        let built_target = built_targets
            .get(&target.name)
            .with_context(|| format!("missing built target `{}`", target.name))?;
        embed_dependencies(
            project,
            platform,
            target,
            &built_targets,
            built_target,
            &bundle_content_fingerprints,
        )?;
    }

    Ok(built_targets)
}

fn sign_target_graph(
    project: &ProjectContext,
    request: &BuildRequest,
    ordered_targets: &[&TargetManifest],
    built_targets: &HashMap<String, BuiltTarget>,
    profile: &ProfileManifest,
    platform: ApplePlatform,
) -> Result<HashMap<String, String>> {
    let bundle_content_fingerprints =
        compute_bundle_content_fingerprints(ordered_targets, built_targets)?;
    let mut signing_fingerprints = HashMap::new();
    for target in ordered_targets {
        let built_target = built_targets
            .get(&target.name)
            .with_context(|| format!("missing built target `{}`", target.name))?;
        let bundle_content_fingerprint = bundle_content_fingerprints
            .get(&target.name)
            .cloned()
            .with_context(|| format!("missing bundle content fingerprint for `{}`", target.name))?;
        if !target.kind.is_bundle() {
            continue;
        }
        let material = crate::apple::signing::prepare_signing(
            project,
            target,
            platform,
            profile,
            request.provisioning_udids.clone(),
        )?;
        let signing_fingerprint = cache::compute_signing_fingerprint(
            platform,
            request.profile.distribution,
            target,
            built_target,
            &bundle_content_fingerprint,
            &material,
        )?;
        if !cache::cached_signed_bundle_can_be_reused(built_target, platform, &signing_fingerprint)?
        {
            crate::apple::signing::sign_bundle(
                platform,
                request.profile.distribution,
                &built_target.bundle_path,
                &material,
            )?;
            cache::write_signing_cache(&built_target.target_dir, &signing_fingerprint)?;
        }
        signing_fingerprints.insert(target.name.clone(), signing_fingerprint);
    }

    Ok(signing_fingerprints)
}

fn build_universal_macos_target_graph(
    project: &ProjectContext,
    platform_manifest: &crate::manifest::PlatformManifest,
    request: &BuildRequest,
    ordered_targets: &[&TargetManifest],
    build_root: &Path,
    profile: &ProfileManifest,
    output_mode: BuildOutputMode,
) -> Result<HashMap<String, BuiltTarget>> {
    let arch_root = build_root.join("arch");
    ensure_dir(&arch_root)?;

    let architectures = ["arm64", "x86_64"];
    let mut architecture_builds = Vec::with_capacity(architectures.len());
    for architecture in architectures {
        if matches!(output_mode, BuildOutputMode::UserFacing) {
            println!("universal macOS slice `{architecture}`:");
        }
        let toolchain = Toolchain::resolve_for_architecture(
            request.platform,
            platform_manifest.deployment_target.as_str(),
            request.destination,
            project.selected_xcode.as_ref(),
            Some(architecture),
        )?;
        let arch_build_root = arch_root.join(architecture);
        ensure_dir(&arch_build_root)?;
        let built_targets = compile_target_graph(
            project,
            request.platform,
            &toolchain,
            ordered_targets,
            &arch_build_root,
            profile,
            CompileOutputOptions {
                index_store_path: None,
                mode: compile_output_mode(output_mode),
                log_prefix: Some(architecture),
            },
        )?;
        if matches!(output_mode, BuildOutputMode::UserFacing) {
            crate::util::print_success(format!(
                "Built universal macOS slice `{architecture}` for {} target(s).",
                built_targets.len()
            ));
        }
        architecture_builds.push(ArchitectureBuild {
            toolchain,
            build_root: arch_build_root,
            built_targets,
        });
    }

    let primary = architecture_builds
        .first()
        .context("missing primary architecture build")?;
    let secondary = architecture_builds
        .get(1)
        .context("missing secondary architecture build")?;
    let merged_targets =
        merge_universal_macos_targets(project, ordered_targets, primary, secondary, build_root)?;
    if matches!(output_mode, BuildOutputMode::UserFacing) {
        crate::util::print_success(format!(
            "Merged universal macOS slices for {} target(s).",
            merged_targets.len()
        ));
    }
    Ok(merged_targets)
}

fn compile_output_mode(output_mode: BuildOutputMode) -> CompileOutputMode {
    match output_mode {
        BuildOutputMode::UserFacing => CompileOutputMode::UserFacing,
        BuildOutputMode::Silent => CompileOutputMode::Silent,
    }
}

fn merge_universal_macos_targets(
    project: &ProjectContext,
    ordered_targets: &[&TargetManifest],
    primary: &ArchitectureBuild,
    secondary: &ArchitectureBuild,
    build_root: &Path,
) -> Result<HashMap<String, BuiltTarget>> {
    let mut merged_targets = HashMap::new();

    for target in ordered_targets {
        let primary_target = primary.built_targets.get(&target.name).with_context(|| {
            format!("missing primary build output for target `{}`", target.name)
        })?;
        let secondary_target = secondary.built_targets.get(&target.name).with_context(|| {
            format!(
                "missing secondary build output for target `{}`",
                target.name
            )
        })?;
        let target_dir = build_root.join(&target.name);
        let intermediates_dir = target_dir.join("intermediates");
        let layout = product_layout(&target_dir, &intermediates_dir, target, &primary.toolchain);
        let merge_fingerprint =
            cache::compute_universal_merge_fingerprint(target, primary_target, secondary_target);
        let merged_module_outputs = expected_merged_arch_artifacts(
            &primary.build_root,
            primary_target.module_output_path.as_deref(),
            &secondary.build_root,
            secondary_target.module_output_path.as_deref(),
            build_root,
        )?;

        if !cache::cached_universal_merge_can_be_reused(
            &target_dir,
            &layout,
            &merged_module_outputs,
            &merge_fingerprint,
        )? {
            remove_existing_path(&layout.product_path)?;
            if primary_target.bundle_path.is_dir() {
                copy_dir_recursive(&primary_target.bundle_path, &layout.product_path)?;
            } else {
                copy_file(&primary_target.bundle_path, &layout.product_path)?;
            }

            ensure_parent_dir(&layout.binary_path)?;
            let mut lipo = primary.toolchain.lipo();
            lipo.args(["-create", "-output"]);
            lipo.arg(&layout.binary_path);
            lipo.arg(&primary_target.binary_path);
            lipo.arg(&secondary_target.binary_path);
            run_command(&mut lipo)?;

            if let Some(module_output_path) = &primary_target.module_output_path {
                copy_arch_artifact_to_merged_root(
                    &primary.build_root,
                    module_output_path,
                    build_root,
                )?;
            }
            if let Some(module_output_path) = &secondary_target.module_output_path {
                copy_arch_artifact_to_merged_root(
                    &secondary.build_root,
                    module_output_path,
                    build_root,
                )?;
            }
            cache::write_universal_merge_cache(&target_dir, &merge_fingerprint)?;
        }

        merged_targets.insert(
            target.name.clone(),
            BuiltTarget {
                target_name: target.name.clone(),
                target_kind: target.kind,
                target_dir,
                bundle_path: layout.product_path,
                binary_path: layout.binary_path,
                module_output_path: layout.module_output_path,
                code_phase_fingerprint: cache::combine_fingerprints(&[
                    primary_target.code_phase_fingerprint.as_str(),
                    secondary_target.code_phase_fingerprint.as_str(),
                ]),
                bundle_phase_fingerprint: cache::combine_fingerprints(&[
                    primary_target.bundle_phase_fingerprint.as_str(),
                    secondary_target.bundle_phase_fingerprint.as_str(),
                ]),
            },
        );
    }

    let merged_bundle_content_fingerprints =
        compute_bundle_content_fingerprints(ordered_targets, &merged_targets)?;

    for target in ordered_targets {
        if !target.kind.is_bundle() {
            continue;
        }
        let built_target = merged_targets
            .get(&target.name)
            .with_context(|| format!("missing merged target `{}`", target.name))?;
        embed_dependencies(
            project,
            ApplePlatform::Macos,
            target,
            &merged_targets,
            built_target,
            &merged_bundle_content_fingerprints,
        )?;
    }

    Ok(merged_targets)
}

fn compute_bundle_content_fingerprints(
    ordered_targets: &[&TargetManifest],
    built_targets: &HashMap<String, BuiltTarget>,
) -> Result<HashMap<String, String>> {
    let mut bundle_content_fingerprints = HashMap::new();
    for target in ordered_targets {
        let built_target = built_targets
            .get(&target.name)
            .with_context(|| format!("missing built target `{}`", target.name))?;
        let dependency_fingerprints = target
            .dependencies
            .iter()
            .map(|dependency_name| {
                bundle_content_fingerprints
                    .get(dependency_name)
                    .cloned()
                    .with_context(|| {
                        format!(
                            "missing bundle content fingerprint for dependency `{dependency_name}`"
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        let bundle_content_fingerprint = cache::compute_bundle_content_fingerprint(
            target,
            built_target,
            &dependency_fingerprints,
        )?;
        bundle_content_fingerprints.insert(target.name.clone(), bundle_content_fingerprint);
    }
    Ok(bundle_content_fingerprints)
}

fn copy_arch_artifact_to_merged_root(
    architecture_root: &Path,
    source: &Path,
    merged_root: &Path,
) -> Result<()> {
    let destination = merged_arch_artifact_path(architecture_root, source, merged_root)?;
    if source.is_dir() {
        copy_dir_recursive(source, &destination)?;
    } else {
        copy_file(source, &destination)?;
    }
    Ok(())
}

fn expected_merged_arch_artifacts(
    primary_build_root: &Path,
    primary_output: Option<&Path>,
    secondary_build_root: &Path,
    secondary_output: Option<&Path>,
    merged_root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut outputs = Vec::new();
    if let Some(output) = primary_output {
        outputs.push(merged_arch_artifact_path(
            primary_build_root,
            output,
            merged_root,
        )?);
    }
    if let Some(output) = secondary_output {
        outputs.push(merged_arch_artifact_path(
            secondary_build_root,
            output,
            merged_root,
        )?);
    }
    outputs.sort();
    outputs.dedup();
    Ok(outputs)
}

fn merged_arch_artifact_path(
    architecture_root: &Path,
    source: &Path,
    merged_root: &Path,
) -> Result<PathBuf> {
    let relative = source.strip_prefix(architecture_root).with_context(|| {
        format!(
            "failed to relativize architecture artifact {} against {}",
            source.display(),
            architecture_root.display()
        )
    })?;
    Ok(merged_root.join(relative))
}

fn resolve_destination(
    project: &ProjectContext,
    platform: ApplePlatform,
    simulator: bool,
    device: bool,
    distribution: DistributionKind,
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
    if matches!(distribution, DistributionKind::Development) && project.app.interactive {
        let options = ["Simulator", "Physical device"];
        let index = prompt_select("Select a destination", &options)?;
        return Ok(match index {
            0 => DestinationKind::Simulator,
            _ => DestinationKind::Device,
        });
    }
    Ok(default_destination_for_distribution(platform, distribution))
}

fn default_destination_for_distribution(
    platform: ApplePlatform,
    distribution: DistributionKind,
) -> DestinationKind {
    if platform == ApplePlatform::Macos {
        return DestinationKind::Device;
    }

    match distribution {
        DistributionKind::Development => DestinationKind::Simulator,
        DistributionKind::AdHoc
        | DistributionKind::AppStore
        | DistributionKind::DeveloperId
        | DistributionKind::MacAppStore => DestinationKind::Device,
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

fn macos_bundle_uses_contents(platform: ApplePlatform, target_kind: TargetKind) -> bool {
    platform == ApplePlatform::Macos
        && matches!(
            target_kind,
            TargetKind::App
                | TargetKind::AppExtension
                | TargetKind::WatchApp
                | TargetKind::WatchExtension
                | TargetKind::WidgetExtension
        )
}

fn bundle_metadata_root(
    toolchain: &Toolchain,
    target_kind: TargetKind,
    bundle_root: &Path,
) -> PathBuf {
    if macos_bundle_uses_contents(toolchain.platform, target_kind) {
        bundle_root.join("Contents")
    } else {
        bundle_root.to_path_buf()
    }
}

fn bundle_resources_root(
    toolchain: &Toolchain,
    target_kind: TargetKind,
    bundle_root: &Path,
) -> PathBuf {
    if macos_bundle_uses_contents(toolchain.platform, target_kind) {
        bundle_root.join("Contents").join("Resources")
    } else {
        bundle_root.to_path_buf()
    }
}

fn bundle_frameworks_root(
    toolchain: &Toolchain,
    target_kind: TargetKind,
    bundle_root: &Path,
) -> PathBuf {
    if macos_bundle_uses_contents(toolchain.platform, target_kind) {
        bundle_root.join("Contents").join("Frameworks")
    } else {
        bundle_root.join("Frameworks")
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
            if macos_bundle_uses_contents(toolchain.platform, target.kind) =>
        {
            product_path
                .join("Contents")
                .join("MacOS")
                .join(&target.name)
        }
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

fn embedded_dependency_root(
    project: &ProjectContext,
    platform: ApplePlatform,
    parent_target: &TargetManifest,
    child_target: &TargetManifest,
) -> Result<Option<PathBuf>> {
    let relative = match (parent_target.kind, child_target.kind) {
        (
            TargetKind::App | TargetKind::WatchApp,
            TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension,
        ) => Some(PathBuf::from("PlugIns")),
        (TargetKind::App, TargetKind::WatchApp) => Some(PathBuf::from("Watch")),
        (TargetKind::App, TargetKind::App)
            if crate::apple::signing::target_is_app_clip(project, child_target)? =>
        {
            Some(PathBuf::from("AppClips"))
        }
        (
            TargetKind::App
            | TargetKind::AppExtension
            | TargetKind::WatchApp
            | TargetKind::WatchExtension
            | TargetKind::WidgetExtension,
            TargetKind::Framework,
        ) => Some(PathBuf::from("Frameworks")),
        _ => None,
    };
    Ok(relative.map(|path| {
        if macos_bundle_uses_contents(platform, parent_target.kind) {
            PathBuf::from("Contents").join(path)
        } else {
            path
        }
    }))
}
