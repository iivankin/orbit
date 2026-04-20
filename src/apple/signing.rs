mod cleanup;
mod entitlements;
mod local_state;
mod prepare;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::Value as JsonValue;

pub use self::cleanup::{LocalSigningCleanSummary, clean_local_signing_state};
use self::entitlements::materialize_macos_debug_trace_entitlements;
pub use self::entitlements::target_is_app_clip;
#[cfg(test)]
use self::local_state::{
    CertificateOrigin, ManagedCertificate, ManagedProfile, SigningState, team_signing_paths,
};
use self::local_state::{
    SigningIdentity, delete_certificate_files, delete_file_if_exists, delete_p12_password,
    load_state, recover_system_keychain_identity, save_state,
};
pub use self::prepare::{prepare_distribution_artifact_signing, prepare_signing};
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, TargetManifest};
use crate::util::{CliSpinner, ensure_parent_dir};

const P12_PASSWORD_SERVICE: &str = "dev.orbi.cli.codesign-p12";

#[derive(Debug, Clone)]
pub struct SigningMaterial {
    pub signing_identity: String,
    pub keychain_path: Option<PathBuf>,
    pub provisioning_profile_path: Option<PathBuf>,
    pub entitlements_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ArtifactSigningMaterial {
    pub signing_identity: String,
    pub keychain_path: PathBuf,
}

fn signing_progress_step<T, F, G>(
    message: impl Into<String>,
    success_message: G,
    action: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
    G: FnOnce(&T) -> String,
{
    let spinner = CliSpinner::new(message.into());
    match action() {
        Ok(value) => {
            spinner.finish_success(success_message(&value));
            Ok(value)
        }
        Err(error) => {
            spinner.finish_clear();
            Err(error)
        }
    }
}

pub fn sign_bundle(
    platform: ApplePlatform,
    distribution: DistributionKind,
    bundle_path: &Path,
    material: &SigningMaterial,
) -> Result<()> {
    if let Some(provisioning_profile_path) = &material.provisioning_profile_path {
        let embedded_profile = if platform == ApplePlatform::Macos {
            bundle_path
                .join("Contents")
                .join("embedded.provisionprofile")
        } else {
            bundle_path.join("embedded.mobileprovision")
        };
        ensure_parent_dir(&embedded_profile)?;
        fs::copy(provisioning_profile_path, &embedded_profile).with_context(|| {
            format!(
                "failed to embed provisioning profile into {}",
                bundle_path.display()
            )
        })?;
    }

    let mut command = Command::new("codesign");
    command.args(["--force", "--sign"]);
    command.arg(&material.signing_identity);
    if let Some(keychain_path) = &material.keychain_path {
        command.args(["--keychain"]);
        command.arg(keychain_path);
    }
    if platform == ApplePlatform::Macos && distribution == DistributionKind::DeveloperId {
        // Developer ID notarization requires the app binary to opt into hardened runtime.
        command.args(["--options", "runtime"]);
    }
    if let Some(entitlements) = &material.entitlements_path {
        command.args(["--entitlements"]);
        command.arg(entitlements);
    }
    command.arg(bundle_path);
    crate::util::run_command(&mut command)
}

pub fn prepare_macos_bundle_for_debug_tracing(
    project: &ProjectContext,
    target: &TargetManifest,
    bundle_path: &Path,
) -> Result<()> {
    let entitlements_path =
        materialize_macos_debug_trace_entitlements(project, target, bundle_path)?;
    let mut command = Command::new("codesign");
    command.args(["--force", "--sign", "-", "--entitlements"]);
    command.arg(&entitlements_path);
    command.arg(bundle_path);
    crate::util::run_command(&mut command)
}

#[cfg(test)]
fn identifier_name(prefix: &str, identifier: &str) -> String {
    // Keep cleanup fixtures aligned with portal-managed artifact names, which
    // normalize punctuation into word boundaries instead of preserving dots.
    let normalized_identifier = identifier
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' => character,
            _ => ' ',
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!("{prefix} {normalized_identifier}")
}

fn resolve_local_team_id_if_known(project: &ProjectContext) -> Result<Option<String>> {
    // Local signing cleanup is keyed by the embedded ASC config after the hard cutover.
    let manifest =
        crate::manifest::read_manifest_value(&project.manifest_path, project.app.manifest_env())?;
    Ok(manifest
        .get("asc")
        .and_then(JsonValue::as_object)
        .and_then(|asc| asc.get("team_id"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned))
}

#[cfg(test)]
mod tests;
