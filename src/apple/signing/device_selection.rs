use anyhow::{Result, bail};

use super::{
    ApplePlatform, DistributionKind, ManagedProfile, ProjectContext, SigningState, canonical_ids,
};
use crate::util::{prompt_confirm, prompt_multi_select, prompt_select};

pub(super) struct CurrentProfileLookup<'a> {
    pub(super) state: &'a SigningState,
    pub(super) bundle_identifier: &'a str,
    pub(super) profile_kind: &'a str,
    pub(super) certificate_id: &'a str,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum AdHocProfileReuse {
    ReuseCurrent,
    ChooseAgain,
}

pub(super) fn resolve_profile_device_ids<F>(
    project: &ProjectContext,
    distribution: DistributionKind,
    platform: ApplePlatform,
    current_profile_lookup: CurrentProfileLookup<'_>,
    explicit_udids: Option<Vec<String>>,
    target_name: &str,
    resolve_remote_ids: F,
) -> Result<Vec<String>>
where
    F: FnOnce(&[String]) -> Result<Vec<String>>,
{
    if !distribution_requires_devices(distribution) {
        return Ok(Vec::new());
    }

    let selected_udids = resolve_requested_device_udids(
        project,
        distribution,
        platform,
        current_profile_lookup,
        explicit_udids,
    )?;
    super::signing_progress_step(
        format!("Resolving Apple devices for provisioning profile for target `{target_name}`"),
        |device_ids: &Vec<String>| {
            format!(
                "Resolved {} Apple device(s) for target `{target_name}`.",
                device_ids.len()
            )
        },
        || resolve_remote_ids(&selected_udids),
    )
}

fn distribution_requires_devices(distribution: DistributionKind) -> bool {
    matches!(
        distribution,
        DistributionKind::Development | DistributionKind::AdHoc
    )
}

fn resolve_requested_device_udids(
    project: &ProjectContext,
    distribution: DistributionKind,
    platform: ApplePlatform,
    current_profile_lookup: CurrentProfileLookup<'_>,
    explicit_udids: Option<Vec<String>>,
) -> Result<Vec<String>> {
    if let Some(explicit_udids) = explicit_udids.filter(|udids| !udids.is_empty()) {
        return Ok(explicit_udids);
    }

    select_device_udids(
        project,
        distribution,
        platform,
        current_profile_for_target(
            current_profile_lookup.state,
            current_profile_lookup.bundle_identifier,
            current_profile_lookup.profile_kind,
            current_profile_lookup.certificate_id,
        ),
    )
}

fn select_device_udids(
    project: &ProjectContext,
    distribution: DistributionKind,
    platform: ApplePlatform,
    current_profile: Option<&ManagedProfile>,
) -> Result<Vec<String>> {
    let devices = ensure_registered_devices(project, platform)?;
    if devices.is_empty() {
        bail!(
            "no registered Apple devices found for {platform}; run `orbit apple device register` first"
        );
    }

    if let Some(selected_udids) = current_macos_device_udids(platform, &devices)? {
        return Ok(selected_udids);
    }

    if !project.app.interactive {
        return Ok(devices.iter().map(|device| device.udid.clone()).collect());
    }

    if matches!(distribution, DistributionKind::AdHoc) {
        if let Some(current_profile) = current_profile {
            let provisioned_udids = profile_udids(current_profile, &devices);
            if !provisioned_udids.is_empty() {
                let registered_udids = devices
                    .iter()
                    .map(|device| device.udid.clone())
                    .collect::<Vec<_>>();
                if same_udid_set(&registered_udids, &provisioned_udids) {
                    match prompt_ad_hoc_profile_reuse(&provisioned_udids, &devices)? {
                        AdHocProfileReuse::ReuseCurrent => return Ok(provisioned_udids),
                        AdHocProfileReuse::ChooseAgain => {}
                    }
                } else {
                    let missing = missing_registered_devices(&devices, &provisioned_udids);
                    if !missing.is_empty() {
                        println!(
                            "warning: the current ad-hoc provisioning profile is missing the following devices:"
                        );
                        for device in &missing {
                            println!("- {}", format_cached_device_label(device));
                        }
                        if !prompt_confirm(
                            "Would you like to choose the devices to provision again?",
                            true,
                        )? {
                            return Ok(provisioned_udids);
                        }
                    }
                }
            }
        }
        return prompt_ad_hoc_device_selection(&devices, current_profile);
    }

    let labels = devices
        .iter()
        .map(format_cached_device_label)
        .collect::<Vec<_>>();
    let index = prompt_select("Select a device to provision", &labels)?;
    Ok(vec![devices[index].udid.clone()])
}

