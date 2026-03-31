use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[path = "testing/ui.rs"]
pub(crate) mod ui;

use crate::apple::build::external::resolve_swift_package_dependency;
use crate::cli::TestArgs;
use crate::context::ProjectContext;
use crate::manifest::{
    SwiftPackageDependency, SwiftPackageSource, TargetManifest, TestTargetManifest,
};
use crate::util::{
    collect_files_with_extensions, ensure_dir, print_success, resolve_path, run_command,
};

const GENERATED_PACKAGE_NAME: &str = "OrbitGeneratedTests";
const GENERATED_TESTS_DIR: &str = "tests/swift-testing";
const C_FAMILY_SOURCE_EXTENSIONS: &[&str] = &["c", "m", "mm", "cpp", "cc", "cxx"];
const SWIFT_SOURCE_EXTENSIONS: &[&str] = &["swift"];

pub fn run_tests(project: &ProjectContext, args: &TestArgs) -> Result<()> {
    if args.ui {
        return ui::run_ui_tests(project, args);
    }
    if args.platform.is_some() {
        bail!("`orbit test --platform ...` is only supported together with `--ui`");
    }

    let Some(unit_tests) = project.resolved_manifest.tests.unit.as_ref() else {
        if project.resolved_manifest.tests.ui.is_some() {
            bail!("manifest does not declare `tests.unit`; pass `orbit test --ui` to run UI tests");
        }
        bail!("manifest does not declare `tests.unit`");
    };

    let root_target = project.resolved_manifest.resolve_target(None)?;
    validate_swift_testing_layout(project, root_target, unit_tests)?;

    let package = materialize_swift_testing_package(project, root_target, unit_tests)?;
    run_swift_testing_package(&package)?;
    print_success(format!(
        "Swift Testing passed for `{}` using {} test source root(s).",
        root_target.name,
        unit_tests.sources.len()
    ));
    Ok(())
}

#[derive(Debug, Clone)]
struct GeneratedSwiftTestingPackage {
    package_root: PathBuf,
    scratch_path: PathBuf,
    cache_path: PathBuf,
    attachments_path: PathBuf,
}

#[derive(Debug, Clone)]
struct GeneratedTargetLayout {
    target_path: String,
    source_entries: Vec<String>,
    resource_entries: Vec<String>,
}

#[derive(Debug, Clone)]
struct GeneratedPackageDependency {
    package_declaration: String,
    target_dependency: String,
}

fn validate_swift_testing_layout(
    project: &ProjectContext,
    root_target: &TargetManifest,
    unit_tests: &TestTargetManifest,
) -> Result<()> {
    let app_swift_sources =
        collect_declared_sources(project, &root_target.sources, SWIFT_SOURCE_EXTENSIONS)?;
    if app_swift_sources.is_empty() {
        bail!(
            "target `{}` does not contain any Swift sources for Swift Testing",
            root_target.name
        );
    }

    let c_family_sources =
        collect_declared_sources(project, &root_target.sources, C_FAMILY_SOURCE_EXTENSIONS)?;
    if let Some(source) = c_family_sources.first() {
        bail!(
            "Orbit Swift Testing currently supports Swift-only app targets; found C-family source `{}` in `{}`",
            source.display(),
            root_target.name
        );
    }

    let test_swift_sources =
        collect_declared_sources(project, &unit_tests.sources, SWIFT_SOURCE_EXTENSIONS)?;
    if test_swift_sources.is_empty() {
        bail!("`tests.unit` does not contain any Swift sources");
    }

    Ok(())
}

fn materialize_swift_testing_package(
    project: &ProjectContext,
    root_target: &TargetManifest,
    unit_tests: &TestTargetManifest,
) -> Result<GeneratedSwiftTestingPackage> {
    let runner_root = project.project_paths.orbit_dir.join(GENERATED_TESTS_DIR);
    let package_root = runner_root.join("package");
    let scratch_path = runner_root.join("scratch");
    let cache_path = runner_root.join("cache");
    let attachments_path = runner_root.join("attachments");

    ensure_dir(&runner_root)?;
    ensure_dir(&scratch_path)?;
    ensure_dir(&cache_path)?;
    ensure_dir(&attachments_path)?;
    recreate_dir(&package_root)?;

    let app_target_dir = package_root
        .join("Targets")
        .join(sanitize_path_component(&root_target.name));
    let test_target_name = format!("{}UnitTests", root_target.name);
    let test_target_dir = package_root
        .join("Targets")
        .join(sanitize_path_component(&test_target_name));

    let app_layout = materialize_target_layout(
        &project.root,
        &app_target_dir,
        &root_target.sources,
        &root_target.resources,
    )?;
    let test_layout =
        materialize_target_layout(&project.root, &test_target_dir, &unit_tests.sources, &[])?;
    let package_dependencies = resolve_package_dependencies(project, &root_target.swift_packages)?;
    let package_manifest = render_package_manifest(
        root_target.name.as_str(),
        test_target_name.as_str(),
        &app_layout,
        &test_layout,
        &package_dependencies,
    );
    fs::write(package_root.join("Package.swift"), package_manifest).with_context(|| {
        format!(
            "failed to write {}",
            package_root.join("Package.swift").display()
        )
    })?;

    Ok(GeneratedSwiftTestingPackage {
        package_root,
        scratch_path,
        cache_path,
        attachments_path,
    })
}

