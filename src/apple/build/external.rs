use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::apple::build::swiftc::{
    SwiftPackageTargetCompilePlan, package_target_swiftc_invocation,
};
use crate::apple::build::toolchain::{DestinationKind, Toolchain};
use crate::apple::git_dependencies::materialize_git_dependency;
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, ProfileManifest, SwiftPackageDependency, SwiftPackageSource, TargetManifest,
    XcframeworkDependency,
};
use crate::util::{
    collect_files_with_extensions, command_output, ensure_dir, read_json_file_if_exists,
    resolve_path, run_command, write_json_file,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct ExternalLinkInputs {
    pub module_search_paths: Vec<PathBuf>,
    pub framework_search_paths: Vec<PathBuf>,
    pub library_search_paths: Vec<PathBuf>,
    pub link_frameworks: Vec<String>,
    pub weak_frameworks: Vec<String>,
    pub link_libraries: Vec<String>,
    pub embedded_payloads: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct PackageBuildOutput {
    pub module_dir: PathBuf,
    pub library_dir: PathBuf,
    pub link_libraries: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SwiftPackageBuildCacheInfo {
    fingerprint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SwiftPackageManifestCacheInfo {
    fingerprint: String,
    description: String,
    manifest: SwiftPackageManifest,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SwiftPackageManifest {
    pub name: String,
    pub products: Vec<SwiftPackageProduct>,
    pub targets: Vec<SwiftPackageTarget>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SwiftPackageProduct {
    pub name: String,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SwiftPackageTarget {
    pub name: String,
    pub path: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<SwiftPackageTargetDependency>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum SwiftPackageTargetDependency {
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

#[derive(Debug, Clone, Deserialize)]
struct XcframeworkInfoPlist {
    #[serde(rename = "AvailableLibraries")]
    available_libraries: Vec<XcframeworkLibrary>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct XcframeworkLibrary {
    #[serde(rename = "LibraryIdentifier")]
    pub library_identifier: String,
    #[serde(rename = "LibraryPath")]
    pub library_path: String,
    #[serde(rename = "HeadersPath")]
    pub headers_path: Option<String>,
    #[serde(rename = "SupportedPlatform")]
    pub supported_platform: String,
    #[serde(rename = "SupportedPlatformVariant")]
    pub supported_platform_variant: Option<String>,
    #[serde(rename = "SupportedArchitectures")]
    pub supported_architectures: Vec<String>,
}

pub(crate) fn resolve_external_link_inputs(
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

pub(crate) fn target_dependency_watch_roots(
    project: &ProjectContext,
    target: &TargetManifest,
) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for dependency in &target.swift_packages {
        if let SwiftPackageSource::Path { path } = &dependency.source {
            let root = resolve_path(&project.root, path);
            if root.starts_with(&project.root) {
                roots.insert(root);
            }
        }
    }
    for dependency in &target.xcframeworks {
        let root = resolve_path(&project.root, &dependency.path);
        if root.starts_with(&project.root) {
            roots.insert(root);
        }
    }
    roots.into_iter().collect()
}

pub(crate) fn resolve_xcframework_dependency(
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

    let explicit_name = dependency.library.as_deref();
    let file_name = library_path
        .file_name()
        .and_then(OsStr::to_str)
        .context("XCFramework library path is missing a file name")?;
    let should_embed = resolve_xcframework_embed_behavior(&library_path, dependency.embed)?;
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
        if should_embed {
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
        if should_embed && file_name.ends_with(".dylib") {
            inputs.embedded_payloads.push(library_path);
        }
    }

    Ok(inputs)
}

pub(crate) fn select_xcframework_library<'a>(
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

pub(crate) fn apply_external_link_inputs(command: &mut Command, inputs: &ExternalLinkInputs) {
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

pub(crate) fn apply_external_compile_inputs(args: &mut Vec<OsString>, inputs: &ExternalLinkInputs) {
    for path in &inputs.module_search_paths {
        args.push("-I".into());
        args.push(path.as_os_str().to_os_string());
    }
    for path in &inputs.framework_search_paths {
        args.push("-F".into());
        args.push(path.as_os_str().to_os_string());
    }
}

pub(crate) fn compile_swift_package(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    _intermediates_dir: &Path,
    index_store_path: Option<&Path>,
    dependency: &SwiftPackageDependency,
) -> Result<PackageBuildOutput> {
    let package_root = swift_package_root(project, dependency)?;
    let (description, package) = load_swift_package_manifest(project, &package_root)?;

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
    let cache_root = swift_package_cache_root(
        project,
        &package_root,
        dependency,
        toolchain,
        profile,
        index_store_path,
    );
    let module_dir = cache_root.join("modules");
    let library_dir = cache_root.join("libs");
    let cache_info_path = cache_root.join("build-info.json");
    ensure_dir(&module_dir)?;
    ensure_dir(&library_dir)?;

    let targets_by_name = package
        .targets
        .iter()
        .map(|target| (target.name.as_str(), target))
        .collect::<HashMap<_, _>>();
    let built_libraries = built_target_names
        .iter()
        .map(|target_name| swift_package_library_name(&package.name, target_name))
        .collect::<Vec<_>>();
    let fingerprint = swift_package_build_fingerprint(
        &description,
        &package_root,
        toolchain,
        profile,
        &built_target_names,
        &targets_by_name,
    )?;
    if let Some(cache_info) =
        read_json_file_if_exists::<SwiftPackageBuildCacheInfo>(&cache_info_path)?
        && cache_info.fingerprint == fingerprint
        && swift_package_cache_outputs_exist(
            &module_dir,
            &library_dir,
            &built_target_names,
            &built_libraries,
        )
    {
        return Ok(PackageBuildOutput {
            module_dir,
            library_dir,
            link_libraries: built_libraries,
        });
    }

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
        let dependency_libraries =
            package_target_local_dependencies(package_target, &targets_by_name)?
                .into_iter()
                .map(|dependency_name| swift_package_library_name(&package.name, &dependency_name))
                .collect::<Vec<_>>();
        let invocation = package_target_swiftc_invocation(
            toolchain,
            profile,
            SwiftPackageTargetCompilePlan {
                module_name: target_name,
                product_path: &library_path,
                module_output_path: &module_path,
                swift_sources: &swift_sources,
                module_search_paths: std::slice::from_ref(&module_dir),
                library_search_paths: std::slice::from_ref(&library_dir),
                link_libraries: &dependency_libraries,
                index_store_path,
            },
        )?;
        let source_count = invocation.source_files.len();
        let mut command = invocation.command(toolchain);
        run_command(&mut command).with_context(|| {
            format!(
                "failed to compile Swift package target `{target_name}` from {source_count} source file(s)"
            )
        })?;
    }
    write_json_file(
        &cache_info_path,
        &SwiftPackageBuildCacheInfo { fingerprint },
    )?;

    Ok(PackageBuildOutput {
        module_dir,
        library_dir,
        link_libraries: built_libraries,
    })
}

fn swift_package_root(
    project: &ProjectContext,
    dependency: &SwiftPackageDependency,
) -> Result<PathBuf> {
    match &dependency.source {
        SwiftPackageSource::Path { path } => Ok(resolve_path(&project.root, path)),
        SwiftPackageSource::Git {
            url,
            version,
            revision,
        } => match (version.as_deref(), revision.as_deref()) {
            (_, Some(revision)) => materialize_git_dependency(&project.app, url, revision),
            (Some(_), None) => unreachable!(
                "versioned git dependencies must be resolved through orbit.lock before build resolution"
            ),
            (None, None) => unreachable!("git dependencies are validated before build resolution"),
        },
    }
}

pub(crate) fn ordered_package_targets(
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

fn resolve_xcframework_embed_behavior(library_path: &Path, explicit: Option<bool>) -> Result<bool> {
    match explicit {
        Some(embed) => Ok(embed),
        None => detect_embedded_binary_kind(library_path),
    }
}

fn detect_embedded_binary_kind(library_path: &Path) -> Result<bool> {
    match library_path.extension().and_then(OsStr::to_str) {
        Some("dylib") => Ok(true),
        Some("a") => Ok(false),
        Some("framework") => framework_requires_embedding(library_path),
        _ => macho_library_requires_embedding(library_path),
    }
}

fn framework_requires_embedding(framework_path: &Path) -> Result<bool> {
    let framework_name = framework_path
        .file_stem()
        .and_then(OsStr::to_str)
        .context("framework path is missing a framework name")?;
    let binary_path = framework_path.join(framework_name);
    macho_library_requires_embedding(&binary_path).with_context(|| {
        format!(
            "failed to inspect XCFramework binary inside {}",
            framework_path.display()
        )
    })
}

fn macho_library_requires_embedding(binary_path: &Path) -> Result<bool> {
    let description = command_output(Command::new("file").arg(binary_path))?;
    let description = description.trim();
    if description.contains("current ar archive") || description.contains("static library") {
        return Ok(false);
    }
    if description.contains("dynamically linked shared library")
        || description.contains("shared library")
    {
        return Ok(true);
    }
    bail!(
        "failed to determine whether `{}` should be embedded from `file` output: {}",
        binary_path.display(),
        description
    )
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

fn dedup_vec<T>(values: &mut Vec<T>)
where
    T: Ord,
{
    values.sort();
    values.dedup();
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

fn swift_package_cache_root(
    project: &ProjectContext,
    package_root: &Path,
    dependency: &SwiftPackageDependency,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    index_store_path: Option<&Path>,
) -> PathBuf {
    let package_key = short_hash(
        &[
            package_root.to_string_lossy().as_ref(),
            dependency.product.as_str(),
            toolchain.target_triple.as_str(),
            profile.variant_name().as_str(),
            &index_store_cache_key(index_store_path),
        ]
        .join("\n"),
    );
    project
        .app
        .global_paths
        .cache_dir
        .join("swiftpackages")
        .join(format!("{}-{}", dependency.product, package_key))
}

fn load_swift_package_manifest(
    project: &ProjectContext,
    package_root: &Path,
) -> Result<(String, SwiftPackageManifest)> {
    let cache_root = swift_package_manifest_cache_root(project, package_root);
    let cache_info_path = cache_root.join("manifest.json");
    ensure_dir(&cache_root)?;

    let fingerprint = swift_package_manifest_fingerprint(package_root)?;
    if let Some(cache_info) =
        read_json_file_if_exists::<SwiftPackageManifestCacheInfo>(&cache_info_path)?
        && cache_info.fingerprint == fingerprint
    {
        return Ok((cache_info.description, cache_info.manifest));
    }

    let description = command_output(
        Command::new("swift")
            .args(["package", "--package-path"])
            .arg(package_root)
            .arg("dump-package"),
    )?;
    let manifest: SwiftPackageManifest = serde_json::from_str(&description).with_context(|| {
        format!(
            "failed to parse Swift package description for {}",
            package_root.display()
        )
    })?;
    write_json_file(
        &cache_info_path,
        &SwiftPackageManifestCacheInfo {
            fingerprint,
            description: description.clone(),
            manifest: manifest.clone(),
        },
    )?;
    Ok((description, manifest))
}

fn swift_package_manifest_cache_root(project: &ProjectContext, package_root: &Path) -> PathBuf {
    project
        .app
        .global_paths
        .cache_dir
        .join("swiftpackage-manifests")
        .join(short_hash(package_root.to_string_lossy().as_ref()))
}

fn swift_package_manifest_fingerprint(package_root: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_optional_file(&mut hasher, &package_root.join("Package.swift"))?;
    hash_optional_file(&mut hasher, &package_root.join("Package.resolved"))?;
    hash_optional_file(
        &mut hasher,
        &package_root.join(".swiftpm").join("Package.resolved"),
    )?;
    Ok(hex_digest(hasher.finalize()))
}

fn swift_package_build_fingerprint(
    package_description: &str,
    package_root: &Path,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    built_target_names: &[String],
    targets_by_name: &HashMap<&str, &SwiftPackageTarget>,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(package_description.as_bytes());
    hasher.update(toolchain.target_triple.as_bytes());
    hasher.update(toolchain.sdk_name.as_bytes());
    hasher.update(profile.variant_name().as_bytes());
    for target_name in built_target_names {
        hasher.update(target_name.as_bytes());
        let package_target = targets_by_name
            .get(target_name.as_str())
            .copied()
            .with_context(|| format!("missing Swift package target `{target_name}`"))?;
        let source_root = package_target
            .path
            .as_ref()
            .map(|path| package_root.join(path))
            .unwrap_or_else(|| package_root.join("Sources").join(target_name));
        for source in collect_files_with_extensions(&source_root, &["swift"])? {
            let metadata = fs::metadata(&source)
                .with_context(|| format!("failed to read metadata for {}", source.display()))?;
            hasher.update(source.to_string_lossy().as_bytes());
            hasher.update(metadata.len().to_string().as_bytes());
            let modified = metadata
                .modified()
                .with_context(|| format!("failed to read mtime for {}", source.display()))?
                .duration_since(std::time::UNIX_EPOCH)
                .with_context(|| format!("mtime for {} was before UNIX_EPOCH", source.display()))?;
            hasher.update(modified.as_nanos().to_string().as_bytes());
        }
    }
    Ok(hex_digest(hasher.finalize()))
}

fn swift_package_cache_outputs_exist(
    module_dir: &Path,
    library_dir: &Path,
    built_target_names: &[String],
    built_libraries: &[String],
) -> bool {
    built_target_names.iter().all(|target_name| {
        module_dir
            .join(format!("{target_name}.swiftmodule"))
            .exists()
    }) && built_libraries
        .iter()
        .all(|library_name| library_dir.join(format!("lib{library_name}.a")).exists())
}

fn index_store_cache_key(index_store_path: Option<&Path>) -> String {
    match index_store_path {
        Some(path) => format!("indexed:{}", short_hash(path.to_string_lossy().as_ref())),
        None => "no-index".to_owned(),
    }
}

fn hash_optional_file(hasher: &mut Sha256, path: &Path) -> Result<()> {
    hasher.update(path.to_string_lossy().as_bytes());
    if !path.exists() {
        hasher.update(b"missing");
        return Ok(());
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    hasher.update(bytes.len().to_string().as_bytes());
    hasher.update(&bytes);
    Ok(())
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
