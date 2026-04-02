use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::apple::build::clang::{
    ClangCompilePlan, ClangSourceLanguage, object_file_name, target_clang_invocation,
};
use crate::apple::build::external::{
    PackageBuildOutput, compile_swift_package, resolve_external_link_inputs,
};
use crate::apple::build::swiftc::{SwiftTargetCompilePlan, target_swiftc_invocation};
use crate::apple::build::toolchain::{DestinationKind, Toolchain};
use crate::apple::xcode::resolve_requested_xcode;
use crate::context::{AppContext, ProjectContext, ProjectPaths};
use crate::manifest::{
    ApplePlatform, BuildConfiguration, DistributionKind, ManifestSchema, ProfileManifest,
    ResolvedManifest, TargetKind, TargetManifest, detect_schema,
};
use crate::util::{collect_files_with_extensions, ensure_dir, resolve_path};

pub(crate) const C_FAMILY_SOURCE_EXTENSIONS: &[&str] = &["c", "m", "mm", "cpp", "cc", "cxx"];
pub(crate) const C_FAMILY_HEADER_EXTENSIONS: &[&str] = &["h", "hh", "hpp", "hxx"];

pub(crate) struct AnalysisProject {
    pub(crate) project: ProjectContext,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SemanticCompilationArtifact {
    pub platforms: Vec<String>,
    pub index_store_path: PathBuf,
    pub index_database_path: PathBuf,
    pub invocations: Vec<SemanticCompilerInvocation>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SemanticCompilerInvocation {
    pub platform: String,
    pub destination: String,
    pub language: String,
    pub sdk_name: String,
    pub target: String,
    pub module_name: String,
    pub target_triple: String,
    pub working_directory: PathBuf,
    pub toolchain_root: PathBuf,
    pub arguments: Vec<String>,
    pub source_files: Vec<PathBuf>,
    pub output_path: Option<String>,
}

pub(crate) fn load_persistent_analysis_project(
    app: &AppContext,
    requested_manifest: Option<&Path>,
) -> Result<AnalysisProject> {
    let (manifest_path, manifest_schema) = resolve_analysis_manifest(app, requested_manifest)?;
    let root = manifest_path
        .parent()
        .context("manifest path did not contain a parent directory")?
        .to_path_buf();
    let orbit_dir = root.join(".orbit").join("ide");
    load_analysis_project_with_orbit_dir(app, manifest_path, manifest_schema, orbit_dir)
}

pub(crate) fn load_cached_analysis_project(
    app: &AppContext,
    requested_manifest: Option<&Path>,
) -> Result<AnalysisProject> {
    let (manifest_path, manifest_schema) = resolve_analysis_manifest(app, requested_manifest)?;
    let orbit_dir = cached_analysis_orbit_dir(app, &manifest_path);
    load_analysis_project_with_orbit_dir(app, manifest_path, manifest_schema, orbit_dir)
}

fn load_analysis_project_with_orbit_dir(
    app: &AppContext,
    manifest_path: PathBuf,
    manifest_schema: ManifestSchema,
    orbit_dir: PathBuf,
) -> Result<AnalysisProject> {
    let root = manifest_path
        .parent()
        .context("manifest path did not contain a parent directory")?
        .to_path_buf();
    let build_dir = orbit_dir.join("build");
    let artifacts_dir = orbit_dir.join("artifacts");
    let receipts_dir = orbit_dir.join("receipts");

    // Reuse Orbit's build graph without polluting the project's checked-in `.orbit` state.
    ensure_dir(&orbit_dir)?;
    ensure_dir(&build_dir)?;
    ensure_dir(&artifacts_dir)?;
    ensure_dir(&receipts_dir)?;

    let resolved_manifest = ResolvedManifest::load(&manifest_path, &orbit_dir)?;
    let selected_xcode = resolve_requested_xcode(resolved_manifest.xcode.as_deref())?;
    let project = ProjectContext {
        app: app.clone(),
        root,
        manifest_path,
        manifest_schema,
        resolved_manifest,
        selected_xcode,
        project_paths: ProjectPaths {
            orbit_dir,
            build_dir,
            artifacts_dir,
            receipts_dir,
        },
    };

    Ok(AnalysisProject { project })
}

fn resolve_analysis_manifest(
    app: &AppContext,
    requested_manifest: Option<&Path>,
) -> Result<(PathBuf, ManifestSchema)> {
    let manifest_path = app.resolve_manifest_path_for_dispatch(requested_manifest)?;
    let manifest_path = manifest_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", manifest_path.display()))?;
    let manifest_schema = detect_schema(&manifest_path)?;
    Ok((manifest_path, manifest_schema))
}

fn cached_analysis_orbit_dir(app: &AppContext, manifest_path: &Path) -> PathBuf {
    let manifest_key = short_hash(manifest_path.to_string_lossy().as_ref());
    app.global_paths
        .cache_dir
        .join("analysis")
        .join(manifest_key)
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn build_semantic_compilation_artifact<F>(
    project: &ProjectContext,
    explicit_platform: Option<ApplePlatform>,
    include_source: &F,
) -> Result<SemanticCompilationArtifact>
where
    F: Fn(&Path) -> bool,
{
    let platforms = semantic_compilation_platforms(project, explicit_platform)?;
    let profile = ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development);
    let index_root = project.project_paths.orbit_dir.join("index");
    let index_store_path = index_root.join("store");
    let index_database_path = index_root.join("db");
    ensure_dir(&index_store_path)?;
    ensure_dir(&index_database_path)?;
    let mut compiler_invocations = Vec::new();

    for platform in &platforms {
        let platform_manifest = project
            .resolved_manifest
            .platforms
            .get(platform)
            .context("platform configuration missing from manifest")?;
        let toolchain = Toolchain::resolve(
            *platform,
            &platform_manifest.deployment_target,
            analysis_destination_for_platform(*platform),
            project.selected_xcode.as_ref(),
        )?;
        let build_root = project
            .project_paths
            .build_dir
            .join(platform.to_string())
            .join("analysis")
            .join(toolchain.destination.as_str());
        ensure_dir(&build_root)?;

        let root_target = project
            .resolved_manifest
            .default_build_target_for_platform(*platform)?;
        let ordered_targets = project
            .resolved_manifest
            .topological_targets(&root_target.name)?;
        for target in ordered_targets
            .into_iter()
            .filter(|target| target.supports_platform(*platform))
        {
            compiler_invocations.extend(semantic_target_compiler_invocations(
                project,
                &toolchain,
                &profile,
                &build_root,
                &index_store_path,
                target,
                include_source,
            )?);
        }
    }

    if compiler_invocations.is_empty() {
        bail!("semantic analysis did not resolve any compilation commands");
    }

    Ok(SemanticCompilationArtifact {
        platforms: platforms
            .into_iter()
            .map(|platform| platform.to_string())
            .collect(),
        index_store_path,
        index_database_path,
        invocations: compiler_invocations,
    })
}

pub(crate) fn collect_project_swift_files<F>(
    project: &ProjectContext,
    include_source: &F,
) -> Result<Vec<PathBuf>>
where
    F: Fn(&Path) -> bool,
{
    let mut files = BTreeSet::new();
    for root in source_roots(project) {
        collect_files_under_root(&root, &["swift"], include_source, &mut files)?;
    }
    Ok(files.into_iter().collect())
}

pub(crate) fn collect_target_header_files<F>(
    project: &ProjectContext,
    target: &TargetManifest,
    include_source: &F,
) -> Result<Vec<PathBuf>>
where
    F: Fn(&Path) -> bool,
{
    collect_target_files_with_extensions(
        project,
        target,
        C_FAMILY_HEADER_EXTENSIONS,
        include_source,
    )
}

fn semantic_compilation_platforms(
    project: &ProjectContext,
    explicit_platform: Option<ApplePlatform>,
) -> Result<Vec<ApplePlatform>> {
    if let Some(platform) = explicit_platform {
        if !project.resolved_manifest.platforms.contains_key(&platform) {
            bail!("platform `{platform}` is not declared in the manifest");
        }
        return Ok(vec![platform]);
    }

    Ok(project
        .resolved_manifest
        .platforms
        .keys()
        .copied()
        .collect())
}

fn semantic_target_compiler_invocations<F>(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    build_root: &Path,
    index_store_path: &Path,
    target: &TargetManifest,
    include_source: &F,
) -> Result<Vec<SemanticCompilerInvocation>>
where
    F: Fn(&Path) -> bool,
{
    let target_dir = build_root.join(&target.name);
    let intermediates_dir = target_dir.join("intermediates");
    ensure_dir(&intermediates_dir)?;

    let package_outputs = compile_analysis_swift_packages(
        project,
        toolchain,
        profile,
        &intermediates_dir,
        index_store_path,
        target,
    )?;
    let external_link_inputs =
        resolve_external_link_inputs(project, toolchain, &intermediates_dir, target)?;
    let mut compiler_invocations = semantic_c_family_compiler_invocations(
        project,
        toolchain,
        profile,
        &intermediates_dir,
        index_store_path,
        &external_link_inputs,
        target,
        include_source,
    )?;
    let swift_sources = collect_target_swift_files(project, target, include_source)?;
    if swift_sources.is_empty() {
        return Ok(compiler_invocations);
    }

    let module_output_path = semantic_module_output_path(target, &intermediates_dir);
    let product_path = intermediates_dir.join(format!("{}.artifact", target.name));
    let invocation = target_swiftc_invocation(
        toolchain,
        profile,
        SwiftTargetCompilePlan {
            target_kind: target.kind,
            module_name: &target.name,
            product_path: &product_path,
            module_output_path: module_output_path.as_deref(),
            swift_sources: &swift_sources,
            package_outputs: &package_outputs,
            external_link_inputs: &external_link_inputs,
            object_files: &[],
            index_store_path: Some(index_store_path),
        },
    )?;
    let mut arguments = Vec::with_capacity(invocation.args.len() + 1);
    arguments.push("swiftc".to_owned());
    arguments.extend(
        invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned()),
    );
    compiler_invocations.push(SemanticCompilerInvocation {
        platform: toolchain.platform.to_string(),
        destination: toolchain.destination.as_str().to_owned(),
        language: "swift".to_owned(),
        sdk_name: toolchain.sdk_name.clone(),
        target: target.name.clone(),
        module_name: target.name.clone(),
        target_triple: toolchain.target_triple.clone(),
        working_directory: project.root.clone(),
        toolchain_root: toolchain.toolchain_root()?,
        arguments,
        source_files: invocation.source_files,
        output_path: None,
    });
    Ok(compiler_invocations)
}

fn semantic_module_output_path(
    target: &TargetManifest,
    intermediates_dir: &Path,
) -> Option<PathBuf> {
    if matches!(
        target.kind,
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Framework
    ) {
        Some(intermediates_dir.join(format!("{}.swiftmodule", target.name)))
    } else {
        None
    }
}

fn compile_analysis_swift_packages(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    index_store_path: &Path,
    target: &TargetManifest,
) -> Result<Vec<PackageBuildOutput>> {
    let mut outputs = Vec::new();
    for dependency in &target.swift_packages {
        outputs.push(compile_swift_package(
            project,
            toolchain,
            profile,
            intermediates_dir,
            Some(index_store_path),
            dependency,
        )?);
    }
    Ok(outputs)
}

fn semantic_c_family_compiler_invocations<F>(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    index_store_path: &Path,
    external_link_inputs: &crate::apple::build::external::ExternalLinkInputs,
    target: &TargetManifest,
    include_source: &F,
) -> Result<Vec<SemanticCompilerInvocation>>
where
    F: Fn(&Path) -> bool,
{
    let toolchain_root = toolchain.toolchain_root()?;
    let mut invocations = Vec::new();
    for extension in C_FAMILY_SOURCE_EXTENSIONS {
        for source in
            collect_target_files_with_extensions(project, target, &[extension], include_source)?
        {
            let language = ClangSourceLanguage::from_extension(extension)
                .with_context(|| format!("unsupported C-family extension `{extension}`"))?;
            let output_path = intermediates_dir.join(object_file_name(&source)?);
            let invocation = target_clang_invocation(
                toolchain,
                profile,
                ClangCompilePlan {
                    source_file: &source,
                    output_path: &output_path,
                    language,
                    external_link_inputs,
                    index_store_path: Some(index_store_path),
                },
            )?;
            let compiler = if language == ClangSourceLanguage::ObjectiveCpp
                || language == ClangSourceLanguage::Cpp
            {
                "clang++"
            } else {
                "clang"
            };
            let mut arguments = Vec::with_capacity(invocation.args.len() + 1);
            arguments.push(compiler.to_owned());
            arguments.extend(
                invocation
                    .args
                    .iter()
                    .map(|value| value.to_string_lossy().into_owned()),
            );
            invocations.push(SemanticCompilerInvocation {
                platform: toolchain.platform.to_string(),
                destination: toolchain.destination.as_str().to_owned(),
                language: language.language_id().to_owned(),
                sdk_name: toolchain.sdk_name.clone(),
                target: target.name.clone(),
                module_name: target.name.clone(),
                target_triple: toolchain.target_triple.clone(),
                working_directory: project.root.clone(),
                toolchain_root: toolchain_root.clone(),
                arguments,
                source_files: vec![invocation.source_file],
                output_path: Some(invocation.output_path.to_string_lossy().into_owned()),
            });
        }
    }
    Ok(invocations)
}

fn analysis_destination_for_platform(platform: ApplePlatform) -> DestinationKind {
    if platform == ApplePlatform::Macos {
        DestinationKind::Device
    } else {
        DestinationKind::Simulator
    }
}

fn collect_target_swift_files<F>(
    project: &ProjectContext,
    target: &TargetManifest,
    include_source: &F,
) -> Result<Vec<PathBuf>>
where
    F: Fn(&Path) -> bool,
{
    collect_target_files_with_extensions(project, target, &["swift"], include_source)
}

fn collect_target_files_with_extensions<F>(
    project: &ProjectContext,
    target: &TargetManifest,
    extensions: &[&str],
    include_source: &F,
) -> Result<Vec<PathBuf>>
where
    F: Fn(&Path) -> bool,
{
    let mut files = BTreeSet::new();
    for root in project.resolved_manifest.shared_source_roots() {
        collect_files_under_root(
            &resolve_path(&project.root, &root),
            extensions,
            include_source,
            &mut files,
        )?;
    }
    for root in &target.sources {
        collect_files_under_root(
            &resolve_path(&project.root, root),
            extensions,
            include_source,
            &mut files,
        )?;
    }
    Ok(files.into_iter().collect())
}

fn collect_files_under_root<F>(
    root: &Path,
    extensions: &[&str],
    include_source: &F,
    files: &mut BTreeSet<PathBuf>,
) -> Result<()>
where
    F: Fn(&Path) -> bool,
{
    if !root.exists() {
        bail!("declared source root `{}` does not exist", root.display());
    }
    if root.is_file() {
        if root
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extensions
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
            })
            && include_source(root)
        {
            files.insert(root.to_path_buf());
        }
        return Ok(());
    }
    for path in collect_files_with_extensions(root, extensions)? {
        if !include_source(&path) {
            continue;
        }
        files.insert(path);
    }
    Ok(())
}

fn source_roots(project: &ProjectContext) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for root in project.resolved_manifest.shared_source_roots() {
        roots.insert(resolve_path(&project.root, &root));
    }
    for target in &project.resolved_manifest.targets {
        for root in &target.sources {
            roots.insert(resolve_path(&project.root, root));
        }
    }
    roots.into_iter().collect()
}
