use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::apple::git_dependencies::exact_remote_version_revision;
use crate::manifest::{ResolvedManifest, SwiftPackageSource};
use crate::util::{read_json_file_if_exists, write_json_file};

pub(crate) const LOCKFILE_NAME: &str = "orbit.lock";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OrbitLockfile {
    #[serde(default)]
    pub dependencies: BTreeMap<String, LockedGitDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LockedGitDependency {
    pub git: String,
    pub version: String,
    pub revision: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LockfileSyncSummary {
    pub versioned_dependency_count: usize,
    pub wrote_file: bool,
    pub removed_file: bool,
}

impl LockfileSyncSummary {
    pub(crate) fn changed(&self) -> bool {
        self.wrote_file || self.removed_file
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestedVersionedGitDependency {
    git: String,
    version: String,
}

pub(crate) fn apply_lockfile(
    manifest_path: &Path,
    resolved_manifest: &mut ResolvedManifest,
) -> Result<()> {
    let mut required_dependencies = BTreeSet::new();
    for target in &resolved_manifest.targets {
        for dependency in &target.swift_packages {
            if let SwiftPackageSource::Git {
                version: Some(_), ..
            } = &dependency.source
            {
                required_dependencies.insert(dependency.product.clone());
            }
        }
    }
    if required_dependencies.is_empty() {
        return Ok(());
    }

    let lockfile_path = lockfile_path(manifest_path)?;
    let lockfile = read_json_file_if_exists::<OrbitLockfile>(&lockfile_path)?
        .with_context(|| {
            format!(
                "manifest contains versioned git dependencies, but {} is missing; run `orbit deps lock`",
                lockfile_path.display()
            )
        })?;

    for target in &mut resolved_manifest.targets {
        for dependency in &mut target.swift_packages {
            let product = dependency.product.clone();
            let SwiftPackageSource::Git {
                url,
                version,
                revision,
            } = &mut dependency.source
            else {
                continue;
            };
            let Some(version) = version.as_deref() else {
                continue;
            };
            let locked = lockfile.dependencies.get(&product).with_context(|| {
                format!(
                    "dependency `{product}` is versioned in orbit.json but missing from {}; run `orbit deps lock`",
                    lockfile_path.display()
                )
            })?;
            if locked.git != *url || locked.version != version {
                bail!(
                    "dependency `{product}` in {} no longer matches orbit.json; run `orbit deps lock`",
                    lockfile_path.display()
                );
            }
            *revision = Some(locked.revision.clone());
        }
    }

    Ok(())
}

pub(crate) fn sync_lockfile(manifest_path: &Path) -> Result<LockfileSyncSummary> {
    let requested_dependencies = collect_versioned_git_dependencies(manifest_path)?;
    let lockfile_path = lockfile_path(manifest_path)?;

    if requested_dependencies.is_empty() {
        if lockfile_path.exists() {
            fs::remove_file(&lockfile_path)
                .with_context(|| format!("failed to remove {}", lockfile_path.display()))?;
            return Ok(LockfileSyncSummary {
                versioned_dependency_count: 0,
                wrote_file: false,
                removed_file: true,
            });
        }
        return Ok(LockfileSyncSummary::default());
    }
    let previous = read_json_file_if_exists::<OrbitLockfile>(&lockfile_path)?;

    let mut dependencies = BTreeMap::new();
    for (name, requested) in &requested_dependencies {
        let revision = exact_remote_version_revision(&requested.git, &requested.version)
            .with_context(|| {
                format!(
                    "failed to resolve exact version `{}` for dependency `{name}`",
                    requested.version
                )
            })?;
        dependencies.insert(
            name.clone(),
            LockedGitDependency {
                git: requested.git.clone(),
                version: requested.version.clone(),
                revision,
            },
        );
    }
    let lockfile = OrbitLockfile { dependencies };
    if previous.as_ref() == Some(&lockfile) {
        return Ok(LockfileSyncSummary {
            versioned_dependency_count: requested_dependencies.len(),
            wrote_file: false,
            removed_file: false,
        });
    }

    write_json_file(&lockfile_path, &lockfile)?;
    Ok(LockfileSyncSummary {
        versioned_dependency_count: requested_dependencies.len(),
        wrote_file: true,
        removed_file: false,
    })
}

pub(crate) fn lockfile_path(manifest_path: &Path) -> Result<PathBuf> {
    let root = manifest_path
        .parent()
        .context("manifest path did not contain a parent directory")?;
    Ok(root.join(LOCKFILE_NAME))
}

fn collect_versioned_git_dependencies(
    manifest_path: &Path,
) -> Result<BTreeMap<String, RequestedVersionedGitDependency>> {
    let manifest_bytes = fs::read(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let mut dependencies = BTreeMap::new();
    collect_versioned_git_dependencies_from_value(&manifest, &mut dependencies)?;
    Ok(dependencies)
}

fn collect_versioned_git_dependencies_from_value(
    value: &Value,
    dependencies: &mut BTreeMap<String, RequestedVersionedGitDependency>,
) -> Result<()> {
    match value {
        Value::Object(object) => {
            if let Some(Value::Object(dependency_map)) = object.get("dependencies") {
                collect_versioned_git_dependencies_from_map(dependency_map, dependencies)?;
            }
            for (key, child) in object {
                if key == "dependencies" {
                    continue;
                }
                collect_versioned_git_dependencies_from_value(child, dependencies)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_versioned_git_dependencies_from_value(item, dependencies)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_versioned_git_dependencies_from_map(
    dependency_map: &serde_json::Map<String, Value>,
    dependencies: &mut BTreeMap<String, RequestedVersionedGitDependency>,
) -> Result<()> {
    for (name, dependency) in dependency_map {
        let dependency_object = dependency
            .as_object()
            .with_context(|| format!("dependency `{name}` must be a JSON object"))?;
        let Some(git) = dependency_object.get("git").and_then(Value::as_str) else {
            continue;
        };
        let Some(version) = dependency_object.get("version").and_then(Value::as_str) else {
            continue;
        };
        let requested = RequestedVersionedGitDependency {
            git: git.to_owned(),
            version: version.to_owned(),
        };
        match dependencies.get(name) {
            Some(existing) if existing != &requested => {
                bail!(
                    "dependency `{name}` is declared multiple times with different git version sources; Orbit requires a single versioned definition per dependency key"
                );
            }
            Some(_) => {}
            None => {
                dependencies.insert(name.clone(), requested);
            }
        }
    }
    Ok(())
}
