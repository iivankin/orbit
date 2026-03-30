use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::util::{command_output, ensure_parent_dir};

const AOSKIT_FRAMEWORK_PATH: &str = "/System/Library/PrivateFrameworks/AOSKit.framework";

#[derive(Debug, Clone)]
pub(crate) struct LocalAnisette {
    pub md: String,
    pub md_m: String,
}

#[derive(Debug, Deserialize)]
struct LocalAnisetteJson {
    md: String,
    md_m: String,
}

pub(crate) fn load_local_anisette() -> Result<LocalAnisette> {
    if !Path::new(AOSKIT_FRAMEWORK_PATH).exists() {
        bail!("AOSKit.framework is not available at {AOSKIT_FRAMEWORK_PATH}");
    }

    let helper = ensure_anisette_helper()?;
    let mut command = Command::new(helper);
    let output =
        command_output(&mut command).context("failed to execute native AOSKit anisette helper")?;
    let payload: LocalAnisetteJson = serde_json::from_str(output.trim())
        .context("failed to decode native AOSKit anisette response")?;
    Ok(LocalAnisette {
        md: payload.md,
        md_m: payload.md_m,
    })
}

fn ensure_anisette_helper() -> Result<PathBuf> {
    let executable_path = anisette_helper_root().join("orbit-anisette-helper");
    if executable_path.exists() {
        return Ok(executable_path);
    }

    ensure_parent_dir(&executable_path)?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/apple/anisette_helper.m");
    let mut command = Command::new("clang");
    command
        .arg("-fobjc-arc")
        .arg("-framework")
        .arg("Foundation")
        .arg(&source)
        .arg("-o")
        .arg(&executable_path);
    let _ = command_output(&mut command)?;
    Ok(executable_path)
}

fn anisette_helper_root() -> PathBuf {
    helper_cache_root()
        .join("orbit")
        .join("helpers")
        .join("anisette")
}

fn helper_cache_root() -> PathBuf {
    if let Some(value) = std::env::var_os("ORBIT_CACHE_DIR") {
        return PathBuf::from(value);
    }
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("orbit")
}
