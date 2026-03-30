use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tempfile::tempdir;

use super::BuiltTarget;
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest, TargetKind};
use crate::util::{copy_dir_recursive, copy_file, ensure_dir, resolve_path, run_command};

pub(super) fn export_artifact(
    project: &ProjectContext,
    platform: ApplePlatform,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
) -> Result<std::path::PathBuf> {
    if !matches!(
        built_target.target_kind,
        TargetKind::App | TargetKind::WatchApp
    ) {
        return export_non_app_artifact(project, built_target, explicit_output);
    }
    match profile.distribution {
        DistributionKind::Development => {
            if let Some(output) = explicit_output {
                let output = resolve_path(&project.root, output);
                if built_target.bundle_path != output {
                    remove_existing_path(&output)?;
                    copy_product(&built_target.bundle_path, &output)?;
                    return Ok(output);
                }
            }
            Ok(built_target.bundle_path.clone())
        }
        DistributionKind::AdHoc | DistributionKind::AppStore => {
            let artifact_name = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
                project.project_paths.artifacts_dir.join(format!(
                    "{}-{:?}.ipa",
                    built_target.target_name, profile.distribution
                ))
            });
            let artifact_path = resolve_path(&project.root, &artifact_name);
            if artifact_path.exists() {
                remove_existing_path(&artifact_path)?;
            }
            let payload_dir = tempdir()?;
            let payload_root = payload_dir.path().join("Payload");
            ensure_dir(&payload_root)?;
            let bundle_destination = payload_root.join(
                built_target
                    .bundle_path
                    .file_name()
                    .context("bundle file name missing")?,
            );
            copy_product(&built_target.bundle_path, &bundle_destination)?;
            let mut command = Command::new("ditto");
            command.args([
                "-c",
                "-k",
                "--keepParent",
                payload_root
                    .to_str()
                    .context("payload path contains invalid UTF-8")?,
                artifact_path
                    .to_str()
                    .context("artifact path contains invalid UTF-8")?,
            ]);
            run_command(&mut command)?;
            Ok(artifact_path)
        }
        DistributionKind::DeveloperId | DistributionKind::MacAppStore => {
            export_macos_artifact(project, platform, built_target, explicit_output, profile)
        }
    }
}

pub(super) fn remove_existing_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn export_macos_artifact(
    project: &ProjectContext,
    platform: ApplePlatform,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
) -> Result<std::path::PathBuf> {
    if platform != ApplePlatform::Macos {
        bail!("macOS artifact export was requested for non-macOS platform `{platform}`");
    }
    let artifact_name = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
        project.project_paths.artifacts_dir.join(format!(
            "{}-{:?}.pkg",
            built_target.target_name, profile.distribution
        ))
    });
    let artifact_path = resolve_path(&project.root, &artifact_name);
    remove_existing_path(&artifact_path)?;

    let signing = crate::apple::signing::prepare_package_signing(project, profile)?;
    let mut command = Command::new("productbuild");
    command.arg("--component");
    command.arg(&built_target.bundle_path);
    command.arg("/Applications");
    command.arg("--sign").arg(&signing.signing_identity);
    command.arg("--keychain").arg(&signing.keychain_path);
    command.arg("--timestamp");
    command.arg(&artifact_path);
    run_command(&mut command)?;
    Ok(artifact_path)
}

fn export_non_app_artifact(
    project: &ProjectContext,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
) -> Result<std::path::PathBuf> {
    let output = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
        project.project_paths.artifacts_dir.join(
            built_target
                .bundle_path
                .file_name()
                .unwrap_or_else(|| OsStr::new(built_target.target_name.as_str())),
        )
    });
    let output = resolve_path(&project.root, &output);
    if output != built_target.bundle_path {
        remove_existing_path(&output)?;
        copy_product(&built_target.bundle_path, &output)?;
        return Ok(output);
    }
    Ok(built_target.bundle_path.clone())
}

fn copy_product(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        copy_dir_recursive(source, destination)
    } else {
        copy_file(source, destination)
    }
}
