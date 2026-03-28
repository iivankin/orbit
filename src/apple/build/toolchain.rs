use std::path::PathBuf;
use std::process::Command;

use crate::manifest::ApplePlatform;
use crate::util::command_output;
use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DestinationKind {
    Simulator,
    Device,
}

impl DestinationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DestinationKind::Simulator => "simulator",
            DestinationKind::Device => "device",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub platform: ApplePlatform,
    pub destination: DestinationKind,
    pub sdk_name: String,
    pub sdk_path: PathBuf,
    pub deployment_target: String,
    pub architecture: String,
    pub target_triple: String,
}

impl Toolchain {
    pub fn resolve(
        platform: ApplePlatform,
        deployment_target: &str,
        destination: DestinationKind,
    ) -> Result<Self> {
        let sdk_name = match (platform, destination) {
            (ApplePlatform::Ios, DestinationKind::Simulator) => "iphonesimulator",
            (ApplePlatform::Ios, DestinationKind::Device) => "iphoneos",
            (ApplePlatform::Macos, _) => "macosx",
            (ApplePlatform::Tvos, DestinationKind::Simulator) => "appletvsimulator",
            (ApplePlatform::Tvos, DestinationKind::Device) => "appletvos",
            (ApplePlatform::Visionos, DestinationKind::Simulator) => "xrsimulator",
            (ApplePlatform::Visionos, DestinationKind::Device) => "xros",
            (ApplePlatform::Watchos, DestinationKind::Simulator) => "watchsimulator",
            (ApplePlatform::Watchos, DestinationKind::Device) => "watchos",
        }
        .to_owned();

        let sdk_path = command_output(Command::new("xcrun").args([
            "--sdk",
            sdk_name.as_str(),
            "--show-sdk-path",
        ]))?;
        let sdk_path = PathBuf::from(sdk_path.trim());

        let host_architecture = host_architecture()?;
        let architecture = match (platform, destination) {
            (ApplePlatform::Ios, DestinationKind::Device)
            | (ApplePlatform::Tvos, DestinationKind::Device)
            | (ApplePlatform::Visionos, DestinationKind::Device) => "arm64".to_owned(),
            (ApplePlatform::Watchos, DestinationKind::Device) => "arm64_32".to_owned(),
            _ => host_architecture.clone(),
        };
        let target_triple = match (platform, destination) {
            (ApplePlatform::Ios, DestinationKind::Simulator) => {
                format!("{architecture}-apple-ios{deployment_target}-simulator")
            }
            (ApplePlatform::Ios, DestinationKind::Device) => {
                format!("arm64-apple-ios{deployment_target}")
            }
            (ApplePlatform::Macos, _) => format!("{architecture}-apple-macosx{deployment_target}"),
            (ApplePlatform::Tvos, DestinationKind::Simulator) => {
                format!("{architecture}-apple-tvos{deployment_target}-simulator")
            }
            (ApplePlatform::Tvos, DestinationKind::Device) => {
                format!("arm64-apple-tvos{deployment_target}")
            }
            (ApplePlatform::Visionos, DestinationKind::Simulator) => {
                format!("{architecture}-apple-xros{deployment_target}-simulator")
            }
            (ApplePlatform::Visionos, DestinationKind::Device) => {
                format!("arm64-apple-xros{deployment_target}")
            }
            (ApplePlatform::Watchos, DestinationKind::Simulator) => {
                format!("{architecture}-apple-watchos{deployment_target}-simulator")
            }
            (ApplePlatform::Watchos, DestinationKind::Device) => {
                format!("arm64_32-apple-watchos{deployment_target}")
            }
        };

        Ok(Self {
            platform,
            destination,
            sdk_name,
            sdk_path,
            deployment_target: deployment_target.to_owned(),
            architecture,
            target_triple,
        })
    }

    pub fn swiftc(&self) -> Command {
        let mut command = Command::new("xcrun");
        command.args(["--sdk", self.sdk_name.as_str(), "swiftc"]);
        command
    }

    pub fn clang(&self, cpp: bool) -> Command {
        let tool = if cpp { "clang++" } else { "clang" };
        let mut command = Command::new("xcrun");
        command.args(["--sdk", self.sdk_name.as_str(), tool]);
        command
    }

    pub fn libtool(&self) -> Command {
        let mut command = Command::new("xcrun");
        command.args(["--sdk", self.sdk_name.as_str(), "libtool"]);
        command
    }

    pub fn actool_command(&self) -> Command {
        Command::new("xcrun")
    }

    pub fn info_plist_supported_platform(&self) -> &'static str {
        match (self.platform, self.destination) {
            (ApplePlatform::Ios, DestinationKind::Simulator) => "iPhoneSimulator",
            (ApplePlatform::Ios, DestinationKind::Device) => "iPhoneOS",
            (ApplePlatform::Macos, _) => "MacOSX",
            (ApplePlatform::Tvos, DestinationKind::Simulator) => "AppleTVSimulator",
            (ApplePlatform::Tvos, DestinationKind::Device) => "AppleTVOS",
            (ApplePlatform::Visionos, DestinationKind::Simulator) => "XRSimulator",
            (ApplePlatform::Visionos, DestinationKind::Device) => "XROS",
            (ApplePlatform::Watchos, DestinationKind::Simulator) => "WatchSimulator",
            (ApplePlatform::Watchos, DestinationKind::Device) => "WatchOS",
        }
    }

    pub fn actool_platform_name(&self) -> &'static str {
        match (self.platform, self.destination) {
            (ApplePlatform::Ios, DestinationKind::Simulator) => "iphonesimulator",
            (ApplePlatform::Ios, DestinationKind::Device) => "iphoneos",
            (ApplePlatform::Macos, _) => "macosx",
            (ApplePlatform::Tvos, DestinationKind::Simulator) => "appletvsimulator",
            (ApplePlatform::Tvos, DestinationKind::Device) => "appletvos",
            (ApplePlatform::Visionos, DestinationKind::Simulator) => "xrsimulator",
            (ApplePlatform::Visionos, DestinationKind::Device) => "xros",
            (ApplePlatform::Watchos, DestinationKind::Simulator) => "watchsimulator",
            (ApplePlatform::Watchos, DestinationKind::Device) => "watchos",
        }
    }

    pub fn actool_target_device(&self) -> &'static [&'static str] {
        match self.platform {
            ApplePlatform::Ios => &["iphone", "ipad"],
            ApplePlatform::Tvos => &["tv"],
            ApplePlatform::Watchos => &["watch"],
            ApplePlatform::Visionos => &["vision"],
            ApplePlatform::Macos => &["mac"],
        }
    }
}

fn host_architecture() -> Result<String> {
    let output = command_output(Command::new("uname").arg("-m"))?;
    let architecture = output.trim();
    match architecture {
        "arm64" | "x86_64" => Ok(architecture.to_owned()),
        other => bail!("unsupported host architecture `{other}`"),
    }
}
