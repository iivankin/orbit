use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::entitlements::{
    materialize_local_macos_development_entitlements, materialize_signing_entitlements,
};
use super::{
    SigningIdentity, SigningMaterial, recover_system_keychain_identity, signing_progress_step,
};
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest, TargetManifest};

#[derive(Debug, Clone, Copy)]
pub(crate) enum SigningStrategy {
    Automatic,
    LocalMacosDevelopment,
}

pub fn prepare_signing(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    profile: &ProfileManifest,
    device_udids: Option<Vec<String>>,
) -> Result<SigningMaterial> {
    prepare_signing_with_strategy(
        project,
        target,
        platform,
        profile,
        device_udids,
        SigningStrategy::Automatic,
    )
}

pub(crate) fn prepare_signing_with_strategy(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    profile: &ProfileManifest,
    device_udids: Option<Vec<String>>,
    strategy: SigningStrategy,
) -> Result<SigningMaterial> {
    if matches!(strategy, SigningStrategy::LocalMacosDevelopment) {
        ensure_local_macos_development_strategy(platform, profile)?;
        return prepare_local_macos_development_signing(project, target);
    }

    if platform == ApplePlatform::Macos
        && profile.distribution == DistributionKind::Development
        && crate::asc::config::load_raw(project)?.is_none()
    {
        return prepare_local_macos_development_signing(project, target);
    }
    prepare_signing_with_embedded_asc(project, target, platform, profile, device_udids)
}

fn ensure_local_macos_development_strategy(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<()> {
    if platform != ApplePlatform::Macos || profile.distribution != DistributionKind::Development {
        bail!("local macOS development signing can only be forced for macOS development builds");
    }
    Ok(())
}

pub fn prepare_distribution_artifact_signing(
    project: &ProjectContext,
    bundle_identifier: &str,
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<super::ArtifactSigningMaterial> {
    if platform != ApplePlatform::Macos || profile.distribution != DistributionKind::DeveloperId {
        bail!("distribution artifact signing is only supported for macOS `developer-id` builds");
    }

    let embedded = crate::asc::config::materialize(project)?;
    let state = asc_sync::bundle::load_state(&embedded.bundle_path).with_context(|| {
        format!(
            "missing ASC signing bundle at {}; run `orbi asc apply` first",
            embedded.bundle_path.display()
        )
    })?;
    state
        .ensure_team(&embedded.parsed.team_id)
        .context("ASC signing bundle team does not match embedded `asc.team_id`")?;

    let profile_kind = asc_sync_profile_kind(platform, profile)?;
    let managed_profile = select_embedded_profile(&state, bundle_identifier, profile_kind, None)?;
    let artifact_identity = resolve_embedded_signing_identity(&state, managed_profile)?;

    Ok(super::ArtifactSigningMaterial {
        signing_identity: artifact_identity.hash,
        keychain_path: artifact_identity.keychain_path,
    })
}

fn prepare_signing_with_embedded_asc(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    profile: &ProfileManifest,
    device_udids: Option<Vec<String>>,
) -> Result<SigningMaterial> {
    let embedded = crate::asc::config::materialize(project)?;
    let state = asc_sync::bundle::load_state(&embedded.bundle_path).with_context(|| {
        format!(
            "missing ASC signing bundle at {}; run `orbi asc apply` first",
            embedded.bundle_path.display()
        )
    })?;
    state
        .ensure_team(&embedded.parsed.team_id)
        .context("ASC signing bundle team does not match embedded `asc.team_id`")?;

    let profile_kind = asc_sync_profile_kind(platform, profile)?;
    let managed_profile = select_embedded_profile(
        &state,
        &target.bundle_id,
        profile_kind,
        device_udids.as_deref(),
    )?;
    let signing_identity = resolve_embedded_signing_identity(&state, managed_profile)?;
    let provisioning_profile_path = installed_profile_path(&managed_profile.uuid)?;
    let entitlements_path = signing_progress_step(
        format!("Preparing entitlements for target `{}`", target.name),
        |path: &Option<PathBuf>| match path {
            Some(path) => format!(
                "Prepared entitlements for target `{}`: {}.",
                target.name,
                path.display()
            ),
            None => format!(
                "No generated entitlements were needed for target `{}`.",
                target.name
            ),
        },
        || materialize_signing_entitlements(project, target, &provisioning_profile_path),
    )?;

    Ok(SigningMaterial {
        signing_identity: signing_identity.hash,
        keychain_path: Some(signing_identity.keychain_path),
        provisioning_profile_path: Some(provisioning_profile_path),
        entitlements_path,
    })
}

fn prepare_local_macos_development_signing(
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<SigningMaterial> {
    let entitlements_path = signing_progress_step(
        format!(
            "Preparing local macOS development entitlements for target `{}`",
            target.name
        ),
        |path: &Option<PathBuf>| match path {
            Some(path) => format!(
                "Prepared local macOS development entitlements for target `{}`: {}.",
                target.name,
                path.display()
            ),
            None => format!(
                "No additional entitlements were needed for local macOS development target `{}`.",
                target.name
            ),
        },
        || materialize_local_macos_development_entitlements(project, target),
    )?;

    Ok(SigningMaterial {
        signing_identity: "-".to_owned(),
        keychain_path: None,
        provisioning_profile_path: None,
        entitlements_path,
    })
}

fn select_embedded_profile<'a>(
    state: &'a asc_sync::state::State,
    bundle_identifier: &str,
    expected_kind: &str,
    requested_device_udids: Option<&[String]>,
) -> Result<&'a asc_sync::state::ManagedProfile> {
    let candidates = state
        .profiles
        .iter()
        .filter(|(_, profile)| profile.kind == expected_kind)
        .filter(|(_, profile)| profile_bundle_identifier(state, profile) == Some(bundle_identifier))
        .filter(|(_, profile)| {
            profile_covers_requested_udids(state, profile, requested_device_udids)
        })
        .map(|(logical_name, profile)| (logical_name.as_str(), profile))
        .collect::<Vec<_>>();

    match candidates.as_slice() {
        [] => bail!(
            "no ASC-managed provisioning profile matched bundle `{bundle_identifier}` and kind `{expected_kind}`; run `orbi asc apply` and `orbi asc signing import`"
        ),
        [(_, profile)] => Ok(*profile),
        _ => bail!(
            "multiple ASC-managed provisioning profiles match bundle `{bundle_identifier}` and kind `{expected_kind}`; keep a single matching profile in `asc.profiles`"
        ),
    }
}

fn profile_bundle_identifier<'a>(
    state: &'a asc_sync::state::State,
    profile: &asc_sync::state::ManagedProfile,
) -> Option<&'a str> {
    state
        .bundle_ids
        .get(&profile.bundle_id)
        .map(|bundle_id| bundle_id.bundle_id.as_str())
}

