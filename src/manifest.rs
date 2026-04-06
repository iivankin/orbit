use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value as JsonValue;

pub use crate::apple::manifest::{
    ApplePlatform, BuildConfiguration, DistributionKind, ExtensionEntry, ExtensionManifest,
    ExtensionRuntime, HooksManifest, IosDeviceFamily, IosInterfaceOrientation,
    IosSupportedOrientationsManifest, IosTargetManifest, PlatformManifest, ProfileManifest,
    PushManifest, QualityManifest, ResolvedManifest, SwiftPackageDependency, SwiftPackageSource,
    TargetKind, TargetManifest, TestFormat, TestTargetManifest, TestsManifest,
    XcframeworkDependency,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ManifestBackend {
    Apple,
    Android,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ManifestSchema {
    AppleAppV1,
}

impl ManifestSchema {
    pub const fn backend(self) -> ManifestBackend {
        match self {
            Self::AppleAppV1 => ManifestBackend::Apple,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AppleAppV1 => crate::apple::manifest::SCHEMA_URL,
        }
    }

    pub const fn file_name(self) -> &'static str {
        match self {
            Self::AppleAppV1 => crate::apple::manifest::SCHEMA_FILENAME,
        }
    }

    fn matches(self, schema: &str) -> bool {
        match self {
            Self::AppleAppV1 => schema_file_name(schema) == crate::apple::manifest::SCHEMA_FILENAME,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SchemaProbe {
    #[serde(rename = "$schema")]
    schema: String,
}

pub fn detect_schema(path: &Path) -> Result<ManifestSchema> {
    detect_schema_with_env(path, None)
}

pub fn detect_schema_with_env(path: &Path, env: Option<&str>) -> Result<ManifestSchema> {
    let manifest = read_manifest_value(path, env)?;
    let probe: SchemaProbe = serde_json::from_value(manifest)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let schema_name = schema_file_name(&probe.schema);
    if ManifestSchema::AppleAppV1.matches(&probe.schema) {
        return Ok(ManifestSchema::AppleAppV1);
    }
    if schema_name.contains("android") || probe.schema.contains("android") {
        bail!(
            "manifest schema `{other}` targets Android, but Android support is not implemented yet",
            other = probe.schema
        );
    }
    bail!(
        "unsupported manifest schema `{}`; expected a schema path or URL ending with `{}`",
        probe.schema,
        crate::apple::manifest::SCHEMA_FILENAME
    )
}

fn schema_file_name(schema: &str) -> &str {
    schema.rsplit(['/', '\\']).next().unwrap_or(schema)
}

pub fn read_manifest_value(path: &Path, env: Option<&str>) -> Result<JsonValue> {
    let mut manifest = read_manifest_file(path)?;
    if let Some(env) = env {
        let overlay_path = overlay_manifest_path(path, env)?;
        let overlay = read_manifest_file(&overlay_path)?;
        merge_manifest_value(&mut manifest, overlay);
    }
    Ok(manifest)
}

pub fn overlay_manifest_path(path: &Path, env: &str) -> Result<PathBuf> {
    let env = env.trim();
    if env.is_empty() {
        bail!("`--env` requires a non-empty value");
    }
    if env.contains(['/', '\\']) {
        bail!("`--env` must not contain path separators");
    }

    let parent = path
        .parent()
        .context("manifest path did not contain a parent directory")?;
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .with_context(|| {
            format!(
                "manifest path `{}` must have a valid UTF-8 file name",
                path.display()
            )
        })?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .with_context(|| {
            format!(
                "manifest path `{}` must have a file extension",
                path.display()
            )
        })?;

    Ok(parent.join(format!("{stem}.{env}.{extension}")))
}

fn read_manifest_file(path: &Path) -> Result<JsonValue> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

fn merge_manifest_value(base: &mut JsonValue, overlay: JsonValue) {
    match (base, overlay) {
        (JsonValue::Object(base_object), JsonValue::Object(overlay_object)) => {
            for (key, overlay_value) in overlay_object {
                if let Some(base_value) = base_object.get_mut(&key) {
                    merge_manifest_value(base_value, overlay_value);
                } else {
                    base_object.insert(key, overlay_value);
                }
            }
        }
        (base_slot, overlay_value) => {
            *base_slot = overlay_value;
        }
    }
}

pub fn installed_schema_path(schema_dir: &Path, schema: ManifestSchema) -> PathBuf {
    schema_dir.join(schema.file_name())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{overlay_manifest_path, read_manifest_value};

    #[test]
    fn reads_base_manifest_without_env_overlay() {
        let temp = tempdir().unwrap();
        let manifest_path = temp.path().join("orbit.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "name": "Base",
                "platforms": { "ios": "18.0" }
            }))
            .unwrap(),
        )
        .unwrap();

        let manifest = read_manifest_value(&manifest_path, None).unwrap();
        assert_eq!(manifest["name"], "Base");
        assert_eq!(manifest["platforms"]["ios"], "18.0");
    }

    #[test]
    fn merges_environment_manifest_recursively() {
        let temp = tempdir().unwrap();
        let manifest_path = temp.path().join("orbit.json");
        let overlay_path = temp.path().join("orbit.stage.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "$schema": "https://orbit.dev/schemas/apple-app.v1.json",
                "name": "Base",
                "platforms": {
                    "ios": "18.0",
                    "macos": "15.0"
                },
                "dependencies": {
                    "BaseOnly": { "path": "Packages/BaseOnly" }
                },
                "info": {
                    "extra": {
                        "Base": "value"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            &overlay_path,
            serde_json::to_vec_pretty(&json!({
                "name": "Stage",
                "platforms": {
                    "ios": "18.2"
                },
                "dependencies": {
                    "StageOnly": { "path": "Packages/StageOnly" }
                },
                "info": {
                    "extra": {
                        "Stage": "value"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let manifest = read_manifest_value(&manifest_path, Some("stage")).unwrap();
        assert_eq!(manifest["name"], "Stage");
        assert_eq!(manifest["platforms"]["ios"], "18.2");
        assert_eq!(manifest["platforms"]["macos"], "15.0");
        assert_eq!(
            manifest["dependencies"]["BaseOnly"]["path"],
            "Packages/BaseOnly"
        );
        assert_eq!(
            manifest["dependencies"]["StageOnly"]["path"],
            "Packages/StageOnly"
        );
        assert_eq!(manifest["info"]["extra"]["Base"], "value");
        assert_eq!(manifest["info"]["extra"]["Stage"], "value");
    }

    #[test]
    fn builds_overlay_path_from_manifest_name() {
        let manifest_path = PathBuf::from("/tmp/project/orbit.json");
        assert_eq!(
            overlay_manifest_path(&manifest_path, "prod").unwrap(),
            PathBuf::from("/tmp/project/orbit.prod.json")
        );
    }
}
