use std::cmp::Ordering;
use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::context::ProjectContext;
use crate::manifest::ApplePlatform;
use crate::util::{command_output, prompt_select};

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
    let mut devices = list_available_simulators(platform)?;
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

pub fn list_available_simulators(platform: ApplePlatform) -> Result<Vec<SimulatorDevice>> {
    let output = command_output(std::process::Command::new("xcrun").args([
        "simctl",
        "list",
        "devices",
        "available",
        "--json",
    ]))?;
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
