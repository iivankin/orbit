use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::apple::xcode::{SelectedXcode, resolve_requested_xcode_for_app};
use crate::manifest::{ManifestSchema, ResolvedManifest, detect_schema_with_env};
use crate::util::{
    ensure_dir, prompt_select, read_json_file_if_exists, resolve_path, write_json_file,
};

#[derive(Debug, Clone)]
pub struct AppContext {
    pub cwd: PathBuf,
    pub interactive: bool,
    pub verbose: bool,
    pub manifest_env: Option<String>,
    pub global_paths: GlobalPaths,
}

#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub app: AppContext,
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_schema: ManifestSchema,
    pub resolved_manifest: ResolvedManifest,
    pub selected_xcode: Option<SelectedXcode>,
    pub project_paths: ProjectPaths,
}

#[derive(Debug, Clone)]
pub struct GlobalPaths {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub schema_dir: PathBuf,
    pub auth_state_path: PathBuf,
    pub device_cache_path: PathBuf,
    pub keychain_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub orbit_dir: PathBuf,
    pub build_dir: PathBuf,
    pub artifacts_dir: PathBuf,
    pub receipts_dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceCache {
    pub devices: Vec<crate::apple::device::CachedDevice>,
}

impl AppContext {
    pub fn new(non_interactive: bool, verbose: bool, manifest_env: Option<String>) -> Result<Self> {
        let cwd =
            std::env::current_dir().context("failed to resolve the current working directory")?;

        Ok(Self {
            cwd,
            interactive: !non_interactive,
            verbose,
            manifest_env,
            global_paths: resolve_global_paths()?,
        })
    }

    pub fn load_project(&self, requested_manifest: Option<&Path>) -> Result<ProjectContext> {
        let manifest_path = self.resolve_manifest_path(requested_manifest)?;
        let manifest_path = manifest_path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", manifest_path.display()))?;
        let root = manifest_path
            .parent()
            .context("manifest path did not contain a parent directory")?
            .to_path_buf();
        let orbit_dir = root.join(".orbit");
        let build_dir = orbit_dir.join("build");
        let artifacts_dir = orbit_dir.join("artifacts");
        let receipts_dir = orbit_dir.join("receipts");

        ensure_dir(&orbit_dir)?;
        ensure_dir(&build_dir)?;
        ensure_dir(&artifacts_dir)?;
        ensure_dir(&receipts_dir)?;
        let manifest_schema = detect_schema_with_env(&manifest_path, self.manifest_env())?;
        let resolved_manifest =
            ResolvedManifest::load_with_env(&manifest_path, &orbit_dir, self.manifest_env())?;
        let selected_xcode =
            resolve_requested_xcode_for_app(self, resolved_manifest.xcode.as_deref())?;

        Ok(ProjectContext {
            app: self.clone(),
            root,
            manifest_path,
            manifest_schema,
            resolved_manifest,
            selected_xcode,
            project_paths: ProjectPaths {
                orbit_dir,
                build_dir,
                artifacts_dir,
                receipts_dir,
            },
        })
    }

    pub fn read_device_cache(&self) -> Result<DeviceCache> {
        Ok(read_json_file_if_exists(&self.global_paths.device_cache_path)?.unwrap_or_default())
    }

    pub fn write_device_cache(&self, cache: &DeviceCache) -> Result<()> {
        write_json_file(&self.global_paths.device_cache_path, cache)
    }

    pub fn resolve_manifest_path_for_dispatch(
        &self,
        requested_manifest: Option<&Path>,
    ) -> Result<PathBuf> {
        self.resolve_manifest_path(requested_manifest)
    }

    pub fn manifest_env(&self) -> Option<&str> {
        self.manifest_env.as_deref()
    }

    fn resolve_manifest_path(&self, requested_manifest: Option<&Path>) -> Result<PathBuf> {
        if let Some(manifest) = requested_manifest {
            return Ok(resolve_path(&self.cwd, manifest));
        }

        let direct_manifest = self.cwd.join("orbit.json");
        if direct_manifest.exists() {
            return Ok(direct_manifest);
        }

        let mut manifests = Vec::new();
        for entry in WalkDir::new(&self.cwd).max_depth(4) {
            let entry = entry?;
            if entry.file_type().is_file() && entry.file_name() == "orbit.json" {
                manifests.push(entry.into_path());
            }
        }
        manifests.sort();

        match manifests.len() {
            0 => bail!(
                "could not find `orbit.json` under {}; pass --manifest explicitly",
                self.cwd.display()
            ),
            1 => Ok(manifests.remove(0)),
            _ if !self.interactive => bail!(
                "found multiple manifests under {}; pass --manifest explicitly",
                self.cwd.display()
            ),
            _ => {
                let display = manifests
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>();
                let index = prompt_select("Select a manifest", &display)?;
                Ok(manifests.remove(index))
            }
        }
    }
}

fn resolve_global_paths() -> Result<GlobalPaths> {
    let data_dir_override = env_path("ORBIT_DATA_DIR");
    let data_dir = match &data_dir_override {
        Some(path) => path.clone(),
        None => dirs::data_local_dir()
            .context("failed to resolve the user data directory")?
            .join("orbit"),
    };
    let cache_dir = match env_path("ORBIT_CACHE_DIR") {
        Some(path) => path,
        None if data_dir_override.is_some() => data_dir.join("cache"),
        None => dirs::cache_dir()
            .unwrap_or_else(|| data_dir.join("cache"))
            .join("orbit"),
    };
    let schema_dir = match env_path("ORBIT_SCHEMA_DIR") {
        Some(path) => path,
        None => dirs::home_dir()
            .context("failed to resolve the user home directory for Orbit schemas")?
            .join(".orbit")
            .join("schemas"),
    };
    let keychain_path = data_dir.join("orbit.keychain-db");

    ensure_dir(&data_dir)?;
    ensure_dir(&cache_dir)?;
    Ok(GlobalPaths {
        auth_state_path: data_dir.join("auth.json"),
        device_cache_path: data_dir.join("devices.json"),
        data_dir,
        cache_dir,
        schema_dir,
        keychain_path,
    })
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}
