use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use walkdir::WalkDir;

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

pub fn ensure_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!("{} does not have a parent directory", path.display());
    };
    ensure_dir(parent)
}

pub fn read_json_file<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn read_json_file_if_exists<T>(path: &Path) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    if !path.exists() {
        return Ok(None);
    }
    read_json_file(path).map(Some)
}

pub fn write_json_file<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    ensure_parent_dir(path)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

pub fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<()> {
    ensure_dir(destination)?;
    for entry in WalkDir::new(source) {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(source)
            .with_context(|| format!("failed to relativize {}", path.display()))?;
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            ensure_dir(&target)?;
        } else {
            ensure_parent_dir(&target)?;
            fs::copy(path, &target).with_context(|| {
                format!("failed to copy {} to {}", path.display(), target.display())
            })?;
        }
    }
    Ok(())
}

pub fn collect_files_with_extensions(root: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(extension) = entry.path().extension().and_then(OsStr::to_str) else {
            continue;
        };
        if extensions
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

pub fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    ensure_parent_dir(destination)?;
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

pub fn timestamp_slug() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    seconds.to_string()
}

pub fn resolve_path(root: &Path, input: &Path) -> PathBuf {
    if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    }
}

pub fn parse_json_str<T>(text: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(text).map_err(|error| anyhow!(error))
}
