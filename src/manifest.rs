use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub team_id: Option<String>,
    pub provider_id: Option<String>,
    #[serde(default)]
    pub source_roots: Vec<PathBuf>,
    #[serde(default)]
    pub toolchain: ToolchainManifest,
    pub platforms: BTreeMap<ApplePlatform, PlatformManifest>,
    pub targets: Vec<TargetManifest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolchainManifest {
    pub xcode_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformManifest {
    pub deployment_target: String,
    pub profiles: BTreeMap<String, ProfileManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileManifest {
    pub configuration: String,
    pub distribution: DistributionKind,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetManifest {
    pub name: String,
    pub kind: TargetKind,
    pub bundle_id: String,
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
    pub entitlements: Option<PathBuf>,
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
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XcframeworkDependency {
    pub path: PathBuf,
    pub library: Option<String>,
    #[serde(default = "default_xcframework_embed")]
    pub embed: bool,
}

fn default_xcframework_embed() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub point_identifier: String,
    pub principal_class: String,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, JsonValue>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.platform != "apple" {
            bail!(
                "unsupported manifest platform `{}`; Orbit v2 requires `platform: \"apple\"`",
                self.platform
            );
        }

        if self.platforms.is_empty() {
            bail!("manifest must declare at least one Apple platform");
        }

        if self.targets.is_empty() {
            bail!("manifest must declare at least one target");
        }

        let target_names = self
            .targets
            .iter()
            .map(|target| target.name.as_str())
            .collect::<HashSet<_>>();
        let mut dependents_by_target = HashMap::<&str, Vec<&TargetManifest>>::new();

        for (platform, manifest) in &self.platforms {
            if manifest.deployment_target.trim().is_empty() {
                bail!("platform `{platform}` must declare a deployment_target");
            }
            if manifest.profiles.is_empty() {
                bail!("platform `{platform}` must declare at least one build profile");
            }
        }

        for target in &self.targets {
            if target.sources.is_empty()
                && !matches!(
                    target.kind,
                    TargetKind::Framework | TargetKind::StaticLibrary
                )
            {
                bail!(
                    "target `{}` must declare at least one source root",
                    target.name
                );
            }
            if target.bundle_id.trim().is_empty() {
                bail!("target `{}` must declare a bundle_id", target.name);
            }
            for dependency in &target.dependencies {
                if !target_names.contains(dependency.as_str()) {
                    bail!(
                        "target `{}` depends on unknown target `{dependency}`",
                        target.name
                    );
                }
                dependents_by_target
                    .entry(dependency.as_str())
                    .or_default()
                    .push(target);
            }
            match target.kind {
                TargetKind::AppExtension
                | TargetKind::WatchExtension
                | TargetKind::WidgetExtension => {
                    if target.extension.is_none() {
                        bail!(
                            "target `{}` of kind `{}` must define the `extension` block",
                            target.name,
                            serde_json::to_string(&target.kind).unwrap_or_default()
                        );
                    }
                }
                _ => {}
            }

            if matches!(target.kind, TargetKind::App)
                && target
                    .dependencies
                    .iter()
                    .filter(|dependency| {
                        self.targets.iter().any(|candidate| {
                            candidate.name == **dependency
                                && matches!(candidate.kind, TargetKind::WatchApp)
                        })
                    })
                    .count()
                    > 1
            {
                bail!(
                    "app target `{}` cannot host more than one watch app",
                    target.name
                );
            }
        }

        let target_name_set = self
            .targets
            .iter()
            .map(|target| target.name.as_str())
            .collect::<HashSet<_>>();
        if target_name_set.len() != self.targets.len() {
            bail!("target names must be unique");
        }

        for target in &self.targets {
            match target.kind {
                TargetKind::WatchApp => {
                    let host_apps = dependents_by_target
                        .get(target.name.as_str())
                        .into_iter()
                        .flatten()
                        .filter(|dependent| matches!(dependent.kind, TargetKind::App))
                        .count();
                    if host_apps > 1 {
                        bail!(
                            "watch app target `{}` cannot be hosted by more than one app target",
                            target.name
                        );
                    }
                }
                TargetKind::WatchExtension => {
                    let host_watch_apps = dependents_by_target
                        .get(target.name.as_str())
                        .into_iter()
                        .flatten()
                        .filter(|dependent| matches!(dependent.kind, TargetKind::WatchApp))
                        .count();
                    if host_watch_apps != 1 {
                        bail!(
                            "watch extension target `{}` must be hosted by exactly one watch app target",
                            target.name
                        );
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    pub fn default_platform(&self) -> ApplePlatform {
        *self
            .platforms
            .keys()
            .next()
            .expect("validated manifest has at least one platform")
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

    pub fn profile_for<'a>(
        &'a self,
        platform: ApplePlatform,
        name: &str,
    ) -> Result<&'a ProfileManifest> {
        self.platforms
            .get(&platform)
            .context("platform missing from manifest")?
            .profiles
            .get(name)
            .with_context(|| format!("unknown profile `{name}` for platform `{platform}`"))
    }

    pub fn profile_names(&self, platform: ApplePlatform) -> Result<Vec<String>> {
        let mut names = self
            .platforms
            .get(&platform)
            .context("platform missing from manifest")?
            .profiles
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        Ok(names)
    }

    pub fn selectable_root_targets(&self) -> Vec<&TargetManifest> {
        let mut targets = self
            .targets
            .iter()
            .filter(|target| matches!(target.kind, TargetKind::App | TargetKind::WatchApp))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            targets = self.targets.iter().collect();
        }
        targets
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
        self.source_roots.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{DistributionKind, Manifest};

    fn fixture(path: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    #[test]
    fn loads_example_simulator_manifest() {
        let manifest = Manifest::load(&fixture("examples/ios-simulator-app/orbit.json")).unwrap();
        let profile = manifest
            .profile_for(super::ApplePlatform::Ios, "development")
            .unwrap();
        assert!(matches!(
            profile.distribution,
            DistributionKind::Development
        ));
        assert_eq!(manifest.targets.len(), 1);
    }

    #[test]
    fn sorts_extension_dependencies_before_host_app() {
        let manifest = Manifest::load(&fixture("examples/ios-app-extension/orbit.json")).unwrap();
        let ordered = manifest
            .topological_targets("ExampleExtensionApp")
            .unwrap()
            .into_iter()
            .map(|target| target.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                "TunnelExtension".to_owned(),
                "ExampleExtensionApp".to_owned()
            ]
        );
    }

    #[test]
    fn exposes_sorted_profile_names() {
        let manifest = Manifest::load(&fixture("examples/ios-simulator-app/orbit.json")).unwrap();
        assert_eq!(
            manifest.profile_names(super::ApplePlatform::Ios).unwrap(),
            vec![
                "development".to_owned(),
                "internal".to_owned(),
                "release".to_owned()
            ]
        );
    }

    #[test]
    fn prefers_app_targets_for_root_selection() {
        let manifest = Manifest::load(&fixture("examples/ios-app-extension/orbit.json")).unwrap();
        let target_names = manifest
            .selectable_root_targets()
            .into_iter()
            .map(|target| target.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(target_names, vec!["ExampleExtensionApp".to_owned()]);
    }

    #[test]
    fn sorts_watch_dependencies_inside_companion_graph() {
        let manifest = Manifest::load(&fixture("examples/ios-watch-app/orbit.json")).unwrap();
        let ordered = manifest
            .topological_targets("ExampleCompanionApp")
            .unwrap()
            .into_iter()
            .map(|target| target.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                "ExampleWatchExtension".to_owned(),
                "ExampleWatchApp".to_owned(),
                "ExampleCompanionApp".to_owned(),
            ]
        );
    }

    #[test]
    fn rejects_unhosted_watch_extension_targets() {
        let manifest = serde_json::json!({
            "name": "BrokenWatchApp",
            "version": "0.1.0",
            "platform": "apple",
            "platforms": {
                "watchos": {
                    "deployment_target": "11.0",
                    "profiles": {
                        "development": {
                            "configuration": "debug",
                            "distribution": "development"
                        }
                    }
                }
            },
            "targets": [
                {
                    "name": "OrphanWatchExtension",
                    "kind": "watch-extension",
                    "bundle_id": "dev.orbit.examples.orphan.watchkitextension",
                    "platforms": ["watchos"],
                    "sources": ["Sources/WatchExtension"],
                    "extension": {
                        "point_identifier": "com.apple.watchkit",
                        "principal_class": "WatchExtensionDelegate"
                    }
                }
            ]
        });
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("orbit.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

        let error = Manifest::load(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must be hosted by exactly one watch app target")
        );
    }

    #[test]
    fn loads_additional_platform_and_fixture_manifests() {
        let fixture_paths = [
            "examples/macos-app/orbit.json",
            "examples/tvos-app/orbit.json",
            "examples/visionos-app/orbit.json",
            "examples/mixed-language-app/orbit.json",
            "examples/compiled-resources-app/orbit.json",
            "examples/swiftpm-multi-target-app/orbit.json",
        ];

        for path in fixture_paths {
            let manifest = Manifest::load(&fixture(path)).unwrap();
            assert!(
                !manifest.targets.is_empty(),
                "fixture `{path}` should contain at least one target"
            );
            assert!(
                !manifest.platforms.is_empty(),
                "fixture `{path}` should contain at least one platform"
            );
        }
    }
}
