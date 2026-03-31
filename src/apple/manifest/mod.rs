mod authoring;
mod entitlements;
mod normalize;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub const SCHEMA_URL: &str = "https://orbit.dev/schemas/apple-app.v1.json";
pub const SCHEMA_FILENAME: &str = "apple-app.v1.json";

pub use authoring::{
    AppManifest, FormatQualityManifest, HooksManifest, LintQualityManifest, QualityManifest,
    TestFormat, TestTargetManifest, TestsManifest,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedManifest {
    pub name: String,
    pub version: String,
    pub team_id: Option<String>,
    pub provider_id: Option<String>,
    pub hooks: HooksManifest,
    pub tests: TestsManifest,
    pub quality: QualityManifest,
    pub platforms: BTreeMap<ApplePlatform, PlatformManifest>,
    pub targets: Vec<TargetManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformManifest {
    pub deployment_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileManifest {
    pub configuration: BuildConfiguration,
    pub distribution: DistributionKind,
}

impl ProfileManifest {
    pub fn new(configuration: BuildConfiguration, distribution: DistributionKind) -> Self {
        Self {
            configuration,
            distribution,
        }
    }

    pub fn is_debug(&self) -> bool {
        self.configuration == BuildConfiguration::Debug
    }

    pub fn variant_name(&self) -> String {
        format!(
            "{}-{}",
            self.distribution.as_str(),
            self.configuration.as_str()
        )
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildConfiguration {
    Debug,
    Release,
}

impl BuildConfiguration {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildConfiguration::Debug => "debug",
            BuildConfiguration::Release => "release",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplePlatform {
    Ios,
    Macos,
    Tvos,
    Visionos,
    Watchos,
}

impl std::fmt::Display for ApplePlatform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            ApplePlatform::Ios => "ios",
            ApplePlatform::Macos => "macos",
            ApplePlatform::Tvos => "tvos",
            ApplePlatform::Visionos => "visionos",
            ApplePlatform::Watchos => "watchos",
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DistributionKind {
    Development,
    AdHoc,
    AppStore,
    DeveloperId,
    MacAppStore,
}

impl DistributionKind {
    pub fn supports_submit(self) -> bool {
        matches!(
            self,
            DistributionKind::AppStore
                | DistributionKind::DeveloperId
                | DistributionKind::MacAppStore
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DistributionKind::Development => "development",
            DistributionKind::AdHoc => "ad-hoc",
            DistributionKind::AppStore => "app-store",
            DistributionKind::DeveloperId => "developer-id",
            DistributionKind::MacAppStore => "mac-app-store",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetManifest {
    pub name: String,
    pub kind: TargetKind,
    pub bundle_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub build_number: Option<String>,
    #[serde(default)]
    pub platforms: Vec<ApplePlatform>,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<PathBuf>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub frameworks: Vec<String>,
    #[serde(default)]
    pub weak_frameworks: Vec<String>,
    #[serde(default)]
    pub system_libraries: Vec<String>,
    #[serde(default)]
    pub xcframeworks: Vec<XcframeworkDependency>,
    #[serde(default)]
    pub swift_packages: Vec<SwiftPackageDependency>,
    #[serde(default)]
    pub info_plist: BTreeMap<String, JsonValue>,
    #[serde(default)]
    pub ios: Option<IosTargetManifest>,
    pub entitlements: Option<PathBuf>,
    #[serde(default)]
    pub push: Option<PushManifest>,
    #[serde(default)]
    pub extension: Option<ExtensionManifest>,
}

impl TargetManifest {
    pub fn supports_platform(&self, platform: ApplePlatform) -> bool {
        self.platforms.is_empty() || self.platforms.contains(&platform)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetKind {
    App,
    AppExtension,
    Framework,
    StaticLibrary,
    DynamicLibrary,
    Executable,
    WatchApp,
    WatchExtension,
    WidgetExtension,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IosTargetManifest {
    #[serde(default)]
    pub device_families: Option<Vec<IosDeviceFamily>>,
    #[serde(default)]
    pub supported_orientations: Option<IosSupportedOrientationsManifest>,
    #[serde(default)]
    pub required_device_capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub launch_screen: Option<BTreeMap<String, JsonValue>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IosSupportedOrientationsManifest {
    #[serde(default)]
    pub iphone: Option<Vec<IosInterfaceOrientation>>,
    #[serde(default)]
    pub ipad: Option<Vec<IosInterfaceOrientation>>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IosDeviceFamily {
    Iphone,
    Ipad,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IosInterfaceOrientation {
    Portrait,
    PortraitUpsideDown,
    LandscapeLeft,
    LandscapeRight,
}

impl TargetKind {
    pub fn bundle_extension(self) -> &'static str {
        match self {
            TargetKind::App | TargetKind::WatchApp => "app",
            TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
                "appex"
            }
            TargetKind::Framework => "framework",
            TargetKind::StaticLibrary => "a",
            TargetKind::DynamicLibrary => "dylib",
            TargetKind::Executable => "",
        }
    }

    pub fn is_bundle(self) -> bool {
        !matches!(
            self,
            TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Executable
        )
    }

    pub fn is_embeddable(self) -> bool {
        matches!(
            self,
            TargetKind::AppExtension
                | TargetKind::WatchApp
                | TargetKind::WatchExtension
                | TargetKind::WidgetExtension
                | TargetKind::Framework
                | TargetKind::DynamicLibrary
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwiftPackageDependency {
    pub product: String,
    pub source: SwiftPackageSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SwiftPackageSource {
    Path {
        path: PathBuf,
    },
    Git {
        url: String,
        version: Option<String>,
        revision: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XcframeworkDependency {
    pub path: PathBuf,
    pub library: Option<String>,
    #[serde(default)]
    pub embed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushManifest {
    #[serde(default)]
    pub broadcast_for_live_activities: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub point_identifier: String,
    pub principal_class: String,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, JsonValue>,
}

impl ResolvedManifest {
    pub fn load(path: &Path, orbit_dir: &Path) -> Result<Self> {
        let mut manifest = normalize::load_manifest(path, orbit_dir)?;
        crate::apple::lockfile::ensure_lockfile(path, &mut manifest)?;
        Ok(manifest)
    }

    pub fn validate_distribution(
        &self,
        platform: ApplePlatform,
        distribution: DistributionKind,
    ) -> Result<()> {
        let supported = match platform {
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos => {
                matches!(
                    distribution,
                    DistributionKind::Development
                        | DistributionKind::AdHoc
                        | DistributionKind::AppStore
                )
            }
            ApplePlatform::Macos => {
                matches!(
                    distribution,
                    DistributionKind::Development
                        | DistributionKind::DeveloperId
                        | DistributionKind::MacAppStore
                )
            }
        };
        if supported {
            Ok(())
        } else {
            bail!(
                "distribution `{}` is not supported for platform `{platform}`",
                distribution.as_str()
            )
        }
    }

    pub fn default_platform(&self) -> ApplePlatform {
        *self
            .platforms
            .keys()
            .next()
            .expect("validated manifest has at least one platform")
    }

    pub fn default_build_target_for_platform(
        &self,
        platform: ApplePlatform,
    ) -> Result<&TargetManifest> {
        if platform == ApplePlatform::Watchos
            && let Some(target) = self.targets.iter().find(|target| {
                target.kind == TargetKind::WatchApp && target.supports_platform(platform)
            })
        {
            return Ok(target);
        }

        self.targets
            .iter()
            .find(|target| {
                target.kind == TargetKind::App
                    && target.supports_platform(platform)
                    && !target_is_app_clip(self, target)
            })
            .with_context(|| {
                format!("could not find a buildable app target for platform `{platform}`")
            })
    }

    pub fn resolve_target<'a>(&'a self, name: Option<&str>) -> Result<&'a TargetManifest> {
        if let Some(name) = name {
            return self
                .targets
                .iter()
                .find(|target| target.name == name)
                .with_context(|| format!("unknown target `{name}`"));
        }

        self.targets
            .iter()
            .find(|target| matches!(target.kind, TargetKind::App))
            .or_else(|| self.targets.first())
            .context("manifest did not contain any targets")
    }

    pub fn resolve_platform_for_target(
        &self,
        target: &TargetManifest,
        explicit: Option<ApplePlatform>,
    ) -> Result<ApplePlatform> {
        if let Some(platform) = explicit {
            if !self.platforms.contains_key(&platform) {
                bail!("platform `{platform}` is not declared in the manifest");
            }
            if !target.supports_platform(platform) {
                bail!(
                    "target `{}` does not support platform `{platform}`",
                    target.name
                );
            }
            return Ok(platform);
        }

        if let Some(platform) = target
            .platforms
            .iter()
            .copied()
            .find(|platform| self.platforms.contains_key(platform))
        {
            return Ok(platform);
        }

        Ok(self.default_platform())
    }

    pub fn topological_targets<'a>(
        &'a self,
        root_target: &'a str,
    ) -> Result<Vec<&'a TargetManifest>> {
        let by_name = self
            .targets
            .iter()
            .map(|target| (target.name.as_str(), target))
            .collect::<HashMap<_, _>>();
        let mut ordered = Vec::new();
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();

        fn visit<'a>(
            name: &'a str,
            by_name: &HashMap<&'a str, &'a TargetManifest>,
            ordered: &mut Vec<&'a TargetManifest>,
            visiting: &mut HashSet<&'a str>,
            visited: &mut HashSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(name) {
                return Ok(());
            }
            if !visiting.insert(name) {
                bail!("target dependency cycle detected at `{name}`");
            }
            let target = by_name
                .get(name)
                .with_context(|| format!("unknown target `{name}`"))?;
            for dependency in &target.dependencies {
                visit(dependency, by_name, ordered, visiting, visited)?;
            }
            visiting.remove(name);
            visited.insert(name);
            ordered.push(*target);
            Ok(())
        }

        visit(
            root_target,
            &by_name,
            &mut ordered,
            &mut visiting,
            &mut visited,
        )?;
        Ok(ordered)
    }

    pub fn shared_source_roots(&self) -> BTreeSet<PathBuf> {
        BTreeSet::new()
    }
}

fn target_is_app_clip(manifest: &ResolvedManifest, target: &TargetManifest) -> bool {
    target.kind == TargetKind::App
        && manifest.targets.iter().any(|candidate| {
            candidate
                .dependencies
                .iter()
                .any(|dependency| dependency == &target.name)
        })
        && target.bundle_id.starts_with(
            &manifest
                .targets
                .iter()
                .find(|candidate| {
                    candidate
                        .dependencies
                        .iter()
                        .any(|dependency| dependency == &target.name)
                })
                .map(|target| format!("{}.", target.bundle_id))
                .unwrap_or_default(),
        )
}