fn profile_covers_requested_udids(
    state: &asc_sync::state::State,
    profile: &asc_sync::state::ManagedProfile,
    requested_device_udids: Option<&[String]>,
) -> bool {
    let Some(requested_device_udids) = requested_device_udids else {
        return true;
    };
    if requested_device_udids.is_empty() {
        return true;
    }

    let registered_udids = profile
        .devices
        .iter()
        .filter_map(|device_name| state.devices.get(device_name))
        .map(|device| device.udid.as_str())
        .collect::<BTreeSet<_>>();
    requested_device_udids
        .iter()
        .all(|udid| registered_udids.contains(udid.as_str()))
}

fn resolve_embedded_signing_identity(
    state: &asc_sync::state::State,
    profile: &asc_sync::state::ManagedProfile,
) -> Result<SigningIdentity> {
    for certificate_name in &profile.certs {
        let certificate = state.certs.get(certificate_name).with_context(|| {
            format!("ASC profile references unknown certificate `{certificate_name}`")
        })?;
        // ASC certificate display names are not guaranteed to appear in Keychain identity names.
        // The serial number is the stable link between the profile state and the imported cert.
        if let Some(identity) = recover_system_keychain_identity(&certificate.serial_number)? {
            return Ok(identity);
        }
    }

    bail!(
        "no imported signing identity matched ASC profile certificates {}; run `orbi asc signing import`",
        profile.certs.join(", ")
    )
}

fn installed_profile_path(uuid: &str) -> Result<PathBuf> {
    let profiles_dir = asc_sync::system::provisioning_profiles_dir()?;
    let path = profiles_dir.join(format!("{uuid}.mobileprovision"));
    anyhow::ensure!(
        path.exists(),
        "missing provisioning profile {}; run `orbi asc signing import`",
        path.display()
    );
    Ok(path)
}

fn asc_sync_profile_kind(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (ApplePlatform::Ios, DistributionKind::Development) => Ok("IOS_APP_DEVELOPMENT"),
        (ApplePlatform::Ios, DistributionKind::AdHoc) => Ok("IOS_APP_ADHOC"),
        (ApplePlatform::Ios, DistributionKind::AppStore) => Ok("IOS_APP_STORE"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("MAC_APP_DEVELOPMENT"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("MAC_APP_STORE"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("MAC_APP_DIRECT"),
        _ => bail!(
            "embedded `asc` signing does not support `{platform}` with `{}` distribution",
            profile.distribution.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::asc_sync_profile_kind;
    use crate::manifest::{ApplePlatform, BuildConfiguration, DistributionKind, ProfileManifest};

    #[test]
    fn maps_macos_release_distribution_kinds_to_embedded_profile_kinds() {
        assert_eq!(
            asc_sync_profile_kind(
                ApplePlatform::Macos,
                &ProfileManifest::new(BuildConfiguration::Release, DistributionKind::DeveloperId)
            )
            .unwrap(),
            "MAC_APP_DIRECT"
        );
        assert_eq!(
            asc_sync_profile_kind(
                ApplePlatform::Macos,
                &ProfileManifest::new(BuildConfiguration::Release, DistributionKind::MacAppStore)
            )
            .unwrap(),
            "MAC_APP_STORE"
        );
    }

    #[test]
    fn rejects_unsupported_distribution_kind_for_platform() {
        let error = asc_sync_profile_kind(
            ApplePlatform::Macos,
            &ProfileManifest::new(BuildConfiguration::Release, DistributionKind::AppStore),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("embedded `asc` signing does not support")
        );
    }
}
