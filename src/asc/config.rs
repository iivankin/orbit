use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use asc_sync::{
    config::{Config, DeviceFamily},
    sync::Workspace,
};
use serde_json::{Map, Value as JsonValue};

use crate::context::ProjectContext;
use crate::util::{read_json_file, write_json_file};

#[derive(Debug, Clone)]
pub(crate) struct EmbeddedAscConfig {
    pub parsed: Config,
    pub raw: JsonValue,
    pub workspace: Workspace,
    pub bundle_path: PathBuf,
}

pub(crate) fn materialize(project: &ProjectContext) -> Result<EmbeddedAscConfig> {
    let raw = load_raw(project)?.context("`orbi asc` requires an `asc` section in orbi.json")?;
    let parsed: Config = serde_json::from_value(raw.clone())
        .context("failed to parse `asc` section from orbi.json")?;
    parsed
        .validate()
        .context("invalid `asc` section in orbi.json")?;
    let workspace_root = project
        .manifest_path
        .parent()
        .context("manifest path did not contain a parent directory")?;
    let workspace = Workspace::new(workspace_root.to_path_buf());
    let bundle_path = workspace.bundle_path.clone();

    Ok(EmbeddedAscConfig {
        parsed,
        raw,
        workspace,
        bundle_path,
    })
}

pub(crate) fn load_raw(project: &ProjectContext) -> Result<Option<JsonValue>> {
    let manifest =
        crate::manifest::read_manifest_value(&project.manifest_path, project.app.manifest_env())?;
    Ok(manifest.get("asc").cloned())
}

pub(crate) fn persist_from_materialized(project: &ProjectContext, asc: JsonValue) -> Result<()> {
    match project.app.manifest_env() {
        Some(_) => persist_overlay_asc(project, asc),
        None => persist_base_asc(project, asc),
    }
}

pub(crate) fn initialize_asc(project: &ProjectContext, asc: JsonValue) -> Result<PathBuf> {
    let manifest_path = active_manifest_path(project)?;
    let mut manifest: JsonValue = read_json_file(&manifest_path)?;
    let object = manifest
        .as_object_mut()
        .context("manifest file must contain a top-level object")?;
    ensure!(
        !object.contains_key("asc"),
        "`{}` already contains an `asc` section",
        manifest_path.display()
    );
    object.insert("asc".to_owned(), asc);
    write_json_file(&manifest_path, &manifest)?;
    Ok(manifest_path)
}

pub(crate) fn active_manifest_path(project: &ProjectContext) -> Result<PathBuf> {
    match project.app.manifest_env() {
        Some(env) => crate::manifest::overlay_manifest_path(&project.manifest_path, env),
        None => Ok(project.manifest_path.clone()),
    }
}

pub(crate) fn upsert_device(
    asc: &mut JsonValue,
    logical_name: &str,
    display_name: &str,
    family: DeviceFamily,
    udid: &str,
) -> Result<()> {
    let root = asc
        .as_object_mut()
        .context("`asc` section must contain a top-level object")?;
    let devices = root
        .entry("devices".to_owned())
        .or_insert_with(|| JsonValue::Object(Map::new()))
        .as_object_mut()
        .context("`asc.devices` must be an object")?;
    devices.insert(
        logical_name.to_owned(),
        serde_json::json!({
            "family": family.to_string(),
            "udid": udid,
            "name": display_name,
        }),
    );
    Ok(())
}

fn persist_base_asc(project: &ProjectContext, asc: JsonValue) -> Result<()> {
    let manifest_path = active_manifest_path(project)?;
    let mut manifest: JsonValue = read_json_file(&manifest_path)?;
    let object = manifest
        .as_object_mut()
        .context("manifest file must contain a top-level object")?;
    object.insert("asc".to_owned(), asc);
    write_json_file(&manifest_path, &manifest)
}

fn persist_overlay_asc(project: &ProjectContext, asc: JsonValue) -> Result<()> {
    let manifest_path = active_manifest_path(project)?;
    let base_asc = load_base_asc(project)?;
    let asc_patch = overlay_patch(base_asc.as_ref(), &asc);

    let mut manifest: JsonValue = read_json_file(&manifest_path)?;
    let object = manifest
        .as_object_mut()
        .context("manifest file must contain a top-level object")?;

    match asc_patch {
        Some(patch) => {
            object.insert("asc".to_owned(), patch);
        }
        None => {
            object.remove("asc");
        }
    }

    write_json_file(&manifest_path, &manifest)
}

fn load_base_asc(project: &ProjectContext) -> Result<Option<JsonValue>> {
    let manifest = crate::manifest::read_manifest_value(&project.manifest_path, None)?;
    Ok(manifest.get("asc").cloned())
}