fn current_macos_device_udids(
    platform: ApplePlatform,
    devices: &[crate::apple::device::CachedDevice],
) -> Result<Option<Vec<String>>> {
    if platform != ApplePlatform::Macos {
        return Ok(None);
    }

    let Ok(current_udid) =
        crate::apple::device::current_machine_provisioning_udid(crate::cli::DevicePlatform::MacOs)
    else {
        return Ok(None);
    };

    Ok(devices
        .iter()
        .find(|device| device.udid == current_udid)
        .map(|device| vec![device.udid.clone()]))
}

fn prompt_ad_hoc_profile_reuse(
    provisioned_udids: &[String],
    devices: &[crate::apple::device::CachedDevice],
) -> Result<AdHocProfileReuse> {
    loop {
        let options = [
            "Yes",
            "Show devices and ask me again",
            "No, let me choose devices again",
        ];
        match prompt_select(
            "All your registered devices are present in the provisioning profile. Would you like to reuse it?",
            &options,
        )? {
            0 => return Ok(AdHocProfileReuse::ReuseCurrent),
            1 => {
                println!("Devices registered in the provisioning profile:");
                for device in devices {
                    if provisioned_udids.iter().any(|udid| udid == &device.udid) {
                        println!("- {}", format_cached_device_label(device));
                    }
                }
            }
            2 => return Ok(AdHocProfileReuse::ChooseAgain),
            _ => unreachable!("prompt_select returned an out-of-range index"),
        }
    }
}

fn prompt_ad_hoc_device_selection(
    devices: &[crate::apple::device::CachedDevice],
    current_profile: Option<&ManagedProfile>,
) -> Result<Vec<String>> {
    let preselected_udids = current_profile
        .map(|profile| profile_udids(profile, devices))
        .unwrap_or_default();
    let defaults = if preselected_udids.is_empty() {
        vec![true; devices.len()]
    } else {
        devices
            .iter()
            .map(|device| preselected_udids.iter().any(|udid| udid == &device.udid))
            .collect::<Vec<_>>()
    };
    let labels = devices
        .iter()
        .map(format_cached_device_label)
        .collect::<Vec<_>>();
    let selections = prompt_multi_select(
        "Select devices for the ad-hoc build",
        &labels,
        Some(&defaults),
    )?;
    if selections.is_empty() {
        bail!("select at least one device for an ad-hoc provisioning profile");
    }
    Ok(selections
        .into_iter()
        .map(|index| devices[index].udid.clone())
        .collect())
}

