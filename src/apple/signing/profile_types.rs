use anyhow::{Result, bail};

use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest};

pub(super) fn developer_services_certificate_type(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<Option<&'static str>> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos
            | ApplePlatform::Macos,
            DistributionKind::Development,
        ) => Ok(Some("DEVELOPMENT")),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos
            | ApplePlatform::Macos,
            DistributionKind::AdHoc | DistributionKind::AppStore | DistributionKind::MacAppStore,
        ) => Ok(Some("DISTRIBUTION")),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => {
            Ok(Some("DEVELOPER_ID_APPLICATION"))
        }
        _ => bail!(
            "signing is not implemented for {platform} with {:?}",
            profile.distribution
        ),
    }
}

pub(super) fn certificate_type(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("83Q87W3TGH"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AdHoc | DistributionKind::AppStore,
        ) => Ok("WXV89964HE"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("749Y1QAGU7"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("HXZEUKP0FP"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("W0EURJRMC5"),
        _ => bail!(
            "signing is not implemented for {platform} with {:?}",
            profile.distribution
        ),
    }
}

pub(super) fn asc_certificate_type(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("IOS_DEVELOPMENT"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AdHoc | DistributionKind::AppStore,
        ) => Ok("IOS_DISTRIBUTION"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("MAC_APP_DEVELOPMENT"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("MAC_APP_DISTRIBUTION"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("DEVELOPER_ID_APPLICATION"),
        _ => bail!(
            "App Store Connect API key auth does not support signing for {platform} with {:?}",
            profile.distribution
        ),
    }
}

pub(super) fn profile_type(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("limited"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AdHoc,
        ) => Ok("adhoc"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AppStore,
        ) => Ok("store"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("limited"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("store"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("direct"),
        _ => bail!("provisioning profiles are not implemented for {platform}"),
    }
}

pub(super) fn asc_profile_type(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios | ApplePlatform::Visionos | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("IOS_APP_DEVELOPMENT"),
        (
            ApplePlatform::Ios | ApplePlatform::Visionos | ApplePlatform::Watchos,
            DistributionKind::AdHoc,
        ) => Ok("IOS_APP_ADHOC"),
        (
            ApplePlatform::Ios | ApplePlatform::Visionos | ApplePlatform::Watchos,
            DistributionKind::AppStore,
        ) => Ok("IOS_APP_STORE"),
        (ApplePlatform::Tvos, DistributionKind::Development) => Ok("TVOS_APP_DEVELOPMENT"),
        (ApplePlatform::Tvos, DistributionKind::AdHoc) => Ok("TVOS_APP_ADHOC"),
        (ApplePlatform::Tvos, DistributionKind::AppStore) => Ok("TVOS_APP_STORE"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("MAC_APP_DEVELOPMENT"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("MAC_APP_STORE"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("MAC_APP_DIRECT"),
        _ => bail!(
            "App Store Connect API key auth does not support provisioning profiles for {platform} with {:?}",
            profile.distribution
        ),
    }
}

pub(super) fn installer_certificate_type(profile: &ProfileManifest) -> Result<&'static str> {
    match profile.distribution {
        DistributionKind::MacAppStore => Ok("2PQI8IDXNH"),
        DistributionKind::DeveloperId => Ok("OYVN2GW35E"),
        _ => bail!(
            "installer signing is not implemented for {:?}",
            profile.distribution
        ),
    }
}

pub(super) fn asc_installer_certificate_type(profile: &ProfileManifest) -> Result<&'static str> {
    match profile.distribution {
        DistributionKind::MacAppStore => Ok("MAC_INSTALLER_DISTRIBUTION"),
        DistributionKind::DeveloperId => bail!(
            "App Store Connect API key auth does not expose a Developer ID installer certificate type yet; use Apple ID so Orbit can use Developer Services cloud-managed certificates"
        ),
        _ => bail!(
            "installer signing is not implemented for {:?}",
            profile.distribution
        ),
    }
}

pub(super) fn developer_services_installer_certificate_type(
    profile: &ProfileManifest,
) -> Option<&'static str> {
    match profile.distribution {
        DistributionKind::MacAppStore => Some("MAC_INSTALLER_DISTRIBUTION"),
        DistributionKind::DeveloperId => Some("DEVELOPER_ID_INSTALLER"),
        _ => None,
    }
}

pub(super) fn asc_bundle_id_platform(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Macos => "MAC_OS",
        ApplePlatform::Ios
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => "IOS",
    }
}
