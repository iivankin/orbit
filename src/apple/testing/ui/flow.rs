use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::UiCommand;
use super::parser::parse_ui_flow;
use crate::context::ProjectContext;
use crate::manifest::TestTargetManifest;
use crate::util::{collect_files_with_extensions, resolve_path};

pub(super) fn collect_ui_flow_paths(
    project: &ProjectContext,
    target: &TestTargetManifest,
) -> Result<Vec<PathBuf>> {
    let mut flows = Vec::new();
    let mut seen = HashSet::new();
    for root in &target.sources {
        let resolved = resolve_path(&project.root, root);
        if !resolved.exists() {
            bail!("declared path `{}` does not exist", resolved.display());
        }
        for path in collect_files_with_extensions(&resolved, &["yml", "yaml"])? {
            let canonical = canonical_or_absolute(&path)?;
            if seen.insert(canonical.clone()) {
                flows.push(canonical);
            }
        }
    }
    flows.sort();
    Ok(flows)
}

pub(super) fn resolve_path_from_flow(parent_flow: &Path, relative_path: &Path) -> PathBuf {
    let base_dir = parent_flow
        .parent()
        .expect("flow paths are always expected to have a parent directory");
    if relative_path.is_absolute() {
        relative_path.to_path_buf()
    } else {
        base_dir.join(relative_path)
    }
}

pub(super) fn flow_uses_manual_recording(
    flow_path: &Path,
    commands: &[UiCommand],
    visited: &mut HashSet<PathBuf>,
) -> Result<bool> {
    let canonical = canonical_or_absolute(flow_path)?;
    if !visited.insert(canonical.clone()) {
        return Ok(false);
    }
    commands_use_manual_recording(canonical.as_path(), commands, visited)
}

pub(super) fn canonical_or_absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        path.canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()))
    } else if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(fs::canonicalize(".")
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path))
    }
}

fn commands_use_manual_recording(
    flow_path: &Path,
    commands: &[UiCommand],
    visited: &mut HashSet<PathBuf>,
) -> Result<bool> {
    for command in commands {
        match command {
            UiCommand::StartRecording(_) | UiCommand::StopRecording => return Ok(true),
            UiCommand::RunFlow(relative_path) => {
                let nested_path = resolve_path_from_flow(flow_path, relative_path);
                let nested_flow = parse_ui_flow(&nested_path)?;
                if flow_uses_manual_recording(
                    nested_flow.path.as_path(),
                    nested_flow.commands.as_slice(),
                    visited,
                )? {
                    return Ok(true);
                }
            }
            UiCommand::Repeat { commands, .. } | UiCommand::Retry { commands, .. } => {
                if commands_use_manual_recording(flow_path, commands, visited)? {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}
