use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::manifest::ManifestSchema;
use crate::util::{ensure_dir, ensure_parent_dir, resolve_path, write_json_file};

use super::naming::swift_type_name;

const DEFAULT_VERSION: &str = "1.0.0";
const DEFAULT_BUILD: u64 = 1;
const DEFAULT_SOURCE_DIR: &str = "Sources/App";
const DEFAULT_RESOURCES_DIR: &str = "Resources";
const DEFAULT_WATCH_APP_SOURCE_DIR: &str = "Sources/WatchApp";
const DEFAULT_WATCH_EXTENSION_SOURCE_DIR: &str = "Sources/WatchExtension";
const HOME_VIEW_NAME: &str = "HomeView";

const APPLE_TEMPLATE_CHOICES: [TemplateChoice; 6] = [
    TemplateChoice {
        kind: InitTemplate::Ios,
        label: "iOS app",
        description: "Single-target SwiftUI iPhone/iPad app",
    },
    TemplateChoice {
        kind: InitTemplate::Macos,
        label: "macOS app",
        description: "Single-target SwiftUI Mac app",
    },
    TemplateChoice {
        kind: InitTemplate::AppleMultiplatform,
        label: "Apple multiplatform app",
        description: "Shared SwiftUI app for iOS and macOS",
    },
    TemplateChoice {
        kind: InitTemplate::IosWatchCompanion,
        label: "iOS app with watch companion",
        description: "Host iOS app plus watch app and watch extension",
    },
    TemplateChoice {
        kind: InitTemplate::Tvos,
        label: "tvOS app",
        description: "Single-target SwiftUI tvOS app",
    },
    TemplateChoice {
        kind: InitTemplate::Visionos,
        label: "visionOS app",
        description: "Single-target SwiftUI visionOS app",
    },
];

const IOS_PLATFORMS: [ManifestPlatform; 1] = [ManifestPlatform::new("ios", "18.0")];
const MACOS_PLATFORMS: [ManifestPlatform; 1] = [ManifestPlatform::new("macos", "15.0")];
const APPLE_MULTIPLATFORM_PLATFORMS: [ManifestPlatform; 2] = [
    ManifestPlatform::new("ios", "18.0"),
    ManifestPlatform::new("macos", "15.0"),
];
const IOS_WATCH_PLATFORMS: [ManifestPlatform; 2] = [
    ManifestPlatform::new("ios", "18.0"),
    ManifestPlatform::new("watchos", "11.0"),
];
const TVOS_PLATFORMS: [ManifestPlatform; 1] = [ManifestPlatform::new("tvos", "18.0")];
const VISIONOS_PLATFORMS: [ManifestPlatform; 1] = [ManifestPlatform::new("visionos", "2.0")];

const IOS_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &IOS_PLATFORMS,
    "Orbit is ready for iOS",
    "Edit Sources/App/HomeView.swift, then launch the simulator again.",
    &["orbit run --platform ios --simulator"],
);
const MACOS_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &MACOS_PLATFORMS,
    "Orbit is ready for macOS",
    "Edit Sources/App/HomeView.swift, then relaunch the app from Orbit.",
    &["orbit run --platform macos"],
);
const APPLE_MULTIPLATFORM_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &APPLE_MULTIPLATFORM_PLATFORMS,
    "Orbit is ready for iOS and macOS",
    "Edit one shared SwiftUI surface and run either platform from Orbit.",
    &[
        "orbit run --platform ios --simulator",
        "orbit run --platform macos",
    ],
);
const TVOS_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &TVOS_PLATFORMS,
    "Orbit is ready for tvOS",
    "Edit Sources/App/HomeView.swift, then relaunch the Apple TV simulator.",
    &["orbit run --platform tvos --simulator"],
);
const VISIONOS_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &VISIONOS_PLATFORMS,
    "Orbit is ready for visionOS",
    "Edit Sources/App/HomeView.swift, then relaunch the visionOS simulator.",
    &["orbit run --platform visionos --simulator"],
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InitEcosystem {
    Apple,
}

impl InitEcosystem {
    pub(super) const fn manifest_schema(self) -> ManifestSchema {
        match self {
            Self::Apple => ManifestSchema::AppleAppV1,
        }
    }

