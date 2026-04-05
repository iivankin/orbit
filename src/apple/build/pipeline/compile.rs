use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::artifacts::remove_existing_path;
use super::cache::{
    cached_bundle_phase_can_be_reused, cached_code_phase_can_be_reused,
    cached_embedded_dependencies_can_be_reused, clear_embedded_dependency_outputs,
    compute_bundle_phase_fingerprint, compute_code_phase_fingerprint,
    compute_embedded_dependency_fingerprint, print_target_build_cache_hit,
    write_bundle_phase_cache, write_code_phase_cache, write_embedded_dependency_cache,
};
use super::info_plist::{needs_info_plist, write_info_plist};
use super::resources::{ResourceWorkSummary, process_resources, should_process_resources};
use super::{
    BuiltTarget, build_progress_step, bundle_frameworks_root, embedded_dependency_root,
    product_layout,
};
use crate::apple::build::clang::{
    ClangCompilePlan, ClangCompileSummary, ClangSourceLanguage, cached_object_can_be_reused,
    object_depfile_path, object_file_name, target_clang_invocation, write_object_cache,
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
use tempfile::tempdir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompileOutputMode {
    UserFacing,
    Silent,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CompileOutputOptions<'a> {
    pub(super) index_store_path: Option<&'a Path>,
    pub(super) mode: CompileOutputMode,
    pub(super) log_prefix: Option<&'a str>,
}

pub(super) fn compile_target(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    build_root: &Path,
    profile: &ProfileManifest,
    output: CompileOutputOptions<'_>,
) -> Result<BuiltTarget> {
    let target_dir = build_root.join(&target.name);
    let intermediates_dir = target_dir.join("intermediates");
    let product = product_layout(&target_dir, &intermediates_dir, target, toolchain);

    let package_outputs = if target.swift_packages.is_empty() {
        Vec::new()
    } else {
        run_compile_step(
            output.mode,
            output.log_prefix,
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
                    output.index_store_path,
                    target,
                )
            },
        )?
    };
    let external_link_inputs =
        resolve_external_link_inputs(project, toolchain, &intermediates_dir, target)?;
    let code_fingerprint = compute_code_phase_fingerprint(
        project,
        toolchain,
        profile,
        target,
        output.index_store_path,
        &package_outputs,
        &external_link_inputs,
    )?;
    let bundle_fingerprint =
        compute_bundle_phase_fingerprint(project, toolchain, target, &external_link_inputs)?;

    let code_cache_hit = cached_code_phase_can_be_reused(&target_dir, &product, &code_fingerprint)?;
    let bundle_cache_hit = if target.kind.is_bundle() && code_cache_hit {
        cached_bundle_phase_can_be_reused(
            &target_dir,
            target,
            toolchain,
            &product,
            &external_link_inputs,
            &bundle_fingerprint,
        )?
    } else {
        false
    };

    if code_cache_hit && (bundle_cache_hit || !target.kind.is_bundle()) {
        if matches!(output.mode, CompileOutputMode::UserFacing) {
            print_target_build_cache_hit(&target.name, output.log_prefix);
        }
        return Ok(BuiltTarget {
            target_name: target.name.clone(),
            target_kind: target.kind,
            target_dir: target_dir.clone(),
            bundle_path: product.product_path,
            binary_path: product.binary_path,
            module_output_path: product.module_output_path,
            code_phase_fingerprint: code_fingerprint,
            bundle_phase_fingerprint: bundle_fingerprint,
        });
    }

    if code_cache_hit {
        rebuild_bundle_layout_preserving_code_outputs(&product)?;
    } else {
        ensure_dir(&intermediates_dir)?;
        remove_existing_path(&product.product_path)?;
        if target.kind.is_bundle() {
            ensure_dir(&product.product_path)?;
        } else {
            ensure_parent_dir(&product.product_path)?;
        }
        ensure_parent_dir(&product.binary_path)?;

        let c_family_summary = run_compile_step(
            output.mode,
            output.log_prefix,
            format!("Compiling C-family sources for target `{}`", target.name),
            |summary: &ClangCompileSummary| {
                if summary.object_files.is_empty() {
                    format!(
                        "No C-family sources were compiled for target `{}`.",
                        target.name
                    )
                } else if summary.reused_count == 0 {
                    format!(
                        "Compiled {} C-family object file(s) for target `{}`.",
                        summary.compiled_count, target.name
                    )
                } else if summary.compiled_count == 0 {
                    format!(
                        "Reused {} cached C-family object file(s) for target `{}`.",
                        summary.reused_count, target.name
                    )
                } else {
                    format!(
                        "Compiled {} and reused {} cached C-family object file(s) for target `{}`.",
                        summary.compiled_count, summary.reused_count, target.name
                    )
                }
            },
            || {
                compile_c_family_sources(
                    project,
                    toolchain,
                    profile,
                    &intermediates_dir,
                    output.index_store_path,
                    &external_link_inputs,
                    target,
                )
            },
        )?;
        let c_objects = c_family_summary.object_files;
        let swift_sources = resolve_target_sources(project, target, &["swift"])?;

        if !swift_sources.is_empty() {
            run_compile_step(
                output.mode,
                output.log_prefix,
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
                            index_store_path: output.index_store_path,
                        },
                    )
                },
            )?;
        } else if !c_objects.is_empty() {
            run_compile_step(
                output.mode,
                output.log_prefix,
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
            relocate_bundle_debug_artifacts(
                &target_dir,
                &product.product_path,
                &product.binary_path,
            )?;
        }
        write_code_phase_cache(&target_dir, &code_fingerprint)?;
    }

    if !target.kind.is_bundle() {
        return Ok(BuiltTarget {
            target_name: target.name.clone(),
            target_kind: target.kind,
            target_dir: target_dir.clone(),
            bundle_path: product.product_path,
            binary_path: product.binary_path,
            module_output_path: product.module_output_path,
            code_phase_fingerprint: code_fingerprint,
            bundle_phase_fingerprint: bundle_fingerprint,
        });
    }

    if needs_info_plist(target.kind) {
        run_compile_step(
            output.mode,
            output.log_prefix,
            format!("Writing Info.plist for target `{}`", target.name),
            |_| format!("Wrote Info.plist for target `{}`.", target.name),
            || write_info_plist(project, toolchain, target, &product.product_path),
        )?;
    }
    if target.kind.is_bundle() {
        if should_process_resources(target) {
            run_compile_step(
                output.mode,
                output.log_prefix,
                format!("Processing resources for target `{}`", target.name),
                |summary: &ResourceWorkSummary| {
                    format!(
                        "Processed resources for target `{}`: {}.",
                        target.name,
                        summary.describe()
                    )
                },
                || {
                    process_resources(
                        project,
                        toolchain,
                        target,
                        &product.product_path,
                        &target_dir,
                    )
                },
            )?;
        }
        if !external_link_inputs.embedded_payloads.is_empty() {
            run_compile_step(
                output.mode,
                output.log_prefix,
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
    write_bundle_phase_cache(&target_dir, &bundle_fingerprint)?;

    Ok(BuiltTarget {
        target_name: target.name.clone(),
        target_kind: target.kind,
        target_dir,
        bundle_path: product.product_path,
        binary_path: product.binary_path,
        module_output_path: product.module_output_path,
        code_phase_fingerprint: code_fingerprint,
        bundle_phase_fingerprint: bundle_fingerprint,
    })
}

fn rebuild_bundle_layout_preserving_code_outputs(product: &super::ProductLayout) -> Result<()> {
    let preserved_root = tempdir().context("failed to create bundle cache preservation tempdir")?;
    let bundle_root = &product.product_path;
    let mut preserved_paths = vec![product.binary_path.clone()];
    if let Some(module_output_path) = &product.module_output_path
        && module_output_path.starts_with(bundle_root)
    {
        preserved_paths.push(module_output_path.clone());
    }

    for path in &preserved_paths {
        let relative = path
            .strip_prefix(bundle_root)
            .with_context(|| format!("failed to relativize preserved path {}", path.display()))?;
        let destination = preserved_root.path().join(relative);
        copy_file(path, &destination)?;
    }

    remove_existing_path(bundle_root)?;
    ensure_dir(bundle_root)?;
    for path in preserved_paths {
        let relative = path
            .strip_prefix(bundle_root)
            .with_context(|| format!("failed to relativize preserved path {}", path.display()))?;
        let source = preserved_root.path().join(relative);
        copy_file(&source, &path)?;
    }
    Ok(())
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

pub(super) fn prefixed_compile_message(prefix: Option<&str>, message: String) -> String {
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
    built_root_target: &BuiltTarget,
    bundle_content_fingerprints: &std::collections::HashMap<String, String>,
) -> Result<()> {
    let planned_embeddings = planned_embedded_dependencies(
        project,
        platform,
        root_target,
        built_targets,
        built_root_target,
        bundle_content_fingerprints,
    )?;
    let outputs = planned_embeddings
        .iter()
        .map(|embedding| embedding.relative_output.clone())
        .collect::<Vec<_>>();
    let dependency_fingerprints = planned_embeddings
        .iter()
        .map(|embedding| embedding.dependency_fingerprint.clone())
        .collect::<Vec<_>>();
    let embedding_fingerprint = compute_embedded_dependency_fingerprint(
        platform,
        root_target,
        built_root_target,
        &outputs,
        &dependency_fingerprints,
    );
    if cached_embedded_dependencies_can_be_reused(
        &built_root_target.target_dir,
        &built_root_target.bundle_path,
        &outputs,
        &embedding_fingerprint,
    )? {
        return Ok(());
    }

    clear_embedded_dependency_outputs(
        &built_root_target.target_dir,
        &built_root_target.bundle_path,
        &outputs,
    )?;
    for embedding in planned_embeddings {
        if embedding.source_path.is_dir() {
            copy_dir_recursive(&embedding.source_path, &embedding.destination)?;
        } else {
            copy_file(&embedding.source_path, &embedding.destination)?;
        }
    }
    write_embedded_dependency_cache(
        &built_root_target.target_dir,
        &embedding_fingerprint,
        &outputs,
    )?;
    Ok(())
}

struct PlannedEmbeddedDependency {
    source_path: PathBuf,
    destination: PathBuf,
    relative_output: PathBuf,
    dependency_fingerprint: String,
}

fn planned_embedded_dependencies(
    project: &ProjectContext,
    platform: ApplePlatform,
    root_target: &TargetManifest,
    built_targets: &std::collections::HashMap<String, BuiltTarget>,
    built_root_target: &BuiltTarget,
    bundle_content_fingerprints: &std::collections::HashMap<String, String>,
) -> Result<Vec<PlannedEmbeddedDependency>> {
    let mut planned = Vec::new();
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
        let relative_output = destination
            .strip_prefix(&built_root_target.bundle_path)
            .with_context(|| {
                format!(
                    "failed to relativize embedded dependency output {}",
                    destination.display()
                )
            })?
            .to_path_buf();
        planned.push(PlannedEmbeddedDependency {
            source_path: built.bundle_path.clone(),
            destination,
            relative_output,
            dependency_fingerprint: bundle_content_fingerprints
                .get(dependency_name)
                .cloned()
                .with_context(|| {
                    format!("missing bundle content fingerprint for dependency `{dependency_name}`")
                })?,
        });
    }
    Ok(planned)
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
) -> Result<ClangCompileSummary> {
    let mut summary = ClangCompileSummary::default();
    for extension in ["c", "m", "mm", "cpp", "cc", "cxx"] {
        for source in resolve_target_sources(project, target, &[extension])? {
            let language = ClangSourceLanguage::from_extension(extension)
                .with_context(|| format!("unsupported C-family extension `{extension}`"))?;
            let object_name = object_file_name(&source)?;
            let object_path = intermediates_dir.join(object_name);
            let depfile_path = object_depfile_path(&object_path);
            let invocation = target_clang_invocation(
                toolchain,
                profile,
                ClangCompilePlan {
                    source_file: &source,
                    output_path: &object_path,
                    depfile_path: Some(depfile_path.as_path()),
                    language,
                    external_link_inputs,
                    index_store_path,
                },
            )?;
            if cached_object_can_be_reused(toolchain, &invocation)? {
                summary.reused_count += 1;
            } else {
                let mut command = invocation.command(toolchain);
                run_command(&mut command)?;
                write_object_cache(toolchain, &invocation)?;
                summary.compiled_count += 1;
            }
            summary.object_files.push(invocation.output_path);
        }
    }

    Ok(summary)
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
