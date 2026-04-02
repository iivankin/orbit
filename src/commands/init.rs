use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value as JsonValue, json};

use crate::context::AppContext;
use crate::manifest::{ManifestSchema, installed_schema_path};
use crate::util::{
    ensure_dir, ensure_parent_dir, print_success, prompt_input, prompt_select, resolve_path,
    write_json_file,
};

const DEFAULT_VERSION: &str = "1.0.0";
const DEFAULT_BUILD: u64 = 1;
const DEFAULT_SOURCE_DIR: &str = "Sources/App";
const DEFAULT_RESOURCES_DIR: &str = "Resources";
const DEFAULT_WATCH_APP_SOURCE_DIR: &str = "Sources/WatchApp";
const DEFAULT_WATCH_EXTENSION_SOURCE_DIR: &str = "Sources/WatchExtension";

const ECOSYSTEM_CHOICES: [EcosystemChoice; 1] = [EcosystemChoice {
    kind: InitEcosystem::Apple,
    label: "Apple",
    description: "iOS, macOS, tvOS, watchOS, and visionOS apps",
}];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitEcosystem {
    Apple,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitTemplate {
    Ios,
    Macos,
    AppleMultiplatform,
    IosWatchCompanion,
    Tvos,
    Visionos,
}

impl InitEcosystem {
    fn manifest_schema(self) -> ManifestSchema {
        match self {
            Self::Apple => ManifestSchema::AppleAppV1,
        }
    }

    fn template_choices(self) -> &'static [TemplateChoice] {
        match self {
            Self::Apple => &APPLE_TEMPLATE_CHOICES,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct EcosystemChoice {
    kind: InitEcosystem,
    label: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct TemplateChoice {
    kind: InitTemplate,
    label: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InitAnswers {
    ecosystem: InitEcosystem,
    name: String,
    bundle_id: String,
    template: InitTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScaffoldPlan {
    manifest: JsonValue,
    directories: Vec<PathBuf>,
    files: Vec<GeneratedFile>,
    next_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneratedFile {
    path: PathBuf,
    contents: String,
}

pub fn execute(app: &AppContext, requested_manifest: Option<&Path>) -> Result<()> {
    if !app.interactive {
        bail!("`orbit init` requires an interactive terminal");
    }

    let manifest_path = init_manifest_path(app, requested_manifest);
    if manifest_path.exists() {
        bail!("manifest already exists at {}", manifest_path.display());
    }

    let project_root = manifest_path
        .parent()
        .context("manifest path did not contain a parent directory")?;
    let answers = collect_init_answers(project_root)?;
    let schema_reference = installed_schema_reference(app, answers.ecosystem);
    let plan = scaffold_plan(&answers, &schema_reference);

    create_scaffold(project_root, &manifest_path, &plan)?;
    print_success(format!("Created {}", manifest_path.display()));

    println!("Next commands:");
    for command in &plan.next_commands {
        println!("  {command}");
    }

    Ok(())
}

fn init_manifest_path(app: &AppContext, requested_manifest: Option<&Path>) -> PathBuf {
    requested_manifest
        .map(|path| resolve_path(&app.cwd, path))
        .unwrap_or_else(|| app.cwd.join("orbit.json"))
}

fn collect_init_answers(project_root: &Path) -> Result<InitAnswers> {
    let ecosystem = prompt_ecosystem()?;
    let default_name = suggested_product_name(project_root);
    let name = prompt_non_empty("Product name", Some(default_name.as_str()))?;
    let default_bundle_id = format!("dev.orbit.{}", bundle_id_suffix(&name));
    let bundle_id = prompt_validated(
        "Bundle ID",
        Some(default_bundle_id.as_str()),
        looks_like_bundle_id,
        "Enter a reverse-DNS bundle ID like `dev.orbit.exampleapp`.",
    )?;
    let template = prompt_template(ecosystem)?;

    Ok(InitAnswers {
        ecosystem,
        name,
        bundle_id,
        template,
    })
}

fn prompt_ecosystem() -> Result<InitEcosystem> {
    let labels = ECOSYSTEM_CHOICES
        .iter()
        .map(|choice| format!("{}: {}", choice.label, choice.description))
        .collect::<Vec<_>>();
    let index = prompt_select("Ecosystem", &labels)?;
    Ok(ECOSYSTEM_CHOICES[index].kind)
}

fn prompt_template(ecosystem: InitEcosystem) -> Result<InitTemplate> {
    let labels = ecosystem
        .template_choices()
        .iter()
        .map(|choice| format!("{}: {}", choice.label, choice.description))
        .collect::<Vec<_>>();
    let index = prompt_select("Template", &labels)?;
    Ok(ecosystem.template_choices()[index].kind)
}

fn installed_schema_reference(app: &AppContext, ecosystem: InitEcosystem) -> String {
    installed_schema_path(&app.global_paths.schema_dir, ecosystem.manifest_schema())
        .display()
        .to_string()
}

fn scaffold_plan(answers: &InitAnswers, schema_reference: &str) -> ScaffoldPlan {
    let swift_name = swift_type_name(&answers.name);
    match answers.template {
        InitTemplate::Ios => app_template_plan(
            answers,
            json!({
                "$schema": schema_reference,
                "name": answers.name,
                "bundle_id": answers.bundle_id,
                "version": DEFAULT_VERSION,
                "build": DEFAULT_BUILD,
                "platforms": {
                    "ios": "18.0"
                },
                "sources": [DEFAULT_SOURCE_DIR],
                "resources": [DEFAULT_RESOURCES_DIR]
            }),
            vec![
                GeneratedFile {
                    path: PathBuf::from("Sources/App/App.swift"),
                    contents: app_file_contents(&format!("{swift_name}App"), "HomeView"),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/App/HomeView.swift"),
                    contents: home_view_file_contents(
                        "HomeView",
                        "Orbit is ready for iOS",
                        "Edit Sources/App/HomeView.swift, then launch the simulator again.",
                    ),
                },
            ],
            vec!["orbit run --platform ios --simulator".to_owned()],
        ),
        InitTemplate::Macos => app_template_plan(
            answers,
            json!({
                "$schema": schema_reference,
                "name": answers.name,
                "bundle_id": answers.bundle_id,
                "version": DEFAULT_VERSION,
                "build": DEFAULT_BUILD,
                "platforms": {
                    "macos": "15.0"
                },
                "sources": [DEFAULT_SOURCE_DIR],
                "resources": [DEFAULT_RESOURCES_DIR]
            }),
            vec![
                GeneratedFile {
                    path: PathBuf::from("Sources/App/App.swift"),
                    contents: app_file_contents(&format!("{swift_name}App"), "HomeView"),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/App/HomeView.swift"),
                    contents: home_view_file_contents(
                        "HomeView",
                        "Orbit is ready for macOS",
                        "Edit Sources/App/HomeView.swift, then relaunch the app from Orbit.",
                    ),
                },
            ],
            vec!["orbit run --platform macos".to_owned()],
        ),
        InitTemplate::AppleMultiplatform => app_template_plan(
            answers,
            json!({
                "$schema": schema_reference,
                "name": answers.name,
                "bundle_id": answers.bundle_id,
                "version": DEFAULT_VERSION,
                "build": DEFAULT_BUILD,
                "platforms": {
                    "ios": "18.0",
                    "macos": "15.0"
                },
                "sources": [DEFAULT_SOURCE_DIR],
                "resources": [DEFAULT_RESOURCES_DIR]
            }),
            vec![
                GeneratedFile {
                    path: PathBuf::from("Sources/App/App.swift"),
                    contents: app_file_contents(&format!("{swift_name}App"), "HomeView"),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/App/HomeView.swift"),
                    contents: home_view_file_contents(
                        "HomeView",
                        "Orbit is ready for iOS and macOS",
                        "Edit one shared SwiftUI surface and run either platform from Orbit.",
                    ),
                },
            ],
            vec![
                "orbit run --platform ios --simulator".to_owned(),
                "orbit run --platform macos".to_owned(),
            ],
        ),
        InitTemplate::IosWatchCompanion => ScaffoldPlan {
            manifest: json!({
                "$schema": schema_reference,
                "name": answers.name,
                "bundle_id": answers.bundle_id,
                "version": DEFAULT_VERSION,
                "build": DEFAULT_BUILD,
                "platforms": {
                    "ios": "18.0",
                    "watchos": "11.0"
                },
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
            directories: vec![
                PathBuf::from(DEFAULT_SOURCE_DIR),
                PathBuf::from(DEFAULT_RESOURCES_DIR),
                PathBuf::from(DEFAULT_WATCH_APP_SOURCE_DIR),
                PathBuf::from(DEFAULT_WATCH_EXTENSION_SOURCE_DIR),
            ],
            files: vec![
                GeneratedFile {
                    path: PathBuf::from("Sources/App/App.swift"),
                    contents: app_file_contents(&format!("{swift_name}App"), "PhoneHomeView"),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/App/PhoneHomeView.swift"),
                    contents: home_view_file_contents(
                        "PhoneHomeView",
                        "Orbit host app",
                        "Edit the host iOS app here, then run the iPhone simulator again.",
                    ),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/WatchApp/App.swift"),
                    contents: app_file_contents(&format!("{swift_name}WatchApp"), "WatchHomeView"),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/WatchApp/WatchHomeView.swift"),
                    contents: home_view_file_contents(
                        "WatchHomeView",
                        "Orbit watch companion",
                        "Edit the watch UI here, then launch the watch simulator from Orbit.",
                    ),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/WatchExtension/Extension.swift"),
                    contents: watch_extension_file_contents(),
                },
            ],
            next_commands: vec![
                "orbit run --platform ios --simulator".to_owned(),
                "orbit run --platform watchos --simulator".to_owned(),
            ],
        },
        InitTemplate::Tvos => app_template_plan(
            answers,
            json!({
                "$schema": schema_reference,
                "name": answers.name,
                "bundle_id": answers.bundle_id,
                "version": DEFAULT_VERSION,
                "build": DEFAULT_BUILD,
                "platforms": {
                    "tvos": "18.0"
                },
                "sources": [DEFAULT_SOURCE_DIR],
                "resources": [DEFAULT_RESOURCES_DIR]
            }),
            vec![
                GeneratedFile {
                    path: PathBuf::from("Sources/App/App.swift"),
                    contents: app_file_contents(&format!("{swift_name}App"), "HomeView"),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/App/HomeView.swift"),
                    contents: home_view_file_contents(
                        "HomeView",
                        "Orbit is ready for tvOS",
                        "Edit Sources/App/HomeView.swift, then relaunch the Apple TV simulator.",
                    ),
                },
            ],
            vec!["orbit run --platform tvos --simulator".to_owned()],
        ),
        InitTemplate::Visionos => app_template_plan(
            answers,
            json!({
                "$schema": schema_reference,
                "name": answers.name,
                "bundle_id": answers.bundle_id,
                "version": DEFAULT_VERSION,
                "build": DEFAULT_BUILD,
                "platforms": {
                    "visionos": "2.0"
                },
                "sources": [DEFAULT_SOURCE_DIR],
                "resources": [DEFAULT_RESOURCES_DIR]
            }),
            vec![
                GeneratedFile {
                    path: PathBuf::from("Sources/App/App.swift"),
                    contents: app_file_contents(&format!("{swift_name}App"), "HomeView"),
                },
                GeneratedFile {
                    path: PathBuf::from("Sources/App/HomeView.swift"),
                    contents: home_view_file_contents(
                        "HomeView",
                        "Orbit is ready for visionOS",
                        "Edit Sources/App/HomeView.swift, then relaunch the visionOS simulator.",
                    ),
                },
            ],
            vec!["orbit run --platform visionos --simulator".to_owned()],
        ),
    }
}

fn app_template_plan(
    _answers: &InitAnswers,
    manifest: JsonValue,
    files: Vec<GeneratedFile>,
    next_commands: Vec<String>,
) -> ScaffoldPlan {
    ScaffoldPlan {
        manifest,
        directories: vec![
            PathBuf::from(DEFAULT_SOURCE_DIR),
            PathBuf::from(DEFAULT_RESOURCES_DIR),
        ],
        files,
        next_commands,
    }
}

fn create_scaffold(project_root: &Path, manifest_path: &Path, plan: &ScaffoldPlan) -> Result<()> {
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

fn prompt_non_empty(prompt: &str, default: Option<&str>) -> Result<String> {
    prompt_validated(
        prompt,
        default,
        |value| !value.is_empty(),
        "Value cannot be empty.",
    )
}

fn prompt_validated(
    prompt: &str,
    default: Option<&str>,
    validator: impl Fn(&str) -> bool,
    error_message: &str,
) -> Result<String> {
    loop {
        let value = prompt_input(prompt, default)?;
        let value = value.trim();
        if validator(value) {
            return Ok(value.to_owned());
        }
        println!("{error_message}");
    }
}

fn suggested_product_name(project_root: &Path) -> String {
    let raw_name = project_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("OrbitApp");
    let words = raw_name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() {
        return "OrbitApp".to_owned();
    }

    words
        .iter()
        .map(|part| {
            let mut characters = part.chars();
            let Some(first) = characters.next() else {
                return String::new();
            };
            format!(
                "{}{}",
                first.to_ascii_uppercase(),
                characters.as_str().to_ascii_lowercase()
            )
        })
        .collect()
}

fn bundle_id_suffix(name: &str) -> String {
    let suffix = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect::<String>();
    if suffix.is_empty() {
        "app".to_owned()
    } else {
        suffix
    }
}

fn swift_type_name(name: &str) -> String {
    let words = name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut value = if words.is_empty() {
        "Orbit".to_owned()
    } else {
        words
            .iter()
            .map(|part| {
                let mut characters = part.chars();
                let Some(first) = characters.next() else {
                    return String::new();
                };
                format!(
                    "{}{}",
                    first.to_ascii_uppercase(),
                    characters.as_str().to_ascii_lowercase()
                )
            })
            .collect::<String>()
    };
    if value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        value.insert_str(0, "Orbit");
    }
    value
}

fn looks_like_bundle_id(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() >= 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && is_bundle_id_component(part))
}

fn is_bundle_id_component(value: &str) -> bool {
    value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-')
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
    fn apple_ecosystem_maps_to_apple_manifest_schema() {
        assert_eq!(
            InitEcosystem::Apple.manifest_schema(),
            ManifestSchema::AppleAppV1
        );
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
