use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::artifacts::remove_existing_path;
use super::info_plist::{needs_info_plist, write_info_plist};
use super::resources::{ResourceWorkSummary, process_resources, should_process_resources};
use super::{
    BuiltTarget, build_progress_step, bundle_frameworks_root, embedded_dependency_root,
    product_layout,
};
use crate::apple::build::clang::{
    ClangCompilePlan, ClangSourceLanguage, object_file_name, target_clang_invocation,
};
use crate::apple::build::external::{
    ExternalLinkInputs, PackageBuildOutput, apply_external_link_inputs, compile_swift_package,
    resolve_external_link_inputs,
};
use crate::apple::build::swiftc::{SwiftTargetCompilePlan, target_swiftc_invocation};
use crate::apple::build::toolchain::Toolchain;
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, ProfileManifest, TargetKind, TargetManifest};
use crate::util::{
    collect_files_with_extensions, copy_dir_recursive, copy_file, ensure_dir, ensure_parent_dir,
    resolve_path, run_command,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompileOutputMode {
    UserFacing,
    Silent,
}

pub(super) fn compile_target(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    build_root: &Path,
    profile: &ProfileManifest,
    index_store_path: Option<&Path>,
    output_mode: CompileOutputMode,
    log_prefix: Option<&str>,
) -> Result<BuiltTarget> {
    let target_dir = build_root.join(&target.name);
    let intermediates_dir = target_dir.join("intermediates");
    let product = product_layout(&target_dir, &intermediates_dir, target, toolchain);
    ensure_dir(&intermediates_dir)?;
    remove_existing_path(&product.product_path)?;
    if target.kind.is_bundle() {
        ensure_dir(&product.product_path)?;
    } else {
        ensure_parent_dir(&product.product_path)?;
    }
    ensure_parent_dir(&product.binary_path)?;

    let package_outputs = if target.swift_packages.is_empty() {
        Vec::new()
    } else {
        run_compile_step(
            output_mode,
            log_prefix,
            format!("Compiling Swift packages for target `{}`", target.name),
            |outputs: &Vec<PackageBuildOutput>| {
                format!(
                    "Compiled {} Swift package product(s) for target `{}`.",
                    outputs.len(),
                    target.name
                )
            },
            || {
                compile_swift_packages(
                    project,
                    toolchain,
                    profile,
                    &intermediates_dir,
                    index_store_path,
                    target,
                )
            },
        )?
    };
    let external_link_inputs =
        resolve_external_link_inputs(project, toolchain, &intermediates_dir, target)?;
    let c_objects = run_compile_step(
        output_mode,
        log_prefix,
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
        || {
            compile_c_family_sources(
                project,
                toolchain,
                profile,
                &intermediates_dir,
                index_store_path,
                &external_link_inputs,
                target,
            )
        },
    )?;
    let swift_sources = resolve_target_sources(project, target, &["swift"])?;

    if !swift_sources.is_empty() {
        run_compile_step(
            output_mode,
            log_prefix,
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
                    SwiftTargetCompilePlan {
                        target_kind: target.kind,
                        module_name: &target.name,
                        product_path: &product.binary_path,
                        module_output_path: product.module_output_path.as_deref(),
                        swift_sources: &swift_sources,
                        package_outputs: &package_outputs,
                        external_link_inputs: &external_link_inputs,
                        object_files: &c_objects,
                        index_store_path,
                    },
                )
            },
        )?;
    } else if !c_objects.is_empty() {
        run_compile_step(
            output_mode,
            log_prefix,
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
        run_compile_step(
            output_mode,
            log_prefix,
            format!("Writing Info.plist for target `{}`", target.name),
            |_| format!("Wrote Info.plist for target `{}`.", target.name),
            || write_info_plist(project, toolchain, target, &product.product_path),
        )?;
    }
    if target.kind.is_bundle() {
        if should_process_resources(toolchain.platform, target) {
            run_compile_step(
                output_mode,
                log_prefix,
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
            run_compile_step(
                output_mode,
                log_prefix,
                format!("Embedding external payloads for target `{}`", target.name),
                |_| {
                    format!(
                        "Embedded {} external payload(s) for target `{}`.",
                        external_link_inputs.embedded_payloads.len(),
                        target.name
                    )
                },
                || {
                    embed_external_payloads(
                        &external_link_inputs,
                        toolchain,
                        target.kind,
                        &product.product_path,
                    )
                },
            )?;
        }
    }

    Ok(BuiltTarget {
        target_name: target.name.clone(),
        target_kind: target.kind,
        bundle_path: product.product_path,
        binary_path: product.binary_path,
        module_output_path: product.module_output_path,
    })
}

fn run_compile_step<T, F, G>(
    output_mode: CompileOutputMode,
    log_prefix: Option<&str>,
    message: impl Into<String>,
    success_message: G,
    action: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
    G: FnOnce(&T) -> String,
{
    let message = prefixed_compile_message(log_prefix, message.into());
    match output_mode {
        CompileOutputMode::UserFacing => build_progress_step(
            message,
            |value| prefixed_compile_message(log_prefix, success_message(value)),
            action,
        ),
        CompileOutputMode::Silent => action(),
    }
}

fn prefixed_compile_message(prefix: Option<&str>, message: String) -> String {
    match prefix {
        Some(prefix) => format!("[{prefix}] {message}"),
        None => message,
    }
}

pub(super) fn embed_dependencies(
    project: &ProjectContext,
    platform: ApplePlatform,
    root_target: &TargetManifest,
    built_targets: &std::collections::HashMap<String, BuiltTarget>,
    built_root_target: &mut BuiltTarget,
) -> Result<()> {
    for dependency_name in &root_target.dependencies {
        let dependency_target = project
            .resolved_manifest
            .resolve_target(Some(dependency_name))?;
        let built = built_targets
            .get(dependency_name)
            .with_context(|| format!("missing built dependency `{dependency_name}`"))?;
        let Some(destination_root) =
            embedded_dependency_root(project, platform, root_target, dependency_target)?
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

pub(super) fn relocate_bundle_debug_artifacts(
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

fn resolve_target_sources(
    project: &ProjectContext,
    target: &TargetManifest,
    extensions: &[&str],
) -> Result<Vec<PathBuf>> {
    let mut sources = Vec::new();
    for root in project.resolved_manifest.shared_source_roots() {
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
    index_store_path: Option<&Path>,
    external_link_inputs: &ExternalLinkInputs,
    target: &TargetManifest,
) -> Result<Vec<PathBuf>> {
    let mut object_files = Vec::new();
    for extension in ["c", "m", "mm", "cpp", "cc", "cxx"] {
        for source in resolve_target_sources(project, target, &[extension])? {
            let language = ClangSourceLanguage::from_extension(extension)
                .with_context(|| format!("unsupported C-family extension `{extension}`"))?;
            let object_name = object_file_name(&source)?;
            let object_path = intermediates_dir.join(object_name);
            let invocation = target_clang_invocation(
                toolchain,
                profile,
                ClangCompilePlan {
                    source_file: &source,
                    output_path: &object_path,
                    language,
                    external_link_inputs,
                    index_store_path,
                },
            )?;
            let mut command = invocation.command(toolchain);
            run_command(&mut command)?;
            object_files.push(invocation.output_path);
        }
    }

    Ok(object_files)
}

fn compile_swift_target(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    plan: SwiftTargetCompilePlan<'_>,
) -> Result<()> {
    let module_name = plan.module_name.to_owned();
    let invocation = target_swiftc_invocation(toolchain, profile, plan)?;
    let source_count = invocation.source_files.len();
    let mut command = invocation.command(toolchain);
    run_command(&mut command).with_context(|| {
        format!("failed to compile Swift target `{module_name}` from {source_count} source file(s)")
    })
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
            if profile.is_debug() {
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
            if profile.is_debug() {
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

fn compile_swift_packages(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    index_store_path: Option<&Path>,
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
            || {
                compile_swift_package(
                    project,
                    toolchain,
                    profile,
                    intermediates_dir,
                    index_store_path,
                    dependency,
                )
            },
        )?);
    }

    Ok(outputs)
}

fn embed_external_payloads(
    inputs: &ExternalLinkInputs,
    toolchain: &Toolchain,
    target_kind: TargetKind,
    bundle_root: &Path,
) -> Result<()> {
    if inputs.embedded_payloads.is_empty() {
        return Ok(());
    }

    let frameworks_root = bundle_frameworks_root(toolchain, target_kind, bundle_root);
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
