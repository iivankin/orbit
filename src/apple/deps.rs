use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::apple::git_dependencies::{latest_remote_revision, latest_remote_version_revision};
use crate::apple::lockfile::{LockfileChange, sync_lockfile_with_env};
use crate::cli::DepsUpdateArgs;
use crate::context::AppContext;
use crate::util::{print_success, write_json_file};

#[derive(Debug, Clone)]
struct DependencyRevisionUpdate {
    name: String,
    old_version: Option<String>,
    new_version: Option<String>,
    old_revision: Option<String>,
    new_revision: Option<String>,
}

#[derive(Debug, Default)]
struct DependencyUpdateSummary {
    matched_git_dependencies: usize,
    updates: Vec<DependencyRevisionUpdate>,
}

pub fn update_dependencies(
    app: &AppContext,
    args: &DepsUpdateArgs,
    requested_manifest: Option<&Path>,
) -> Result<()> {
    let manifest_path = app.resolve_manifest_path_for_dispatch(requested_manifest)?;
    let manifest_bytes = fs::read(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let mut manifest: Value = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;

    let mut summary = DependencyUpdateSummary::default();
    update_manifest_dependency_revisions(&mut manifest, args.dependency.as_deref(), &mut summary)?;

    if summary.matched_git_dependencies == 0 {
        match args.dependency.as_deref() {
            Some(name) => bail!("did not find a git dependency named `{name}` in the manifest"),
            None => bail!("did not find any git dependencies in the manifest"),
        }
    }

    let manifest_changed = !summary.updates.is_empty();
    if manifest_changed {
        write_json_file(&manifest_path, &manifest)?;
    }
    let lock_summary = sync_lockfile_with_env(&manifest_path, app.manifest_env())?;

    if !manifest_changed && !lock_summary.changed() {
        print_success(match args.dependency.as_deref() {
            Some(name) => format!(
                "Git dependency `{name}` is already pinned to the latest remote revision for its manifest policy."
            ),
            None => {
                "All git dependencies are already pinned to the latest remote revisions for their manifest policy."
                    .to_owned()
            }
        });
        return Ok(());
    }

    if manifest_changed {
        print_success(format!(
            "Updated {} git dependenc{} in {}.",
            summary.updates.len(),
            if summary.updates.len() == 1 {
                "y"
            } else {
                "ies"
            },
            manifest_path.display()
        ));
        for update in &summary.updates {
            println!(
                "  {} {}{} -> {}{}",
                update.name,
                update
                    .old_version
                    .as_deref()
                    .map(|version| format!("{version} "))
                    .unwrap_or_default(),
                update
                    .old_revision
                    .as_deref()
                    .map(short_revision)
                    .unwrap_or("-"),
                update
                    .new_version
                    .as_deref()
                    .map(|version| format!("{version} "))
                    .unwrap_or_default(),
                update
                    .new_revision
                    .as_deref()
                    .map(short_revision)
                    .unwrap_or("-")
            );
        }
    }

    match lock_summary.change {
        LockfileChange::Written => {
            print_success(format!(
                "Wrote .orbi/orbi.lock for {} versioned git dependenc{}.",
                lock_summary.versioned_dependency_count,
                if lock_summary.versioned_dependency_count == 1 {
                    "y"
                } else {
                    "ies"
                }
            ));
        }
        LockfileChange::Removed => {
            print_success(
                "Removed stale .orbi/orbi.lock because the manifest no longer contains versioned git dependencies.",
            );
        }
        LockfileChange::Unchanged => {}
    }
    Ok(())
}

fn update_manifest_dependency_revisions(
    value: &mut Value,
    filter: Option<&str>,
    summary: &mut DependencyUpdateSummary,
) -> Result<()> {
    match value {
        Value::Object(object) => {
            if let Some(Value::Object(dependencies)) = object.get_mut("dependencies") {
                update_dependency_map(dependencies, filter, summary)?;
            }
            for (key, child) in object {
                if key == "dependencies" {
                    continue;
                }
                update_manifest_dependency_revisions(child, filter, summary)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                update_manifest_dependency_revisions(item, filter, summary)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn update_dependency_map(
    dependencies: &mut Map<String, Value>,
    filter: Option<&str>,
    summary: &mut DependencyUpdateSummary,
) -> Result<()> {
    for (name, dependency) in dependencies {
        if filter.is_some_and(|filter_name| filter_name != name) {
            continue;
        }
        let dependency_object = dependency
            .as_object_mut()
            .with_context(|| format!("dependency `{name}` must be a JSON object"))?;
        let Some(git_url) = dependency_object.get("git").and_then(Value::as_str) else {
            continue;
        };
        let current_revision = dependency_object
            .get("revision")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let current_version = dependency_object
            .get("version")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if current_version.is_none() && current_revision.is_none() {
            bail!("git dependency `{name}` must declare at least one of `version` or `revision`");
        }
        summary.matched_git_dependencies += 1;

        let (new_version, new_revision) =
            if let Some(current_version_value) = current_version.as_deref() {
                let (version, _) = latest_remote_version_revision(git_url, current_version_value)
                    .with_context(|| {
                    format!(
                        "failed to resolve the latest tagged version for `{name}` in major {}",
                        current_version_value
                            .split('.')
                            .next()
                            .unwrap_or(current_version_value)
                    )
                })?;
                (Some(version), None)
            } else {
                (
                    None,
                    Some(latest_remote_revision(git_url).with_context(|| {
                        format!("failed to resolve the latest revision for `{name}`")
                    })?),
                )
            };
        if new_revision == current_revision && new_version == current_version {
            continue;
        }
        if let Some(version) = new_version.as_ref() {
            dependency_object.insert("version".to_owned(), Value::String(version.clone()));
        }
        match new_revision.as_ref() {
            Some(revision) => {
                dependency_object.insert("revision".to_owned(), Value::String(revision.clone()));
            }
            None => {
                dependency_object.remove("revision");
            }
        }
        summary.updates.push(DependencyRevisionUpdate {
            name: name.clone(),
            old_version: current_version,
            new_version,
            old_revision: current_revision,
            new_revision,
        });
    }
    Ok(())
}

fn short_revision(revision: &str) -> &str {
    revision.get(..12).unwrap_or(revision)
}
