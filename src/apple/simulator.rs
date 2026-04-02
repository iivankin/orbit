use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::apple::xcode::{xcodebuild_command, xcrun_command};
use crate::context::ProjectContext;
use crate::manifest::ApplePlatform;
use crate::util::{CliSpinner, command_output, prompt_select, run_command};

#[derive(Debug, Clone, Deserialize)]
pub struct SimctlList {
    pub devices: BTreeMap<String, Vec<SimulatorDevice>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimulatorDevice {
    pub udid: String,
    pub name: String,
    pub state: String,
}

impl SimulatorDevice {
    pub fn is_booted(&self) -> bool {
        self.state.eq_ignore_ascii_case("Booted")
    }

    pub fn selection_label(&self) -> String {
        format!("{} ({})", self.name, self.state)
    }

    fn family_rank(&self, platform: ApplePlatform) -> u8 {
        let name = self.name.as_str();
        match platform {
            ApplePlatform::Ios => {
                if name.starts_with("iPhone") {
                    0
                } else if name.starts_with("iPad") {
                    1
                } else {
                    2
                }
            }
            ApplePlatform::Tvos => {
                if name.starts_with("Apple TV") {
                    0
                } else {
                    1
                }
            }
            ApplePlatform::Visionos => {
                if name.contains("Vision") {
                    0
                } else {
                    1
                }
            }
            ApplePlatform::Watchos => {
                if name.starts_with("Apple Watch") {
                    0
                } else {
                    1
                }
            }
            ApplePlatform::Macos => 0,
        }
    }
}

pub fn select_simulator_device(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<SimulatorDevice> {
    let mut devices = list_available_simulators(project, platform)?;
    if devices.is_empty() && project.resolved_manifest.xcode.is_some() {
        install_missing_platform_runtime(project, platform)?;
        devices = list_available_simulators(project, platform)?;
    }
    if devices.is_empty() {
        bail!("no available {platform} simulators were found");
    }

    let index = if project.app.interactive {
        let options = devices
            .iter()
            .map(SimulatorDevice::selection_label)
            .collect::<Vec<_>>();
        prompt_select("Select a simulator", &options)?
    } else {
        0
    };
    Ok(devices.remove(index))
}

pub fn list_available_simulators(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<Vec<SimulatorDevice>> {
    let mut command = xcrun_command(project.selected_xcode.as_ref());
    command.args(["simctl", "list", "devices", "available", "--json"]);
    let output = command_output(&mut command)?;
    let devices: SimctlList = serde_json::from_str(&output)?;
    let mut flattened = devices
        .devices
        .into_iter()
        .filter(|(runtime, _)| simulator_runtime_matches_platform(runtime, platform))
        .flat_map(|(_, devices)| devices)
        .collect::<Vec<_>>();
    flattened.sort_by(|left, right| compare_simulators(left, right, platform));
    Ok(flattened)
}

fn compare_simulators(
    left: &SimulatorDevice,
    right: &SimulatorDevice,
    platform: ApplePlatform,
) -> Ordering {
    left.family_rank(platform)
        .cmp(&right.family_rank(platform))
        .then_with(|| right.is_booted().cmp(&left.is_booted()))
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.udid.cmp(&right.udid))
}

fn simulator_runtime_matches_platform(runtime_identifier: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios => runtime_identifier.contains(".SimRuntime.iOS-"),
        ApplePlatform::Tvos => runtime_identifier.contains(".SimRuntime.tvOS-"),
        ApplePlatform::Visionos => {
            runtime_identifier.contains(".SimRuntime.xrOS-")
                || runtime_identifier.contains(".SimRuntime.visionOS-")
        }
        ApplePlatform::Watchos => runtime_identifier.contains(".SimRuntime.watchOS-"),
        ApplePlatform::Macos => runtime_identifier.contains(".SimRuntime.macOS-"),
    }
}

fn install_missing_platform_runtime(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<()> {
    let Some(download_name) = runtime_download_platform_name(platform) else {
        return Ok(());
    };

    let selected_xcode = project.selected_xcode.as_ref();
    let label = selected_xcode
        .map(|xcode| xcode.display_name())
        .unwrap_or_else(|| "the selected Xcode".to_owned());
    let spinner = CliSpinner::new(format!(
        "Installing the {} simulator runtime for {}",
        platform, label
    ));
    let result = (|| {
        let download_root = runtime_download_root(project, platform, selected_xcode)?;
        recreate_dir(&download_root)?;

        let mut download = xcodebuild_command(selected_xcode);
        download.args([
            "-downloadPlatform",
            download_name,
            "-exportPath",
            download_root
                .to_str()
                .context("simulator runtime cache path contains invalid UTF-8")?,
        ]);
        run_command(&mut download)?;

        // Export the official runtime DMG first, then let simctl stage, verify, and mount it.
        let disk_image = find_runtime_disk_image(&download_root)?;
        let mut add_runtime = xcrun_command(selected_xcode);
        add_runtime.args([
            "simctl",
            "runtime",
            "add",
            disk_image
                .to_str()
                .context("runtime disk image path contains invalid UTF-8")?,
        ]);
        run_command(&mut add_runtime)
    })();

    match result {
        Ok(()) => {
            spinner.finish_success(format!(
                "Installed the {} simulator runtime for {}.",
                platform, label
            ));
            Ok(())
        }
        Err(error) => {
            spinner.finish_clear();
            Err(error)
        }
    }
}

fn runtime_download_platform_name(platform: ApplePlatform) -> Option<&'static str> {
    match platform {
        ApplePlatform::Ios => Some("iOS"),
        ApplePlatform::Tvos => Some("tvOS"),
        ApplePlatform::Visionos => Some("visionOS"),
        ApplePlatform::Watchos => Some("watchOS"),
        ApplePlatform::Macos => None,
    }
}

fn runtime_download_root(
    project: &ProjectContext,
    platform: ApplePlatform,
    selected_xcode: Option<&crate::apple::xcode::SelectedXcode>,
) -> Result<PathBuf> {
    let xcode_key = selected_xcode
        .map(|xcode| format!("{}-{}", xcode.version, xcode.build_version))
        .unwrap_or_else(|| "selected".to_owned());
    Ok(project
        .app
        .global_paths
        .cache_dir
        .join("simruntimes")
        .join(xcode_key)
        .join(platform.to_string()))
}

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to clear {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

fn find_runtime_disk_image(root: &Path) -> Result<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .find(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("dmg"))
        })
        .with_context(|| {
            format!(
                "`xcodebuild -downloadPlatform` did not export a simulator runtime disk image under {}",
                root.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{SimulatorDevice, compare_simulators};
    use crate::manifest::ApplePlatform;

    fn simulator(name: &str, state: &str, udid: &str) -> SimulatorDevice {
        SimulatorDevice {
            udid: udid.to_owned(),
            name: name.to_owned(),
            state: state.to_owned(),
        }
    }

    #[test]
    fn ios_prefers_iphone_over_booted_ipad() {
        let iphone = simulator("iPhone 17 Pro", "Shutdown", "1");
        let ipad = simulator("iPad Air 11-inch (M4)", "Booted", "2");

        assert!(compare_simulators(&iphone, &ipad, ApplePlatform::Ios).is_lt());
    }

    #[test]
    fn ios_prefers_booted_state_within_same_family() {
        let booted = simulator("iPhone 17 Pro", "Booted", "1");
        let shutdown = simulator("iPhone 17 Pro", "Shutdown", "2");

        assert!(compare_simulators(&booted, &shutdown, ApplePlatform::Ios).is_lt());
    }
}