    pub(super) const fn template_choices(self) -> &'static [TemplateChoice] {
        match self {
            Self::Apple => &APPLE_TEMPLATE_CHOICES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InitTemplate {
    Ios,
    Macos,
    AppleMultiplatform,
    IosWatchCompanion,
    Tvos,
    Visionos,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TemplateChoice {
    pub(super) kind: InitTemplate,
    pub(super) label: &'static str,
    pub(super) description: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InitAnswers {
    pub(super) ecosystem: InitEcosystem,
    pub(super) name: String,
    pub(super) bundle_id: String,
    pub(super) template: InitTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScaffoldPlan {
    manifest: JsonValue,
    directories: Vec<PathBuf>,
    files: Vec<GeneratedFile>,
    pub(super) next_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneratedFile {
    path: PathBuf,
    contents: String,
}

#[derive(Debug, Clone, Copy)]
struct ManifestPlatform {
    key: &'static str,
    version: &'static str,
}

impl ManifestPlatform {
    const fn new(key: &'static str, version: &'static str) -> Self {
        Self { key, version }
    }
}

#[derive(Debug, Clone, Copy)]
struct AppTemplateSpec {
    platforms: &'static [ManifestPlatform],
    home_view_title: &'static str,
    home_view_detail: &'static str,
    next_commands: &'static [&'static str],
}

impl AppTemplateSpec {
    const fn new(
        platforms: &'static [ManifestPlatform],
        home_view_title: &'static str,
        home_view_detail: &'static str,
        next_commands: &'static [&'static str],
    ) -> Self {
        Self {
            platforms,
            home_view_title,
            home_view_detail,
            next_commands,
        }
    }
}

pub(super) fn scaffold_plan(answers: &InitAnswers, schema_reference: &str) -> ScaffoldPlan {
    match answers.template {
        InitTemplate::Ios => app_template_plan(answers, schema_reference, &IOS_APP_TEMPLATE),
        InitTemplate::Macos => app_template_plan(answers, schema_reference, &MACOS_APP_TEMPLATE),
        InitTemplate::AppleMultiplatform => {
            app_template_plan(answers, schema_reference, &APPLE_MULTIPLATFORM_APP_TEMPLATE)
        }
        InitTemplate::IosWatchCompanion => watch_companion_plan(answers, schema_reference),
        InitTemplate::Tvos => app_template_plan(answers, schema_reference, &TVOS_APP_TEMPLATE),
        InitTemplate::Visionos => {
            app_template_plan(answers, schema_reference, &VISIONOS_APP_TEMPLATE)
        }
    }
}

pub(super) fn create_scaffold(
    project_root: &Path,
    manifest_path: &Path,
    plan: &ScaffoldPlan,
) -> Result<()> {
    let directories = plan
        .directories
        .iter()
        .map(|path| resolve_path(project_root, path))
        .collect::<Vec<_>>();
    let files = plan
        .files
        .iter()
        .map(|file| (resolve_path(project_root, &file.path), file))
        .collect::<Vec<_>>();

    for directory in &directories {
        if directory.exists() && !directory.is_dir() {
            bail!(
                "cannot create directory at {} because a file already exists there",
                directory.display()
            );
        }
    }
    for (path, _) in &files {
        if path.exists() {
            bail!(
                "refusing to overwrite existing scaffold file {}",
                path.display()
            );
        }
    }

    write_json_file(manifest_path, &plan.manifest)?;
    for directory in directories {
        ensure_dir(&directory)?;
    }
    for (path, file) in files {
        ensure_parent_dir(&path)?;
        fs::write(&path, &file.contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    ensure_gitignore_entry(project_root, ".bsp/")?;

    Ok(())
}

fn app_template_plan(
    answers: &InitAnswers,
    schema_reference: &str,
    template: &AppTemplateSpec,
) -> ScaffoldPlan {
    let swift_name = swift_type_name(&answers.name);
    ScaffoldPlan {
        manifest: app_manifest(answers, schema_reference, template.platforms),
        directories: base_directories(),
        files: vec![
            generated_file(
                "Sources/App/App.swift",
                app_file_contents(&format!("{swift_name}App"), HOME_VIEW_NAME),
            ),
            generated_file(
                "Sources/App/HomeView.swift",
                home_view_file_contents(
                    HOME_VIEW_NAME,
                    template.home_view_title,
                    template.home_view_detail,
                ),
            ),
        ],
        next_commands: template
            .next_commands
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

fn watch_companion_plan(answers: &InitAnswers, schema_reference: &str) -> ScaffoldPlan {
    let swift_name = swift_type_name(&answers.name);
    ScaffoldPlan {
        manifest: json!({
            "$schema": schema_reference,
            "name": answers.name,
            "bundle_id": answers.bundle_id,
            "version": DEFAULT_VERSION,
            "build": DEFAULT_BUILD,
            "platforms": platform_manifest(&IOS_WATCH_PLATFORMS),
            "sources": [DEFAULT_SOURCE_DIR],
            "resources": [DEFAULT_RESOURCES_DIR],
            "watch": {
                "sources": [DEFAULT_WATCH_APP_SOURCE_DIR],
                "extension": {
                    "sources": [DEFAULT_WATCH_EXTENSION_SOURCE_DIR],
                    "entry": {
                        "class": "WatchExtensionDelegate"
                    }
                }
            }
        }),
        directories: [
            DEFAULT_SOURCE_DIR,
            DEFAULT_RESOURCES_DIR,
            DEFAULT_WATCH_APP_SOURCE_DIR,
            DEFAULT_WATCH_EXTENSION_SOURCE_DIR,
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect(),
        files: vec![
            generated_file(
                "Sources/App/App.swift",
                app_file_contents(&format!("{swift_name}App"), "PhoneHomeView"),
            ),
            generated_file(
                "Sources/App/PhoneHomeView.swift",
                home_view_file_contents(
                    "PhoneHomeView",
                    "Orbit host app",
                    "Edit the host iOS app here, then run the iPhone simulator again.",
                ),
            ),
            generated_file(
                "Sources/WatchApp/App.swift",
                app_file_contents(&format!("{swift_name}WatchApp"), "WatchHomeView"),
            ),
            generated_file(
                "Sources/WatchApp/WatchHomeView.swift",
                home_view_file_contents(
                    "WatchHomeView",
                    "Orbit watch companion",
                    "Edit the watch UI here, then launch the watch simulator from Orbit.",
                ),
            ),
            generated_file(
                "Sources/WatchExtension/Extension.swift",
                watch_extension_file_contents(),
            ),
        ],
        next_commands: vec![
            "orbit run --platform ios --simulator".to_owned(),
            "orbit run --platform watchos --simulator".to_owned(),
        ],
    }
}

fn app_manifest(
    answers: &InitAnswers,
    schema_reference: &str,
    platforms: &[ManifestPlatform],
) -> JsonValue {
    json!({
        "$schema": schema_reference,
        "name": answers.name,
        "bundle_id": answers.bundle_id,
        "version": DEFAULT_VERSION,
        "build": DEFAULT_BUILD,
        "platforms": platform_manifest(platforms),
        "sources": [DEFAULT_SOURCE_DIR],
        "resources": [DEFAULT_RESOURCES_DIR]
    })
}

fn platform_manifest(platforms: &[ManifestPlatform]) -> JsonValue {
    JsonValue::Object(
        platforms
            .iter()
            .map(|platform| {
                (
                    platform.key.to_owned(),
                    JsonValue::String(platform.version.to_owned()),
                )
            })
            .collect::<JsonMap<_, _>>(),
    )
}

fn base_directories() -> Vec<PathBuf> {
    [DEFAULT_SOURCE_DIR, DEFAULT_RESOURCES_DIR]
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

fn generated_file(path: &str, contents: String) -> GeneratedFile {
    GeneratedFile {
        path: PathBuf::from(path),
        contents,
    }
}

fn ensure_gitignore_entry(project_root: &Path, entry: &str) -> Result<()> {
    let gitignore_path = project_root.join(".gitignore");
    let existing = if gitignore_path.exists() {
        fs::read_to_string(&gitignore_path)
            .with_context(|| format!("failed to read {}", gitignore_path.display()))?
    } else {
        String::new()
    };

    if existing
        .lines()
        .map(str::trim)
        .any(|line| line == entry || line == entry.trim_end_matches('/'))
    {
        return Ok(());
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(entry);
    updated.push('\n');

    fs::write(&gitignore_path, updated)
        .with_context(|| format!("failed to write {}", gitignore_path.display()))
}

fn app_file_contents(app_type_name: &str, root_view_name: &str) -> String {
    format!(
        "import SwiftUI\n\n@main\nstruct {app_type_name}: App {{\n    var body: some Scene {{\n        WindowGroup {{\n            {root_view_name}()\n        }}\n    }}\n}}\n"
    )
}

fn home_view_file_contents(view_name: &str, title: &str, detail: &str) -> String {
    format!(
        "import SwiftUI\n\nstruct {view_name}: View {{\n    var body: some View {{\n        VStack(spacing: 16) {{\n            Image(systemName: \"sparkles\")\n                .font(.system(size: 44))\n                .foregroundStyle(.tint)\n            Text(\"{title}\")\n                .font(.largeTitle.bold())\n            Text(\"{detail}\")\n                .multilineTextAlignment(.center)\n                .foregroundStyle(.secondary)\n        }}\n        .padding(32)\n    }}\n}}\n"
    )
}

fn watch_extension_file_contents() -> String {
    "import Foundation\nimport WatchKit\n\n// Orbit uses this principal class from `watch.extension.entry.class`.\nfinal class WatchExtensionDelegate: NSObject, WKApplicationDelegate {}\n".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::read_json_file;

    #[test]
    fn gitignore_appends_bsp_entry_once() {
        let temp = tempfile::tempdir().unwrap();
        let gitignore_path = temp.path().join(".gitignore");
        std::fs::write(&gitignore_path, "DerivedData/\n").unwrap();

        ensure_gitignore_entry(temp.path(), ".bsp/").unwrap();
        ensure_gitignore_entry(temp.path(), ".bsp/").unwrap();

        let contents = std::fs::read_to_string(&gitignore_path).unwrap();
        assert_eq!(contents, "DerivedData/\n.bsp/\n");
    }

    #[test]
    fn gitignore_accepts_existing_bsp_without_trailing_slash() {
        let temp = tempfile::tempdir().unwrap();
        let gitignore_path = temp.path().join(".gitignore");
        std::fs::write(&gitignore_path, ".bsp\n").unwrap();

        ensure_gitignore_entry(temp.path(), ".bsp/").unwrap();

        let contents = std::fs::read_to_string(&gitignore_path).unwrap();
        assert_eq!(contents, ".bsp\n");
    }

    #[test]
    fn ios_template_uses_default_manifest_shape_and_files() {
        let schema_path = "/tmp/.orbit/schemas/apple-app.v1.json";
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbit.exampleapp".to_owned(),
                template: InitTemplate::Ios,
            },
            schema_path,
        );

        assert_eq!(
            plan.manifest,
            json!({
                "$schema": schema_path,
                "name": "Example App",
                "bundle_id": "dev.orbit.exampleapp",
                "version": "1.0.0",
                "build": 1,
                "platforms": {
                    "ios": "18.0"
                },
                "sources": ["Sources/App"],
                "resources": ["Resources"]
            })
        );
        assert_eq!(
            plan.files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("Sources/App/App.swift"),
                PathBuf::from("Sources/App/HomeView.swift"),
            ]
        );
        assert_eq!(
            plan.next_commands,
            vec!["orbit run --platform ios --simulator".to_owned()]
        );
    }

    #[test]
    fn watch_template_generates_watch_manifest_and_delegate_source() {
        let schema_path = "/tmp/.orbit/schemas/apple-app.v1.json";
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbit.exampleapp".to_owned(),
                template: InitTemplate::IosWatchCompanion,
            },
            schema_path,
        );

        assert_eq!(
            plan.manifest,
            json!({
                "$schema": schema_path,
                "name": "Example App",
                "bundle_id": "dev.orbit.exampleapp",
                "version": "1.0.0",
                "build": 1,
                "platforms": {
                    "ios": "18.0",
                    "watchos": "11.0"
                },
                "sources": ["Sources/App"],
                "resources": ["Resources"],
                "watch": {
                    "sources": ["Sources/WatchApp"],
                    "extension": {
                        "sources": ["Sources/WatchExtension"],
                        "entry": {
                            "class": "WatchExtensionDelegate"
                        }
                    }
                }
            })
        );
        assert!(plan.files.iter().any(|file| file.path
            == Path::new("Sources/WatchExtension/Extension.swift")
            && file.contents.contains("final class WatchExtensionDelegate")));
    }

    #[test]
    fn create_scaffold_writes_manifest_directories_and_sources() {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = temp.path().join("nested/orbit.json");
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbit.exampleapp".to_owned(),
                template: InitTemplate::AppleMultiplatform,
            },
            "/tmp/.orbit/schemas/apple-app.v1.json",
        );

        create_scaffold(manifest_path.parent().unwrap(), &manifest_path, &plan).unwrap();

        let manifest: JsonValue = read_json_file(&manifest_path).unwrap();
        assert_eq!(manifest, plan.manifest);
        assert!(manifest_path.parent().unwrap().join("Sources/App").is_dir());
        assert!(manifest_path.parent().unwrap().join("Resources").is_dir());
        assert!(
            manifest_path
                .parent()
                .unwrap()
                .join("Sources/App/App.swift")
                .is_file()
        );
        assert!(
            manifest_path
                .parent()
                .unwrap()
                .join("Sources/App/HomeView.swift")
                .is_file()
        );
    }
}