fn materialize_target_layout(
    project_root: &Path,
    target_root: &Path,
    sources: &[PathBuf],
    resources: &[PathBuf],
) -> Result<GeneratedTargetLayout> {
    recreate_dir(target_root)?;

    let source_root = target_root.join("Sources");
    let resource_root = target_root.join("Resources");
    ensure_dir(&source_root)?;
    ensure_dir(&resource_root)?;

    let source_entries = materialize_path_links(project_root, &source_root, "input", sources)?;
    let resource_entries =
        materialize_path_links(project_root, &resource_root, "resource", resources)?;

    Ok(GeneratedTargetLayout {
        target_path: target_root
            .strip_prefix(
                target_root
                    .parent()
                    .and_then(Path::parent)
                    .context("generated target root was missing package parent")?,
            )
            .context("failed to relativize generated target root")?
            .to_string_lossy()
            .replace('\\', "/"),
        source_entries,
        resource_entries,
    })
}

fn materialize_path_links(
    project_root: &Path,
    destination_root: &Path,
    prefix: &str,
    inputs: &[PathBuf],
) -> Result<Vec<String>> {
    let mut entries = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        let resolved = resolve_path(project_root, input);
        if !resolved.exists() {
            bail!("declared path `{}` does not exist", resolved.display());
        }
        let destination = destination_root.join(format!("{prefix}-{index}"));
        create_symlink(&resolved, &destination)?;
        entries.push(
            destination
                .strip_prefix(
                    destination_root
                        .parent()
                        .context("destination root was missing a parent")?,
                )
                .context("failed to relativize generated input path")?
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }
    Ok(entries)
}

fn resolve_package_dependencies(
    project: &ProjectContext,
    dependencies: &[SwiftPackageDependency],
) -> Result<Vec<GeneratedPackageDependency>> {
    let mut generated = Vec::new();
    for dependency in dependencies {
        let resolved = resolve_swift_package_dependency(project, dependency)?;
        if !resolved
            .manifest
            .products
            .iter()
            .any(|product| product.name == dependency.product)
        {
            bail!(
                "Swift package `{}` does not vend product `{}`",
                resolved.manifest.name,
                dependency.product
            );
        }
        let package_declaration = match &dependency.source {
            SwiftPackageSource::Path { .. } => format!(
                ".package(name: {}, path: {})",
                swift_string_literal(&dependency.product),
                swift_string_literal(&resolved.root.to_string_lossy())
            ),
            SwiftPackageSource::Git { url, revision, .. } => {
                let revision = revision.as_deref().context(
                    "versioned git dependencies must resolve to an exact revision before testing",
                )?;
                format!(
                    ".package(url: {}, revision: {})",
                    swift_string_literal(url),
                    swift_string_literal(revision)
                )
            }
        };
        let package_reference = match &dependency.source {
            SwiftPackageSource::Path { .. } => dependency.product.clone(),
            SwiftPackageSource::Git { url, .. } => swift_package_identity(url),
        };
        generated.push(GeneratedPackageDependency {
            package_declaration,
            target_dependency: format!(
                ".product(name: {}, package: {})",
                swift_string_literal(&dependency.product),
                swift_string_literal(&package_reference)
            ),
        });
    }
    Ok(generated)
}

