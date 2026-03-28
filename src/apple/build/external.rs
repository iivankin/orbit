use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::apple::build::toolchain::{DestinationKind, Toolchain};
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, ProfileManifest, SwiftPackageDependency, TargetManifest, XcframeworkDependency,
};
use crate::util::{
    collect_files_with_extensions, command_output, ensure_dir, resolve_path, run_command,
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

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SwiftPackageManifest {
    pub name: String,
    pub products: Vec<SwiftPackageProduct>,
    pub targets: Vec<SwiftPackageTarget>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SwiftPackageProduct {
    pub name: String,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SwiftPackageTarget {
    pub name: String,
    pub path: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<SwiftPackageTargetDependency>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
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

    let explicit_name = dependency.library.as_ref().map(|name| name.as_str());
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

pub(crate) fn compile_swift_package(
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
        if profile.is_debug() {
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
        run_command(&mut command)?;
        built_libraries.push(library_name);
    }

    Ok(PackageBuildOutput {
        module_dir,
        library_dir,
        link_libraries: built_libraries,
    })
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
