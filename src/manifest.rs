use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub use crate::apple::manifest::{
    ApplePlatform, BuildConfiguration, DistributionKind, ExtensionManifest, IosDeviceFamily,
    IosInterfaceOrientation, IosSupportedOrientationsManifest, IosTargetManifest, PlatformManifest,
    ProfileManifest, PushManifest, ResolvedManifest, SwiftPackageDependency, TargetKind,
    TargetManifest, XcframeworkDependency,
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
    pub fn backend(self) -> ManifestBackend {
        match self {
            ManifestSchema::AppleAppV1 => ManifestBackend::Apple,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ManifestSchema::AppleAppV1 => crate::apple::manifest::SCHEMA_URL,
        }
    }

    fn matches(self, schema: &str) -> bool {
        match self {
            ManifestSchema::AppleAppV1 => {
                schema_file_name(schema) == crate::apple::manifest::SCHEMA_FILENAME
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct SchemaProbe {
    #[serde(rename = "$schema")]
    schema: String,
}

pub fn detect_schema(path: &Path) -> Result<ManifestSchema> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let probe: SchemaProbe = serde_json::from_slice(&bytes)
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