fn render_package_manifest(
    app_target_name: &str,
    test_target_name: &str,
    app_layout: &GeneratedTargetLayout,
    test_layout: &GeneratedTargetLayout,
    package_dependencies: &[GeneratedPackageDependency],
) -> String {
    let dependency_section = if package_dependencies.is_empty() {
        String::new()
    } else {
        format!(
            "    dependencies: [\n{}\n    ],\n",
            package_dependencies
                .iter()
                .map(|dependency| format!("        {},", dependency.package_declaration))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let app_target_dependencies = render_target_dependency_list(
        &package_dependencies
            .iter()
            .map(|dependency| dependency.target_dependency.as_str())
            .collect::<Vec<_>>(),
    );
    let mut test_dependencies = vec![swift_string_literal(app_target_name)];
    test_dependencies.extend(
        package_dependencies
            .iter()
            .map(|dependency| dependency.target_dependency.clone()),
    );
    format!(
        "// swift-tools-version: 6.0\nimport PackageDescription\n\nlet package = Package(\n    name: {package_name},\n{dependency_section}    targets: [\n{app_target},\n{test_target}\n    ]\n)\n",
        package_name = swift_string_literal(GENERATED_PACKAGE_NAME),
        dependency_section = dependency_section,
        app_target = render_target_block(
            "executableTarget",
            app_target_name,
            &app_target_dependencies,
            app_layout
        ),
        test_target = render_target_block(
            "testTarget",
            test_target_name,
            &render_target_dependency_list(
                &test_dependencies
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            ),
            test_layout
        )
    )
}

fn render_target_block(
    kind: &str,
    target_name: &str,
    dependencies: &str,
    layout: &GeneratedTargetLayout,
) -> String {
    let sources = render_string_list(&layout.source_entries);
    let resources = if layout.resource_entries.is_empty() {
        String::new()
    } else {
        format!(
            ",\n            resources: [\n{}\n            ]",
            layout
                .resource_entries
                .iter()
                .map(|entry| format!("                .copy({})", swift_string_literal(entry)))
                .collect::<Vec<_>>()
                .join(",\n")
        )
    };
    format!(
        "        .{kind}(\n            name: {name},\n            dependencies: {dependencies},\n            path: {path},\n            sources: {sources}{resources}\n        )",
        kind = kind,
        name = swift_string_literal(target_name),
        dependencies = dependencies,
        path = swift_string_literal(&layout.target_path),
        sources = sources,
        resources = resources
    )
}

fn render_target_dependency_list(dependencies: &[&str]) -> String {
    if dependencies.is_empty() {
        "[]".to_owned()
    } else {
        format!("[{}]", dependencies.join(", "))
    }
}

fn render_string_list(values: &[String]) -> String {
    if values.is_empty() {
        "[]".to_owned()
    } else {
        format!(
            "[{}]",
            values
                .iter()
                .map(|value| swift_string_literal(value))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn run_swift_testing_package(package: &GeneratedSwiftTestingPackage) -> Result<()> {
    let mut command = Command::new("swift");
    command
        .arg("test")
        .arg("--disable-keychain")
        .arg("--package-path")
        .arg(&package.package_root)
        .arg("--scratch-path")
        .arg(&package.scratch_path)
        .arg("--cache-path")
        .arg(&package.cache_path)
        .arg("--enable-swift-testing")
        .arg("--disable-xctest")
        .arg("--attachments-path")
        .arg(&package.attachments_path);
    run_command(&mut command)
}

fn collect_declared_sources(
    project: &ProjectContext,
    roots: &[PathBuf],
    extensions: &[&str],
) -> Result<Vec<PathBuf>> {
    let mut collected = Vec::new();
    for root in roots {
        let resolved = resolve_path(&project.root, root);
        if !resolved.exists() {
            bail!("declared path `{}` does not exist", resolved.display());
        }
        collected.extend(collect_files_with_extensions(&resolved, extensions)?);
    }
    collected.sort();
    collected.dedup();
    Ok(collected)
}

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    ensure_dir(path)
}

#[cfg(unix)]
fn create_symlink(source: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(source, destination).with_context(|| {
        format!(
            "failed to create symlink {} -> {}",
            destination.display(),
            source.display()
        )
    })
}

#[cfg(not(unix))]
fn create_symlink(_source: &Path, _destination: &Path) -> Result<()> {
    bail!("Orbit Swift Testing currently requires Unix symlink support")
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn swift_package_identity(url: &str) -> String {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    trimmed
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

fn swift_string_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('"');
    for character in value.chars() {
        match character {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            _ => literal.push(character),
        }
    }
    literal.push('"');
    literal
}

#[cfg(test)]
mod tests {
    use super::{GeneratedPackageDependency, GeneratedTargetLayout, render_package_manifest};

    #[test]
    fn package_manifest_includes_external_products_for_app_and_tests() {
        let manifest = render_package_manifest(
            "ExampleApp",
            "ExampleAppUnitTests",
            &GeneratedTargetLayout {
                target_path: "Targets/ExampleApp".to_owned(),
                source_entries: vec!["Sources/input-0".to_owned()],
                resource_entries: vec!["Resources/resource-0".to_owned()],
            },
            &GeneratedTargetLayout {
                target_path: "Targets/ExampleAppUnitTests".to_owned(),
                source_entries: vec!["Sources/input-0".to_owned()],
                resource_entries: Vec::new(),
            },
            &[GeneratedPackageDependency {
                package_declaration: ".package(name: \"OrbitPkg\", path: \"/tmp/OrbitPkg\")"
                    .to_owned(),
                target_dependency: ".product(name: \"OrbitPkg\", package: \"OrbitPkg\")".to_owned(),
            }],
        );

        assert!(manifest.contains(".package(name: \"OrbitPkg\", path: \"/tmp/OrbitPkg\")"));
        assert!(
            manifest.contains(
                ".executableTarget(\n            name: \"ExampleApp\",\n            dependencies: [.product(name: \"OrbitPkg\", package: \"OrbitPkg\")]"
            )
        );
        assert!(
            manifest.contains(
                ".testTarget(\n            name: \"ExampleAppUnitTests\",\n            dependencies: [\"ExampleApp\", .product(name: \"OrbitPkg\", package: \"OrbitPkg\")]"
            )
        );
        assert!(manifest.contains(".copy(\"Resources/resource-0\")"));
    }
}
