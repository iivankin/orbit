use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::apple::git_dependencies::exact_remote_version_revision;
use crate::manifest::{ResolvedManifest, SwiftPackageSource, read_manifest_value};
use crate::util::{read_json_file, write_json_file};

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

#[allow(dead_code)]
pub(crate) fn ensure_lockfile(
    manifest_path: &Path,
    resolved_manifest: &mut ResolvedManifest,
) -> Result<()> {
    ensure_lockfile_with_env(manifest_path, resolved_manifest, None)
}

pub(crate) fn ensure_lockfile_with_env(
    manifest_path: &Path,
    resolved_manifest: &mut ResolvedManifest,
    env: Option<&str>,
) -> Result<()> {
    let requested_dependencies = collect_versioned_git_dependencies(manifest_path, env)?;
    if requested_dependencies.is_empty() {
        return Ok(());
    }

    let lockfile_path = lockfile_path(manifest_path)?;
    let lockfile_matches = read_existing_lockfile(&lockfile_path)?
        .as_ref()
        .is_some_and(|lockfile| lockfile_matches_requested(lockfile, &requested_dependencies));
    if !lockfile_matches {
        sync_lockfile_with_env(manifest_path, env)?;
    }
    let lockfile = read_json_file::<OrbitLockfile>(&lockfile_path)?;

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
                    "dependency `{product}` is versioned in orbit.json but missing from {}",
                    lockfile_path.display()
                )
            })?;
            if locked.git != *url || locked.version != version {
                bail!(
                    "dependency `{product}` in {} no longer matches orbit.json",
                    lockfile_path.display()
                );
            }
            *revision = Some(locked.revision.clone());
        }
    }

    Ok(())
}

#[allow(dead_code)]
pub(crate) fn sync_lockfile(manifest_path: &Path) -> Result<LockfileSyncSummary> {
    sync_lockfile_with_env(manifest_path, None)
}

pub(crate) fn sync_lockfile_with_env(
    manifest_path: &Path,
    env: Option<&str>,
) -> Result<LockfileSyncSummary> {
    let requested_dependencies = collect_versioned_git_dependencies(manifest_path, env)?;
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
    let previous = read_existing_lockfile(&lockfile_path)?;

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
    Ok(root.join(".orbit").join(LOCKFILE_NAME))
}

fn collect_versioned_git_dependencies(
    manifest_path: &Path,
    env: Option<&str>,
) -> Result<BTreeMap<String, RequestedVersionedGitDependency>> {
    let manifest = read_manifest_value(manifest_path, env)?;
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

fn lockfile_matches_requested(
    lockfile: &OrbitLockfile,
    requested_dependencies: &BTreeMap<String, RequestedVersionedGitDependency>,
) -> bool {
    if lockfile.dependencies.len() != requested_dependencies.len() {
        return false;
    }
    requested_dependencies.iter().all(|(name, requested)| {
        lockfile.dependencies.get(name).is_some_and(|locked| {
            locked.git == requested.git && locked.version == requested.version
        })
    })
}

fn read_existing_lockfile(path: &Path) -> Result<Option<OrbitLockfile>> {
    if !path.exists() {
        return Ok(None);
    }
    match read_json_file(path) {
        Ok(lockfile) => Ok(Some(lockfile)),
        Err(_) => Ok(None),
    }
}