fn overlay_patch(base: Option<&JsonValue>, target: &JsonValue) -> Option<JsonValue> {
    match base {
        Some(base) => overlay_patch_inner(base, target),
        None => Some(target.clone()),
    }
}

fn overlay_patch_inner(base: &JsonValue, target: &JsonValue) -> Option<JsonValue> {
    if base == target {
        return None;
    }

    match (base, target) {
        (JsonValue::Object(base_object), JsonValue::Object(target_object)) => {
            let mut patch = Map::new();
            for (key, target_value) in target_object {
                let difference = match base_object.get(key) {
                    Some(base_value) => overlay_patch_inner(base_value, target_value),
                    None => Some(target_value.clone()),
                };
                if let Some(difference) = difference {
                    patch.insert(key.clone(), difference);
                }
            }
            if patch.is_empty() {
                None
            } else {
                Some(JsonValue::Object(patch))
            }
        }
        _ => Some(target.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{JsonValue, initialize_asc, persist_from_materialized};
    use crate::context::{AppContext, GlobalPaths, ProjectContext, ProjectPaths};
    use crate::manifest::{
        ApplePlatform, HooksManifest, ManifestSchema, PlatformManifest, QualityManifest,
        ResolvedManifest, TargetKind, TargetManifest, TestsManifest,
    };
    use crate::util::read_json_file;

    fn project_with_env(root: &std::path::Path, manifest_env: Option<&str>) -> ProjectContext {
        let orbi_dir = root.join(".orbi");
        let build_dir = orbi_dir.join("build");
        let artifacts_dir = orbi_dir.join("artifacts");
        let receipts_dir = orbi_dir.join("receipts");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        std::fs::create_dir_all(&receipts_dir).unwrap();

        ProjectContext {
            app: AppContext {
                cwd: root.to_path_buf(),
                interactive: false,
                verbose: false,
                manifest_env: manifest_env.map(ToOwned::to_owned),
                global_paths: GlobalPaths {
                    data_dir: root.join("data"),
                    cache_dir: root.join("cache"),
                    schema_dir: root.join("schemas"),
                },
            },
            root: root.to_path_buf(),
            manifest_path: root.join("orbi.json"),
            manifest_schema: ManifestSchema::AppleAppV1,
            resolved_manifest: ResolvedManifest {
                name: "Example".to_owned(),
                version: "1.0.0".to_owned(),
                xcode: None,
                hooks: HooksManifest::default(),
                tests: TestsManifest::default(),
                quality: QualityManifest::default(),
                platforms: BTreeMap::from([(
                    ApplePlatform::Ios,
                    PlatformManifest {
                        deployment_target: "18.0".to_owned(),
                        universal_binary: false,
                    },
                )]),
                targets: vec![TargetManifest {
                    name: "Example".to_owned(),
                    kind: TargetKind::App,
                    bundle_id: "dev.orbi.example".to_owned(),
                    display_name: None,
                    build_number: None,
                    platforms: vec![ApplePlatform::Ios],
                    sources: Vec::new(),
                    resources: Vec::new(),
                    dependencies: Vec::new(),
                    frameworks: Vec::new(),
                    weak_frameworks: Vec::new(),
                    system_libraries: Vec::new(),
                    xcframeworks: Vec::new(),
                    swift_packages: Vec::new(),
                    info_plist: BTreeMap::new(),
                    ios: None,
                    entitlements: None,
                    push: None,
                    extension: None,
                }],
            },
            selected_xcode: None,
            project_paths: ProjectPaths {
                orbi_dir,
                build_dir,
                artifacts_dir,
                receipts_dir,
            },
        }
    }

    #[test]
    fn initialize_asc_writes_missing_section_once() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let project = project_with_env(root, None);
        std::fs::write(
            &project.manifest_path,
            serde_json::to_vec_pretty(&json!({
                "$schema": crate::apple::manifest::SCHEMA_URL,
                "name": "Example",
                "bundle_id": "dev.orbi.example",
                "version": "1.0.0",
                "build": 1,
                "platforms": { "ios": "18.0" },
                "sources": ["Sources/App"]
            }))
            .unwrap(),
        )
        .unwrap();

        let asc = json!({
            "team_id": "BASETEAM",
            "bundle_ids": {
                "app": {
                    "bundle_id": "dev.orbi.example",
                    "name": "Example",
                    "platform": "ios"
                }
            }
        });

        let manifest_path = initialize_asc(&project, asc.clone()).unwrap();
        assert_eq!(manifest_path, project.manifest_path);
        let manifest: JsonValue = read_json_file(&project.manifest_path).unwrap();
        assert_eq!(manifest["asc"], asc);

        let err = initialize_asc(&project, asc)
            .expect_err("initialize_asc should not overwrite existing asc");
        assert!(
            err.to_string()
                .contains("already contains an `asc` section")
        );
    }

    #[test]
    fn persist_from_materialized_preserves_sparse_overlay_asc() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let project = project_with_env(root, Some("stage"));
        let overlay_path =
            crate::manifest::overlay_manifest_path(&project.manifest_path, "stage").unwrap();
        std::fs::write(
            &project.manifest_path,
            serde_json::to_vec_pretty(&json!({
                "$schema": crate::apple::manifest::SCHEMA_URL,
                "name": "Example",
                "bundle_id": "dev.orbi.example",
                "version": "1.0.0",
                "build": 1,
                "platforms": { "ios": "18.0" },
                "sources": ["Sources/App"],
                "asc": {
                    "team_id": "BASETEAM",
                    "bundle_ids": {
                        "app": {
                            "bundle_id": "dev.orbi.example",
                            "name": "Example",
                            "platform": "ios"
                        }
                    },
                    "certs": {
                        "ios-dev": {
                            "type": "development",
                            "name": "Example Dev"
                        }
                    },
                    "profiles": {
                        "ios-dev": {
                            "name": "Example Dev",
                            "type": "ios_app_development",
                            "bundle_id": "app",
                            "certs": ["ios-dev"]
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &overlay_path,
            serde_json::to_vec_pretty(&json!({
                "asc": {
                    "devices": {
                        "qa-phone": {
                            "family": "ios",
                            "udid": "EXISTING-UDID",
                            "name": "QA iPhone"
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let asc = json!({
            "team_id": "BASETEAM",
            "bundle_ids": {
                "app": {
                    "bundle_id": "dev.orbi.example",
                    "name": "Example",
                    "platform": "ios"
                }
            },
            "certs": {
                "ios-dev": {
                    "type": "development",
                    "name": "Example Dev"
                }
            },
            "profiles": {
                "ios-dev": {
                    "name": "Example Dev",
                    "type": "ios_app_development",
                    "bundle_id": "app",
                    "certs": ["ios-dev"]
                }
            },
            "devices": {
                "qa-phone": {
                    "family": "ios",
                    "udid": "EXISTING-UDID",
                    "name": "QA iPhone"
                },
                "new-phone": {
                    "family": "ios",
                    "udid": "NEW-UDID",
                    "name": "New iPhone"
                }
            }
        });

        persist_from_materialized(&project, asc).unwrap();

        let base_manifest: JsonValue = read_json_file(&project.manifest_path).unwrap();
        let overlay_manifest: JsonValue = read_json_file(&overlay_path).unwrap();
        assert_eq!(base_manifest["asc"]["team_id"], "BASETEAM");
        assert!(overlay_manifest["asc"].get("team_id").is_none());
        assert!(overlay_manifest["asc"].get("bundle_ids").is_none());
        assert!(overlay_manifest["asc"].get("certs").is_none());
        assert!(overlay_manifest["asc"].get("profiles").is_none());
        assert_eq!(
            overlay_manifest["asc"]["devices"]["qa-phone"]["udid"],
            "EXISTING-UDID"
        );
        assert_eq!(
            overlay_manifest["asc"]["devices"]["new-phone"]["udid"],
            "NEW-UDID"
        );
    }

    #[test]
    fn persist_from_materialized_updates_existing_empty_overlay_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let project = project_with_env(root, Some("stage"));
        let overlay_path =
            crate::manifest::overlay_manifest_path(&project.manifest_path, "stage").unwrap();
        std::fs::write(
            &project.manifest_path,
            serde_json::to_vec_pretty(&json!({
                "$schema": crate::apple::manifest::SCHEMA_URL,
                "name": "Example",
                "bundle_id": "dev.orbi.example",
                "version": "1.0.0",
                "build": 1,
                "platforms": { "ios": "18.0" },
                "sources": ["Sources/App"],
                "asc": {
                    "team_id": "BASETEAM",
                    "bundle_ids": {
                        "app": {
                            "bundle_id": "dev.orbi.example",
                            "name": "Example",
                            "platform": "ios"
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &overlay_path,
            serde_json::to_vec_pretty(&json!({})).unwrap(),
        )
        .unwrap();

        let asc = json!({
            "team_id": "BASETEAM",
            "bundle_ids": {
                "app": {
                    "bundle_id": "dev.orbi.example",
                    "name": "Example",
                    "platform": "ios"
                }
            },
            "devices": {
                "new-phone": {
                    "family": "ios",
                    "udid": "NEW-UDID",
                    "name": "New iPhone"
                }
            }
        });

        persist_from_materialized(&project, asc).unwrap();

        let overlay_manifest: JsonValue = read_json_file(&overlay_path).unwrap();
        assert_eq!(
            overlay_manifest["asc"]["devices"]["new-phone"]["udid"],
            "NEW-UDID"
        );
        assert!(overlay_manifest["asc"].get("team_id").is_none());
        assert!(overlay_manifest["asc"].get("bundle_ids").is_none());
    }
}
