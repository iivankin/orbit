use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::UiCommand;
use super::parser::parse_ui_flow;
use crate::context::ProjectContext;
use crate::util::{collect_files_with_extensions, resolve_path};

pub(super) fn collect_ui_flow_paths(
    project: &ProjectContext,
    sources: &[PathBuf],
    selectors: &[String],
) -> Result<Vec<PathBuf>> {
    let mut flows = Vec::new();
    let mut seen = HashSet::new();
    for root in sources {
        let resolved = resolve_path(&project.root, root);
        if !resolved.exists() {
            bail!("declared path `{}` does not exist", resolved.display());
        }
        for path in collect_files_with_extensions(&resolved, &["json"])? {
            let canonical = canonical_or_absolute(&path)?;
            if seen.insert(canonical.clone()) {
                flows.push(canonical);
            }
        }
    }
    flows.sort();
    select_ui_flow_paths(flows.as_slice(), selectors, project.root.as_path())
}

pub(super) fn select_ui_flow_paths(
    flow_paths: &[PathBuf],
    selectors: &[String],
    project_root: &Path,
) -> Result<Vec<PathBuf>> {
    if selectors.is_empty() {
        return Ok(flow_paths.to_vec());
    }

    let descriptors = flow_paths
        .iter()
        .map(|path| FlowDescriptor::from_path(path, project_root))
        .collect::<Result<Vec<_>>>()?;

    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for descriptor in descriptors {
        if selectors
            .iter()
            .any(|selector| descriptor.matches(selector))
            && seen.insert(descriptor.path.clone())
        {
            selected.push(descriptor.path);
        }
    }

    if !selected.is_empty() {
        return Ok(selected);
    }

    let available = flow_paths
        .iter()
        .map(|path| FlowDescriptor::from_path(path, project_root))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|descriptor| descriptor.display_name())
        .collect::<Vec<_>>()
        .join("\n  - ");
    bail!(
        "no UI flow matched selector(s): {}\nAvailable flows:\n  - {}",
        selectors.join(", "),
        available
    );
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

struct FlowDescriptor {
    path: PathBuf,
    absolute: String,
    relative: String,
    file_name: String,
    file_stem: String,
    configured_name: Option<String>,
}

impl FlowDescriptor {
    fn from_path(path: &Path, project_root: &Path) -> Result<Self> {
        let flow = parse_ui_flow(path)?;
        let absolute = path.display().to_string();
        let relative = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .display()
            .to_string();
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("UI flow file name contains invalid UTF-8")?
            .to_owned();
        let file_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .context("UI flow file stem contains invalid UTF-8")?
            .to_owned();
        Ok(Self {
            path: path.to_path_buf(),
            absolute,
            relative,
            file_name,
            file_stem,
            configured_name: flow.config.name,
        })
    }

    fn matches(&self, selector: &str) -> bool {
        self.absolute == selector
            || self.relative == selector
            || self.file_name == selector
            || self.file_stem == selector
            || self
                .configured_name
                .as_ref()
                .is_some_and(|name| name == selector)
    }

    fn display_name(&self) -> String {
        match self.configured_name.as_deref() {
            Some(name) => format!("{} ({name})", self.relative),
            None => self.relative.clone(),
        }
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