fn ensure_registered_devices(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<Vec<crate::apple::device::CachedDevice>> {
    let mut attempted_current_macos_registration = false;
    loop {
        let cache = crate::apple::device::refresh_cache(&project.app)?;
        let devices = cache
            .devices
            .into_iter()
            .filter(|device| cached_device_matches_platform(&device.platform, platform))
            .collect::<Vec<_>>();
        if platform == ApplePlatform::Macos
            && !attempted_current_macos_registration
            && should_auto_register_current_macos_device(&devices)?
        {
            // Xcode silently provisions the current Mac when a local macOS signing flow needs it.
            // Orbit should do the same instead of failing with a manual device-registration error.
            crate::apple::device::register_device(
                &project.app,
                &crate::cli::RegisterDeviceArgs {
                    name: None,
                    udid: None,
                    platform: crate::cli::DevicePlatform::MacOs,
                    current_machine: true,
                },
            )?;
            attempted_current_macos_registration = true;
            continue;
        }
        if !devices.is_empty() {
            return Ok(devices);
        }
        if !project.app.interactive {
            return Ok(devices);
        }
        if !prompt_confirm(
            &format!(
                "You don't have any registered Apple devices for {platform}. Would you like to register one now?"
            ),
            true,
        )? {
            return Ok(devices);
        }
        crate::apple::device::register_device(
            &project.app,
            &crate::cli::RegisterDeviceArgs {
                name: None,
                udid: None,
                platform: match platform {
                    ApplePlatform::Macos => crate::cli::DevicePlatform::MacOs,
                    _ => crate::cli::DevicePlatform::Ios,
                },
                current_machine: matches!(platform, ApplePlatform::Macos),
            },
        )?;
    }
}

fn should_auto_register_current_macos_device(
    devices: &[crate::apple::device::CachedDevice],
) -> Result<bool> {
    let current_udid = match crate::apple::device::current_machine_provisioning_udid(
        crate::cli::DevicePlatform::MacOs,
    ) {
        Ok(udid) => udid,
        Err(_) => return Ok(false),
    };
    Ok(!devices.iter().any(|device| device.udid == current_udid))
}

pub(super) fn current_profile_for_target<'a>(
    state: &'a SigningState,
    bundle_identifier: &str,
    profile_kind: &str,
    certificate_id: &str,
) -> Option<&'a ManagedProfile> {
    state.profiles.iter().rev().find(|profile| {
        profile.bundle_id == bundle_identifier
            && profile.profile_type == profile_kind
            && profile.path.exists()
            && profile
                .certificate_ids
                .iter()
                .any(|candidate| candidate == certificate_id)
    })
}

pub(super) fn profile_udids(
    profile: &ManagedProfile,
    devices: &[crate::apple::device::CachedDevice],
) -> Vec<String> {
    devices
        .iter()
        .filter(|device| profile.device_ids.iter().any(|id| id == &device.id))
        .map(|device| device.udid.clone())
        .collect()
}

pub(super) fn same_udid_set(left: &[String], right: &[String]) -> bool {
    canonical_ids(left) == canonical_ids(right)
}

pub(super) fn missing_registered_devices<'a>(
    devices: &'a [crate::apple::device::CachedDevice],
    provisioned_udids: &[String],
) -> Vec<&'a crate::apple::device::CachedDevice> {
    devices
        .iter()
        .filter(|device| !provisioned_udids.iter().any(|udid| udid == &device.udid))
        .collect()
}

pub(super) fn format_cached_device_label(device: &crate::apple::device::CachedDevice) -> String {
    let details = format_cached_device_details(device);
    let mut label = device.udid.clone();
    if !details.is_empty() {
        label.push(' ');
        label.push_str(&details);
    }
    if !device.name.is_empty() {
        label.push_str(&format!(" ({})", device.name));
    }
    if let Some(created_at) = device.created_at.as_deref() {
        label.push_str(&format!(" (created at: {created_at})"));
    }
    label
}

fn format_cached_device_details(device: &crate::apple::device::CachedDevice) -> String {
    let mut details = Vec::new();
    if let Some(device_class) = device.device_class.as_deref() {
        details.push(device_class.replace('_', " "));
    }
    if let Some(model) = device.model.as_deref()
        && details.iter().all(|detail| detail != model)
    {
        details.push(model.to_owned());
    }
    if details.is_empty() {
        String::new()
    } else {
        format!("({})", details.join(" "))
    }
}

fn cached_device_matches_platform(device_platform: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios | ApplePlatform::Visionos => device_platform == "IOS",
        ApplePlatform::Tvos => device_platform == "TVOS",
        ApplePlatform::Watchos => device_platform == "WATCH" || device_platform == "WATCHOS",
        ApplePlatform::Macos => {
            device_platform == "MAC_OS"
                || device_platform == "MACOS"
                || device_platform == "UNIVERSAL"
        }
    }
}
