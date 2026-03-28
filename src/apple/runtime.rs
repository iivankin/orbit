use anyhow::{Context, Result, bail};

use crate::cli::{DistributionArg, TargetPlatform};
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, BuildConfiguration, DistributionKind, ProfileManifest, TargetManifest,
};
use crate::util::prompt_select;

pub fn apple_platform_from_cli(platform: TargetPlatform) -> ApplePlatform {
    match platform {
        TargetPlatform::Ios => ApplePlatform::Ios,
        TargetPlatform::Macos => ApplePlatform::Macos,
        TargetPlatform::Tvos => ApplePlatform::Tvos,
        TargetPlatform::Visionos => ApplePlatform::Visionos,
        TargetPlatform::Watchos => ApplePlatform::Watchos,
    }
}

pub fn distribution_from_cli(distribution: Option<DistributionArg>) -> Option<DistributionKind> {
    distribution.map(|distribution| match distribution {
        DistributionArg::Development => DistributionKind::Development,
        DistributionArg::AdHoc => DistributionKind::AdHoc,
        DistributionArg::AppStore => DistributionKind::AppStore,
        DistributionArg::DeveloperId => DistributionKind::DeveloperId,
        DistributionArg::MacAppStore => DistributionKind::MacAppStore,
    })
}

pub fn resolve_platform(
    project: &ProjectContext,
    requested: Option<ApplePlatform>,
    prompt: &str,
) -> Result<ApplePlatform> {
    if let Some(platform) = requested {
        if project.manifest.platforms.contains_key(&platform) {
            return Ok(platform);
        }
        bail!("platform `{platform}` is not declared in the manifest");
    }

    let mut platforms = project
        .manifest
        .platforms
        .keys()
        .copied()
        .collect::<Vec<_>>();
    if platforms.len() == 1 {
        return platforms
            .pop()
            .context("manifest did not contain any declared platforms");
    }
    if !project.app.interactive {
        bail!(
            "manifest declares multiple platforms; pass --platform ({})",
            platforms
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let labels = platforms
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let index = prompt_select(prompt, &labels)?;
    Ok(platforms[index])
}

pub fn resolve_build_distribution(
    project: &ProjectContext,
    platform: ApplePlatform,
    requested: Option<DistributionKind>,
) -> Result<DistributionKind> {
    let distribution = requested.unwrap_or(DistributionKind::Development);
    project
        .manifest
        .validate_distribution(platform, distribution)?;
    Ok(distribution)
}

pub fn profile_for_build(distribution: DistributionKind, release: bool) -> ProfileManifest {
    ProfileManifest::new(
        if release {
            BuildConfiguration::Release
        } else {
            BuildConfiguration::Debug
        },
        distribution,
    )
}

pub fn profile_for_distribution(distribution: DistributionKind) -> ProfileManifest {
    let configuration = if distribution == DistributionKind::Development {
        BuildConfiguration::Debug
    } else {
        BuildConfiguration::Release
    };
    ProfileManifest::new(configuration, distribution)
}

pub fn profile_for_run() -> ProfileManifest {
    ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development)
}

pub fn build_target_for_platform<'a>(
    project: &'a ProjectContext,
    platform: ApplePlatform,
) -> Result<&'a TargetManifest> {
    project.manifest.default_build_target_for_platform(platform)
}
