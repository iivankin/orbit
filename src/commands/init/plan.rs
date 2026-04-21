use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asc_sync::config::DeviceFamily;
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
const HOME_VIEW_CONTROLLER_NAME: &str = "HomeViewController";
const MANIFEST_DESCRIPTION: &str = "This file is documented by its `$schema`. Start with `orbi --help` for the common workflow and `orbi ui init --help` for scaffolding `tests.ui` flows.";
const ASC_MANIFEST_DESCRIPTION: &str = "This embedded config is documented by its `$schema`. Start with `orbi asc --help` for the common workflow.";
const ASC_IOS_DEVICE_ID: &str = "local-ios-device";
const ASC_TVOS_DEVICE_ID: &str = "local-apple-tv";
const ASC_MAC_DEVICE_ID: &str = "local-mac";
const IOS_DEVICE_FAMILIES: [DeviceFamily; 2] = [DeviceFamily::Ios, DeviceFamily::Ipados];
const WATCH_HOST_DEVICE_FAMILIES: [DeviceFamily; 1] = [DeviceFamily::Ios];
const TVOS_DEVICE_FAMILIES: [DeviceFamily; 1] = [DeviceFamily::Tvos];
const MACOS_DEVICE_FAMILIES: [DeviceFamily; 1] = [DeviceFamily::Macos];

const IOS_TEMPLATE_DEVICE_SLOTS: [InitDeviceSlot; 1] = [InitDeviceSlot::new(
    ASC_IOS_DEVICE_ID,
    "Development Device",
    "My iPhone",
    &IOS_DEVICE_FAMILIES,
    true,
)];
const WATCH_TEMPLATE_DEVICE_SLOTS: [InitDeviceSlot; 1] = [InitDeviceSlot::new(
    ASC_IOS_DEVICE_ID,
    "Paired iPhone",
    "My iPhone",
    &WATCH_HOST_DEVICE_FAMILIES,
    true,
)];
const TVOS_TEMPLATE_DEVICE_SLOTS: [InitDeviceSlot; 1] = [InitDeviceSlot::new(
    ASC_TVOS_DEVICE_ID,
    "Apple TV",
    "My Apple TV",
    &TVOS_DEVICE_FAMILIES,
    false,
)];
const MACOS_TEMPLATE_DEVICE_SLOTS: [InitDeviceSlot; 1] = [InitDeviceSlot::new(
    ASC_MAC_DEVICE_ID,
    "Mac Development Device",
    "This Mac",
    &MACOS_DEVICE_FAMILIES,
    false,
)];
const APPLE_MULTIPLATFORM_DEVICE_SLOTS: [InitDeviceSlot; 2] = [
    InitDeviceSlot::new(
        ASC_IOS_DEVICE_ID,
        "iPhone Or iPad",
        "My iPhone",
        &IOS_DEVICE_FAMILIES,
        true,
    ),
    InitDeviceSlot::new(
        ASC_MAC_DEVICE_ID,
        "Mac Development Device",
        "This Mac",
        &MACOS_DEVICE_FAMILIES,
        false,
    ),
];

const APPLE_TEMPLATE_CHOICES: [TemplateChoice; 8] = [
    TemplateChoice {
        kind: InitTemplate::Ios,
        label: "iOS app",
        description: "Single-target SwiftUI iPhone/iPad app",
    },
    TemplateChoice {
        kind: InitTemplate::IosUIKit,
        label: "iOS UIKit app",
        description: "Single-target UIKit iPhone/iPad app",
    },
    TemplateChoice {
        kind: InitTemplate::MacosSwiftUi,
        label: "macOS SwiftUI app",
        description: "Single-target SwiftUI Mac app",
    },
    TemplateChoice {
        kind: InitTemplate::MacosAppKit,
        label: "macOS AppKit app",
        description: "Single-target AppKit Mac app",
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
    "Orbi is ready for iOS",
    "Edit Sources/App/HomeView.swift, then launch the simulator again.",
    &["orbi run --platform ios --simulator"],
);
const IOS_UIKIT_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &IOS_PLATFORMS,
    "Orbi is ready for iOS",
    "Edit Sources/App/HomeViewController.swift, then launch the simulator again.",
    &["orbi run --platform ios --simulator"],
);
const MACOS_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &MACOS_PLATFORMS,
    "Orbi is ready for macOS",
    "Edit Sources/App/HomeView.swift, then relaunch the app from Orbi.",
    &["orbi run --platform macos"],
);
const MACOS_APPKIT_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &MACOS_PLATFORMS,
    "Orbi is ready for macOS",
    "Edit Sources/App/HomeViewController.swift, then relaunch the app from Orbi.",
    &["orbi run --platform macos"],
);
const APPLE_MULTIPLATFORM_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &APPLE_MULTIPLATFORM_PLATFORMS,
    "Orbi is ready for iOS and macOS",
    "Edit one shared SwiftUI surface and run either platform from Orbi.",
    &[
        "orbi run --platform ios --simulator",
        "orbi run --platform macos",
    ],
);
const TVOS_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &TVOS_PLATFORMS,
    "Orbi is ready for tvOS",
    "Edit Sources/App/HomeView.swift, then relaunch the Apple TV simulator.",
    &["orbi run --platform tvos --simulator"],
);
const VISIONOS_APP_TEMPLATE: AppTemplateSpec = AppTemplateSpec::new(
    &VISIONOS_PLATFORMS,
    "Orbi is ready for visionOS",
    "Edit Sources/App/HomeView.swift, then relaunch the visionOS simulator.",
    &["orbi run --platform visionos --simulator"],
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
    IosUIKit,
    MacosSwiftUi,
    MacosAppKit,
    AppleMultiplatform,
    IosWatchCompanion,
    Tvos,
    Visionos,
}

impl InitTemplate {
    pub(super) const fn required_device_slots(self) -> &'static [InitDeviceSlot] {
        match self {
            Self::Ios | Self::IosUIKit => &IOS_TEMPLATE_DEVICE_SLOTS,
            Self::MacosSwiftUi | Self::MacosAppKit => &MACOS_TEMPLATE_DEVICE_SLOTS,
            Self::AppleMultiplatform => &APPLE_MULTIPLATFORM_DEVICE_SLOTS,
            Self::IosWatchCompanion => &WATCH_TEMPLATE_DEVICE_SLOTS,
            Self::Tvos => &TVOS_TEMPLATE_DEVICE_SLOTS,
            Self::Visionos => &[],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TemplateChoice {
    pub(super) kind: InitTemplate,
    pub(super) label: &'static str,
    pub(super) description: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InitDeviceSlot {
    pub(super) logical_id: &'static str,
    pub(super) prompt: &'static str,
    pub(super) default_name: &'static str,
    pub(super) compatible_families: &'static [DeviceFamily],
    pub(super) allow_registration: bool,
}

impl InitDeviceSlot {
    pub(super) const fn new(
        logical_id: &'static str,
        prompt: &'static str,
        default_name: &'static str,
        compatible_families: &'static [DeviceFamily],
        allow_registration: bool,
    ) -> Self {
        Self {
            logical_id,
            prompt,
            default_name,
            compatible_families,
            allow_registration,
        }
    }

    pub(super) fn supports_family(self, family: DeviceFamily) -> bool {
        self.compatible_families.contains(&family)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InitAscDevice {
    pub(super) logical_id: &'static str,
    pub(super) family: DeviceFamily,
    pub(super) udid: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InitAnswers {
    pub(super) ecosystem: InitEcosystem,
    pub(super) name: String,
    pub(super) bundle_id: String,
    pub(super) template: InitTemplate,
    pub(super) asc: Option<InitAscAnswers>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InitAscAnswers {
    pub(super) team_id: String,
    pub(super) devices: Vec<InitAscDevice>,
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
        InitTemplate::IosUIKit => {
            ios_uikit_plan(answers, schema_reference, &IOS_UIKIT_APP_TEMPLATE)
        }
        InitTemplate::MacosSwiftUi => {
            app_template_plan(answers, schema_reference, &MACOS_APP_TEMPLATE)
        }
        InitTemplate::MacosAppKit => {
            macos_appkit_plan(answers, schema_reference, &MACOS_APPKIT_TEMPLATE)
        }
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

fn ios_uikit_plan(
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
                uikit_app_file_contents(&format!("{swift_name}App"), HOME_VIEW_CONTROLLER_NAME),
            ),
            generated_file(
                "Sources/App/HomeViewController.swift",
                uikit_home_view_controller_file_contents(
                    HOME_VIEW_CONTROLLER_NAME,
                    template.home_view_title,
                    template.home_view_detail,
                ),
            ),
        ],
        next_commands: next_commands(answers, template.next_commands),
    }
}

fn macos_appkit_plan(
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
                appkit_app_file_contents(
                    &format!("{swift_name}App"),
                    HOME_VIEW_CONTROLLER_NAME,
                    &answers.name,
                ),
            ),
            generated_file(
                "Sources/App/HomeViewController.swift",
                appkit_home_view_controller_file_contents(
                    HOME_VIEW_CONTROLLER_NAME,
                    template.home_view_title,
                    template.home_view_detail,
                ),
            ),
        ],
        next_commands: next_commands(answers, template.next_commands),
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
    ensure_gitignore_entry(project_root, ".orbi/")?;

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
        next_commands: next_commands(answers, template.next_commands),
    }
}

fn watch_companion_plan(answers: &InitAnswers, schema_reference: &str) -> ScaffoldPlan {
    let swift_name = swift_type_name(&answers.name);
    let manifest = with_optional_scaffolded_asc(
        answers,
        json!({
            "$schema": schema_reference,
            "_description": MANIFEST_DESCRIPTION,
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
    );
    ScaffoldPlan {
        manifest,
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
                    "Orbi host app",
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
                    "Orbi watch companion",
                    "Edit the watch UI here, then launch the watch simulator from Orbi.",
                ),
            ),
            generated_file(
                "Sources/WatchExtension/Extension.swift",
                watch_extension_file_contents(),
            ),
        ],
        next_commands: next_commands(
            answers,
            &[
                "orbi run --platform ios --simulator",
                "orbi run --platform watchos --simulator",
            ],
        ),
    }
}

fn app_manifest(
    answers: &InitAnswers,
    schema_reference: &str,
    platforms: &[ManifestPlatform],
) -> JsonValue {
    with_optional_scaffolded_asc(
        answers,
        json!({
            "$schema": schema_reference,
            "_description": MANIFEST_DESCRIPTION,
            "name": answers.name,
            "bundle_id": answers.bundle_id,
            "version": DEFAULT_VERSION,
            "build": DEFAULT_BUILD,
            "platforms": platform_manifest(platforms),
            "sources": [DEFAULT_SOURCE_DIR],
            "resources": [DEFAULT_RESOURCES_DIR]
        }),
    )
}

fn with_optional_scaffolded_asc(answers: &InitAnswers, mut manifest: JsonValue) -> JsonValue {
    if answers.asc.is_none() {
        return manifest;
    }

    manifest
        .as_object_mut()
        .expect("init manifests always serialize as JSON objects")
        .insert("asc".to_owned(), asc_manifest(answers));
    manifest
}

pub(super) fn asc_manifest(answers: &InitAnswers) -> JsonValue {
    with_asc_description(match answers.template {
        InitTemplate::Ios | InitTemplate::IosUIKit => ios_asc_manifest(answers),
        InitTemplate::MacosSwiftUi | InitTemplate::MacosAppKit => macos_asc_manifest(answers),
        InitTemplate::AppleMultiplatform => multiplatform_asc_manifest(answers),
        InitTemplate::IosWatchCompanion => watch_companion_asc_manifest(answers),
        InitTemplate::Tvos => tvos_asc_manifest(answers),
        InitTemplate::Visionos => visionos_asc_manifest(answers),
    })
}

fn next_commands(answers: &InitAnswers, template_commands: &[&str]) -> Vec<String> {
    let mut commands = Vec::new();
    if answers.asc.is_some() {
        commands.push("orbi asc apply".to_owned());
    }
    commands.extend(template_commands.iter().map(ToString::to_string));
    commands
}

fn with_asc_description(manifest: JsonValue) -> JsonValue {
    let object = manifest
        .as_object()
        .expect("init ASC manifests always serialize as JSON objects");
    let mut ordered = JsonMap::new();
    if let Some(schema) = object.get("$schema") {
        ordered.insert("$schema".to_owned(), schema.clone());
    }
    ordered.insert(
        "_description".to_owned(),
        JsonValue::String(ASC_MANIFEST_DESCRIPTION.to_owned()),
    );
    for (key, value) in object {
        if key != "$schema" && key != "_description" {
            ordered.insert(key.clone(), value.clone());
        }
    }
    JsonValue::Object(ordered)
}

fn ios_asc_manifest(answers: &InitAnswers) -> JsonValue {
    let device = required_device(answers, ASC_IOS_DEVICE_ID);
    json!({
        "$schema": asc_sync::schema::PUBLISHED_SCHEMA_URL,
        "team_id": required_asc(answers).team_id,
        "bundle_ids": {
            "app": asc_bundle_id(&answers.bundle_id, &answers.name, "ios")
        },
        "devices": asc_devices_manifest(&[device]),
        "certs": {
            "development": asc_cert("development", format!("{} Development", answers.name)),
            "distribution": asc_cert("distribution", format!("{} Distribution", answers.name))
        },
        "profiles": {
            "app-development": asc_profile(
                format!("{} Development", answers.name),
                "ios_app_development",
                "app",
                &["development"],
                &[device.logical_id],
            ),
            "app-adhoc": asc_profile(
                format!("{} Ad Hoc", answers.name),
                "ios_app_adhoc",
                "app",
                &["distribution"],
                &[device.logical_id],
            ),
            "app-store": asc_profile(
                format!("{} App Store", answers.name),
                "ios_app_store",
                "app",
                &["distribution"],
                &[],
            )
        }
    })
}

fn tvos_asc_manifest(answers: &InitAnswers) -> JsonValue {
    let device = required_device(answers, ASC_TVOS_DEVICE_ID);
    json!({
        "$schema": asc_sync::schema::PUBLISHED_SCHEMA_URL,
        "team_id": required_asc(answers).team_id,
        "bundle_ids": {
            "app": asc_bundle_id(&answers.bundle_id, &answers.name, "ios")
        },
        "devices": asc_devices_manifest(&[device]),
        "certs": {
            "development": asc_cert("development", format!("{} Development", answers.name)),
            "distribution": asc_cert("distribution", format!("{} Distribution", answers.name))
        },
        "profiles": {
            "app-development": asc_profile(
                format!("{} Development", answers.name),
                "tvos_app_development",
                "app",
                &["development"],
                &[device.logical_id],
            ),
            "app-adhoc": asc_profile(
                format!("{} Ad Hoc", answers.name),
                "tvos_app_adhoc",
                "app",
                &["distribution"],
                &[device.logical_id],
            ),
            "app-store": asc_profile(
                format!("{} App Store", answers.name),
                "tvos_app_store",
                "app",
                &["distribution"],
                &[],
            )
        }
    })
}

fn macos_asc_manifest(answers: &InitAnswers) -> JsonValue {
    let device = required_device(answers, ASC_MAC_DEVICE_ID);
    json!({
        "$schema": asc_sync::schema::PUBLISHED_SCHEMA_URL,
        "team_id": required_asc(answers).team_id,
        "bundle_ids": {
            "app": asc_bundle_id(&answers.bundle_id, &answers.name, "mac_os")
        },
        "devices": asc_devices_manifest(&[device]),
        "certs": {
            "development": asc_cert("development", format!("{} Development", answers.name)),
            "distribution": asc_cert("distribution", format!("{} Distribution", answers.name)),
            "developer-id": asc_cert("developer_id_application", format!("{} Developer ID", answers.name))
        },
        "profiles": {
            "app-development": asc_profile(
                format!("{} Development", answers.name),
                "mac_app_development",
                "app",
                &["development"],
                &[device.logical_id],
            ),
            "app-store": asc_profile(
                format!("{} Mac App Store", answers.name),
                "mac_app_store",
                "app",
                &["distribution"],
                &[],
            ),
            "developer-id": asc_profile(
                format!("{} Developer ID", answers.name),
                "mac_app_direct",
                "app",
                &["developer-id"],
                &[],
            )
        }
    })
}

fn multiplatform_asc_manifest(answers: &InitAnswers) -> JsonValue {
    let ios_device = required_device(answers, ASC_IOS_DEVICE_ID);
    let mac_device = required_device(answers, ASC_MAC_DEVICE_ID);
    json!({
        "$schema": asc_sync::schema::PUBLISHED_SCHEMA_URL,
        "team_id": required_asc(answers).team_id,
        "bundle_ids": {
            "app": asc_bundle_id(&answers.bundle_id, &answers.name, "universal")
        },
        "devices": asc_devices_manifest(&[ios_device, mac_device]),
        "certs": {
            "development": asc_cert("development", format!("{} Development", answers.name)),
            "distribution": asc_cert("distribution", format!("{} Distribution", answers.name)),
            "developer-id": asc_cert("developer_id_application", format!("{} Developer ID", answers.name))
        },
        "profiles": {
            "ios-development": asc_profile(
                format!("{} iOS Development", answers.name),
                "ios_app_development",
                "app",
                &["development"],
                &[ios_device.logical_id],
            ),
            "ios-adhoc": asc_profile(
                format!("{} iOS Ad Hoc", answers.name),
                "ios_app_adhoc",
                "app",
                &["distribution"],
                &[ios_device.logical_id],
            ),
            "ios-app-store": asc_profile(
                format!("{} iOS App Store", answers.name),
                "ios_app_store",
                "app",
                &["distribution"],
                &[],
            ),
            "mac-development": asc_profile(
                format!("{} Mac Development", answers.name),
                "mac_app_development",
                "app",
                &["development"],
                &[mac_device.logical_id],
            ),
            "mac-app-store": asc_profile(
                format!("{} Mac App Store", answers.name),
                "mac_app_store",
                "app",
                &["distribution"],
                &[],
            ),
            "developer-id": asc_profile(
                format!("{} Developer ID", answers.name),
                "mac_app_direct",
                "app",
                &["developer-id"],
                &[],
            )
        }
    })
}

fn watch_companion_asc_manifest(answers: &InitAnswers) -> JsonValue {
    let device = required_device(answers, ASC_IOS_DEVICE_ID);
    let watch_app_name = format!("{} Watch App", answers.name);
    let watch_extension_name = format!("{} Watch Extension", answers.name);
    json!({
        "$schema": asc_sync::schema::PUBLISHED_SCHEMA_URL,
        "team_id": required_asc(answers).team_id,
        "bundle_ids": {
            "app": asc_bundle_id(&answers.bundle_id, &answers.name, "ios"),
            "watch-app": asc_bundle_id(
                &format!("{}.watchkitapp", answers.bundle_id),
                &watch_app_name,
                "ios",
            ),
            "watch-extension": asc_bundle_id(
                &format!("{}.watchkitapp.watchkitextension", answers.bundle_id),
                &watch_extension_name,
                "ios",
            )
        },
        "devices": asc_devices_manifest(&[device]),
        "certs": {
            "development": asc_cert("development", format!("{} Development", answers.name)),
            "distribution": asc_cert("distribution", format!("{} Distribution", answers.name))
        },
        "profiles": {
            "app-development": asc_profile(
                format!("{} Development", answers.name),
                "ios_app_development",
                "app",
                &["development"],
                &[device.logical_id],
            ),
            "app-adhoc": asc_profile(
                format!("{} Ad Hoc", answers.name),
                "ios_app_adhoc",
                "app",
                &["distribution"],
                &[device.logical_id],
            ),
            "app-store": asc_profile(
                format!("{} App Store", answers.name),
                "ios_app_store",
                "app",
                &["distribution"],
                &[],
            ),
            "watch-app-development": asc_profile(
                format!("{watch_app_name} Development"),
                "ios_app_development",
                "watch-app",
                &["development"],
                &[device.logical_id],
            ),
            "watch-app-adhoc": asc_profile(
                format!("{watch_app_name} Ad Hoc"),
                "ios_app_adhoc",
                "watch-app",
                &["distribution"],
                &[device.logical_id],
            ),
            "watch-app-store": asc_profile(
                format!("{watch_app_name} App Store"),
                "ios_app_store",
                "watch-app",
                &["distribution"],
                &[],
            ),
            "watch-extension-development": asc_profile(
                format!("{watch_extension_name} Development"),
                "ios_app_development",
                "watch-extension",
                &["development"],
                &[device.logical_id],
            ),
            "watch-extension-adhoc": asc_profile(
                format!("{watch_extension_name} Ad Hoc"),
                "ios_app_adhoc",
                "watch-extension",
                &["distribution"],
                &[device.logical_id],
            ),
            "watch-extension-store": asc_profile(
                format!("{watch_extension_name} App Store"),
                "ios_app_store",
                "watch-extension",
                &["distribution"],
                &[],
            )
        }
    })
}

fn visionos_asc_manifest(answers: &InitAnswers) -> JsonValue {
    json!({
        "$schema": asc_sync::schema::PUBLISHED_SCHEMA_URL,
        "team_id": required_asc(answers).team_id,
        "bundle_ids": {
            "app": asc_bundle_id(&answers.bundle_id, &answers.name, "ios")
        },
        "devices": {},
        "certs": {
            "distribution": asc_cert("distribution", format!("{} Distribution", answers.name))
        },
        "profiles": {
            "app-store": asc_profile(
                format!("{} App Store", answers.name),
                "ios_app_store",
                "app",
                &["distribution"],
                &[],
            )
        }
    })
}

fn required_asc(answers: &InitAnswers) -> &InitAscAnswers {
    answers
        .asc
        .as_ref()
        .expect("init ASC manifest requires ASC answers")
}

fn required_device<'a>(answers: &'a InitAnswers, logical_id: &str) -> &'a InitAscDevice {
    required_asc(answers)
        .devices
        .iter()
        .find(|device| device.logical_id == logical_id)
        .unwrap_or_else(|| panic!("missing required init ASC device {logical_id}"))
}

fn asc_bundle_id(bundle_id: &str, name: &str, platform: &str) -> JsonValue {
    json!({
        "bundle_id": bundle_id,
        "name": name,
        "platform": platform
    })
}

fn asc_devices_manifest(devices: &[&InitAscDevice]) -> JsonValue {
    JsonValue::Object(
        devices
            .iter()
            .map(|device| (device.logical_id.to_owned(), asc_device(device)))
            .collect::<JsonMap<_, _>>(),
    )
}

fn asc_device(device: &InitAscDevice) -> JsonValue {
    json!({
        "family": device.family.to_string(),
        "udid": device.udid,
        "name": device.name
    })
}

fn asc_cert(kind: &str, name: String) -> JsonValue {
    json!({
        "type": kind,
        "name": name
    })
}

fn asc_profile(
    name: String,
    kind: &str,
    bundle_id: &str,
    certs: &[&str],
    devices: &[&str],
) -> JsonValue {
    json!({
        "name": name,
        "type": kind,
        "bundle_id": bundle_id,
        "certs": certs,
        "devices": devices
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
        "import SwiftUI\n\nstruct {view_name}: View {{\n    var body: some View {{\n        VStack(spacing: 16) {{\n            Image(systemName: \"sparkles\")\n                .font(.system(size: 44))\n                .foregroundStyle(.tint)\n            Text(\"{title}\")\n                .font(.largeTitle.bold())\n            Text(\"{detail}\")\n                .multilineTextAlignment(.center)\n                .foregroundStyle(.secondary)\n        }}\n        .padding(32)\n    }}\n}}\n\n#Preview {{\n    {view_name}()\n}}\n"
    )
}

fn uikit_app_file_contents(app_type_name: &str, root_controller_name: &str) -> String {
    format!(
        "import UIKit\n\n@main\nfinal class {app_type_name}: UIResponder, UIApplicationDelegate {{\n    var window: UIWindow?\n\n    func application(\n        _ application: UIApplication,\n        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?\n    ) -> Bool {{\n        let window = UIWindow(frame: UIScreen.main.bounds)\n        window.rootViewController = {root_controller_name}()\n        window.makeKeyAndVisible()\n        self.window = window\n        return true\n    }}\n}}\n"
    )
}

fn uikit_home_view_controller_file_contents(
    controller_name: &str,
    title: &str,
    detail: &str,
) -> String {
    format!(
        "import UIKit\n\nfinal class {controller_name}: UIViewController {{\n    override func viewDidLoad() {{\n        super.viewDidLoad()\n        view.backgroundColor = .systemBackground\n\n        let symbolView = UIImageView(image: UIImage(systemName: \"sparkles\"))\n        symbolView.tintColor = .tintColor\n        symbolView.preferredSymbolConfiguration = UIImage.SymbolConfiguration(pointSize: 44, weight: .regular)\n\n        let titleLabel = UILabel()\n        titleLabel.text = \"{title}\"\n        titleLabel.font = .preferredFont(forTextStyle: .largeTitle)\n        titleLabel.adjustsFontForContentSizeCategory = true\n        titleLabel.textAlignment = .center\n\n        let detailLabel = UILabel()\n        detailLabel.text = \"{detail}\"\n        detailLabel.font = .preferredFont(forTextStyle: .body)\n        detailLabel.adjustsFontForContentSizeCategory = true\n        detailLabel.textColor = .secondaryLabel\n        detailLabel.textAlignment = .center\n        detailLabel.numberOfLines = 0\n\n        let stack = UIStackView(arrangedSubviews: [symbolView, titleLabel, detailLabel])\n        stack.axis = .vertical\n        stack.alignment = .center\n        stack.spacing = 16\n        stack.translatesAutoresizingMaskIntoConstraints = false\n\n        view.addSubview(stack)\n        NSLayoutConstraint.activate([\n            stack.centerXAnchor.constraint(equalTo: view.centerXAnchor),\n            stack.centerYAnchor.constraint(equalTo: view.centerYAnchor),\n            stack.leadingAnchor.constraint(greaterThanOrEqualTo: view.safeAreaLayoutGuide.leadingAnchor, constant: 32),\n            stack.trailingAnchor.constraint(lessThanOrEqualTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -32),\n            detailLabel.widthAnchor.constraint(lessThanOrEqualToConstant: 360),\n        ])\n    }}\n}}\n\n#Preview {{\n    {controller_name}()\n}}\n"
    )
}

fn appkit_app_file_contents(
    app_type_name: &str,
    root_controller_name: &str,
    app_name: &str,
) -> String {
    format!(
        "import AppKit\n\n@main\nstruct {app_type_name}Main {{\n    @MainActor\n    static func main() {{\n        let app = NSApplication.shared\n        let delegate = {app_type_name}()\n        app.delegate = delegate\n        _ = app.setActivationPolicy(.regular)\n        withExtendedLifetime(delegate) {{\n            app.run()\n        }}\n    }}\n}}\n\n@MainActor\nfinal class {app_type_name}: NSObject, NSApplicationDelegate {{\n    private var window: NSWindow?\n\n    func applicationDidFinishLaunching(_ notification: Notification) {{\n        let window = NSWindow(\n            contentRect: NSRect(x: 0, y: 0, width: 620, height: 420),\n            styleMask: [.titled, .closable, .miniaturizable, .resizable],\n            backing: .buffered,\n            defer: false\n        )\n        window.title = \"{app_name}\"\n        window.contentViewController = {root_controller_name}()\n        window.setContentSize(NSSize(width: 620, height: 420))\n        window.center()\n        window.makeKeyAndOrderFront(nil)\n        NSApplication.shared.activate()\n        self.window = window\n    }}\n\n    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {{\n        true\n    }}\n}}\n"
    )
}

fn appkit_home_view_controller_file_contents(
    controller_name: &str,
    title: &str,
    detail: &str,
) -> String {
    format!(
        "import AppKit\n\nfinal class {controller_name}: NSViewController {{\n    override func loadView() {{\n        view = NSView()\n    }}\n\n    override func viewDidLoad() {{\n        super.viewDidLoad()\n\n        let eyebrowLabel = NSTextField(labelWithString: \"Orbi\")\n        eyebrowLabel.font = .systemFont(ofSize: 15, weight: .semibold)\n        eyebrowLabel.textColor = .controlAccentColor\n\n        let titleLabel = NSTextField(labelWithString: \"{title}\")\n        titleLabel.font = .systemFont(ofSize: 30, weight: .bold)\n\n        let detailLabel = NSTextField(wrappingLabelWithString: \"{detail}\")\n        detailLabel.alignment = .center\n        detailLabel.textColor = .secondaryLabelColor\n\n        let stack = NSStackView(views: [eyebrowLabel, titleLabel, detailLabel])\n        stack.orientation = .vertical\n        stack.alignment = .centerX\n        stack.spacing = 16\n        stack.translatesAutoresizingMaskIntoConstraints = false\n\n        view.addSubview(stack)\n        NSLayoutConstraint.activate([\n            stack.centerXAnchor.constraint(equalTo: view.centerXAnchor),\n            stack.centerYAnchor.constraint(equalTo: view.centerYAnchor),\n            stack.leadingAnchor.constraint(greaterThanOrEqualTo: view.leadingAnchor, constant: 32),\n            stack.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -32),\n            detailLabel.widthAnchor.constraint(lessThanOrEqualToConstant: 360),\n        ])\n    }}\n}}\n\n#Preview {{\n    {controller_name}()\n}}\n"
    )
}

fn watch_extension_file_contents() -> String {
    "import Foundation\nimport WatchKit\n\n// Orbi uses this principal class from `watch.extension.entry.class`.\nfinal class WatchExtensionDelegate: NSObject, WKApplicationDelegate {}\n".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::read_json_file;

    const TEST_ASC_TEAM_ID: &str = "TEAM123456";
    const TEST_IOS_UDID: &str = "00008110-0000000000000001";
    const TEST_MAC_UDID: &str = "00000000-0000-0000-0000-000000000001";

    fn test_init_device(
        logical_id: &'static str,
        family: DeviceFamily,
        udid: &str,
        name: &str,
    ) -> InitAscDevice {
        InitAscDevice {
            logical_id,
            family,
            udid: udid.to_owned(),
            name: name.to_owned(),
        }
    }

    fn test_init_asc(devices: Vec<InitAscDevice>) -> Option<InitAscAnswers> {
        Some(InitAscAnswers {
            team_id: TEST_ASC_TEAM_ID.to_owned(),
            devices,
        })
    }

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
        let schema_path = "/tmp/.orbi/schemas/apple-app.v1.json";
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbi.exampleapp".to_owned(),
                template: InitTemplate::Ios,
                asc: test_init_asc(vec![test_init_device(
                    ASC_IOS_DEVICE_ID,
                    DeviceFamily::Ios,
                    TEST_IOS_UDID,
                    "Your iPhone",
                )]),
            },
            schema_path,
        );

        assert_eq!(
            plan.manifest,
            json!({
                "$schema": schema_path,
                "_description": MANIFEST_DESCRIPTION,
                "name": "Example App",
                "bundle_id": "dev.orbi.exampleapp",
                "version": "1.0.0",
                "build": 1,
                "platforms": {
                    "ios": "18.0"
                },
                "sources": ["Sources/App"],
                "resources": ["Resources"],
                "asc": {
                    "$schema": asc_sync::schema::PUBLISHED_SCHEMA_URL,
                    "_description": ASC_MANIFEST_DESCRIPTION,
                    "team_id": TEST_ASC_TEAM_ID,
                    "bundle_ids": {
                        "app": {
                            "bundle_id": "dev.orbi.exampleapp",
                            "name": "Example App",
                            "platform": "ios"
                        }
                    },
                    "devices": {
                        (ASC_IOS_DEVICE_ID): {
                            "family": "ios",
                            "udid": TEST_IOS_UDID,
                            "name": "Your iPhone"
                        }
                    },
                    "certs": {
                        "development": {
                            "type": "development",
                            "name": "Example App Development"
                        },
                        "distribution": {
                            "type": "distribution",
                            "name": "Example App Distribution"
                        }
                    },
                    "profiles": {
                        "app-development": {
                            "name": "Example App Development",
                            "type": "ios_app_development",
                            "bundle_id": "app",
                            "certs": ["development"],
                            "devices": [ASC_IOS_DEVICE_ID]
                        },
                        "app-adhoc": {
                            "name": "Example App Ad Hoc",
                            "type": "ios_app_adhoc",
                            "bundle_id": "app",
                            "certs": ["distribution"],
                            "devices": [ASC_IOS_DEVICE_ID]
                        },
                        "app-store": {
                            "name": "Example App App Store",
                            "type": "ios_app_store",
                            "bundle_id": "app",
                            "certs": ["distribution"],
                            "devices": []
                        }
                    }
                }
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
        assert!(plan.files.iter().any(|file| {
            file.path == Path::new("Sources/App/HomeView.swift")
                && file.contents.contains("#Preview")
                && file.contents.contains("HomeView()")
        }));
        assert_eq!(
            plan.next_commands,
            vec![
                "orbi asc apply".to_owned(),
                "orbi run --platform ios --simulator".to_owned()
            ]
        );
    }

    #[test]
    fn ios_template_can_skip_asc_manifest() {
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbi.exampleapp".to_owned(),
                template: InitTemplate::Ios,
                asc: None,
            },
            "/tmp/.orbi/schemas/apple-app.v1.json",
        );

        assert!(plan.manifest.get("asc").is_none());
        assert_eq!(
            plan.next_commands,
            vec!["orbi run --platform ios --simulator".to_owned()]
        );
    }

    #[test]
    fn ios_uikit_template_generates_uikit_sources() {
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbi.exampleapp".to_owned(),
                template: InitTemplate::IosUIKit,
                asc: test_init_asc(vec![test_init_device(
                    ASC_IOS_DEVICE_ID,
                    DeviceFamily::Ios,
                    TEST_IOS_UDID,
                    "Your iPhone",
                )]),
            },
            "/tmp/.orbi/schemas/apple-app.v1.json",
        );

        assert_eq!(
            plan.files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("Sources/App/App.swift"),
                PathBuf::from("Sources/App/HomeViewController.swift"),
            ]
        );
        assert!(plan.files.iter().any(|file| {
            file.path == Path::new("Sources/App/App.swift")
                && file.contents.contains("import UIKit")
                && file.contents.contains("UIApplicationDelegate")
        }));
        assert!(plan.files.iter().any(|file| {
            file.path == Path::new("Sources/App/HomeViewController.swift")
                && file
                    .contents
                    .contains("final class HomeViewController: UIViewController")
                && file.contents.contains("#Preview")
                && file.contents.contains("HomeViewController()")
        }));
        assert_eq!(
            plan.next_commands,
            vec![
                "orbi asc apply".to_owned(),
                "orbi run --platform ios --simulator".to_owned()
            ]
        );
    }

    #[test]
    fn macos_appkit_template_generates_appkit_sources() {
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example Mac".to_owned(),
                bundle_id: "dev.orbi.examplemac".to_owned(),
                template: InitTemplate::MacosAppKit,
                asc: test_init_asc(vec![test_init_device(
                    ASC_MAC_DEVICE_ID,
                    DeviceFamily::Macos,
                    TEST_MAC_UDID,
                    "This Mac",
                )]),
            },
            "/tmp/.orbi/schemas/apple-app.v1.json",
        );

        assert_eq!(
            plan.files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("Sources/App/App.swift"),
                PathBuf::from("Sources/App/HomeViewController.swift"),
            ]
        );
        assert!(plan.files.iter().any(|file| {
            file.path == Path::new("Sources/App/App.swift")
                && file.contents.contains("import AppKit")
                && file.contents.contains("NSApplicationDelegate")
                && file.contents.contains("app.delegate = delegate")
                && file.contents.contains("withExtendedLifetime(delegate)")
                && file
                    .contents
                    .contains("window.setContentSize(NSSize(width: 620, height: 420))")
        }));
        assert!(plan.files.iter().any(|file| {
            file.path == Path::new("Sources/App/HomeViewController.swift")
                && file
                    .contents
                    .contains("final class HomeViewController: NSViewController")
                && file.contents.contains("#Preview")
                && file.contents.contains("HomeViewController()")
        }));
        assert_eq!(
            plan.next_commands,
            vec![
                "orbi asc apply".to_owned(),
                "orbi run --platform macos".to_owned()
            ]
        );
    }

    #[test]
    fn watch_template_generates_watch_manifest_and_delegate_source() {
        let schema_path = "/tmp/.orbi/schemas/apple-app.v1.json";
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbi.exampleapp".to_owned(),
                template: InitTemplate::IosWatchCompanion,
                asc: test_init_asc(vec![test_init_device(
                    ASC_IOS_DEVICE_ID,
                    DeviceFamily::Ios,
                    TEST_IOS_UDID,
                    "Your iPhone",
                )]),
            },
            schema_path,
        );

        assert_eq!(
            plan.manifest,
            json!({
                "$schema": schema_path,
                "_description": MANIFEST_DESCRIPTION,
                "name": "Example App",
                "bundle_id": "dev.orbi.exampleapp",
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
                },
                "asc": {
                    "$schema": asc_sync::schema::PUBLISHED_SCHEMA_URL,
                    "_description": ASC_MANIFEST_DESCRIPTION,
                    "team_id": TEST_ASC_TEAM_ID,
                    "bundle_ids": {
                        "app": {
                            "bundle_id": "dev.orbi.exampleapp",
                            "name": "Example App",
                            "platform": "ios"
                        },
                        "watch-app": {
                            "bundle_id": "dev.orbi.exampleapp.watchkitapp",
                            "name": "Example App Watch App",
                            "platform": "ios"
                        },
                        "watch-extension": {
                            "bundle_id": "dev.orbi.exampleapp.watchkitapp.watchkitextension",
                            "name": "Example App Watch Extension",
                            "platform": "ios"
                        }
                    },
                    "devices": {
                        (ASC_IOS_DEVICE_ID): {
                            "family": "ios",
                            "udid": TEST_IOS_UDID,
                            "name": "Your iPhone"
                        }
                    },
                    "certs": {
                        "development": {
                            "type": "development",
                            "name": "Example App Development"
                        },
                        "distribution": {
                            "type": "distribution",
                            "name": "Example App Distribution"
                        }
                    },
                    "profiles": {
                        "app-development": {
                            "name": "Example App Development",
                            "type": "ios_app_development",
                            "bundle_id": "app",
                            "certs": ["development"],
                            "devices": [ASC_IOS_DEVICE_ID]
                        },
                        "app-adhoc": {
                            "name": "Example App Ad Hoc",
                            "type": "ios_app_adhoc",
                            "bundle_id": "app",
                            "certs": ["distribution"],
                            "devices": [ASC_IOS_DEVICE_ID]
                        },
                        "app-store": {
                            "name": "Example App App Store",
                            "type": "ios_app_store",
                            "bundle_id": "app",
                            "certs": ["distribution"],
                            "devices": []
                        },
                        "watch-app-development": {
                            "name": "Example App Watch App Development",
                            "type": "ios_app_development",
                            "bundle_id": "watch-app",
                            "certs": ["development"],
                            "devices": [ASC_IOS_DEVICE_ID]
                        },
                        "watch-app-adhoc": {
                            "name": "Example App Watch App Ad Hoc",
                            "type": "ios_app_adhoc",
                            "bundle_id": "watch-app",
                            "certs": ["distribution"],
                            "devices": [ASC_IOS_DEVICE_ID]
                        },
                        "watch-app-store": {
                            "name": "Example App Watch App App Store",
                            "type": "ios_app_store",
                            "bundle_id": "watch-app",
                            "certs": ["distribution"],
                            "devices": []
                        },
                        "watch-extension-development": {
                            "name": "Example App Watch Extension Development",
                            "type": "ios_app_development",
                            "bundle_id": "watch-extension",
                            "certs": ["development"],
                            "devices": [ASC_IOS_DEVICE_ID]
                        },
                        "watch-extension-adhoc": {
                            "name": "Example App Watch Extension Ad Hoc",
                            "type": "ios_app_adhoc",
                            "bundle_id": "watch-extension",
                            "certs": ["distribution"],
                            "devices": [ASC_IOS_DEVICE_ID]
                        },
                        "watch-extension-store": {
                            "name": "Example App Watch Extension App Store",
                            "type": "ios_app_store",
                            "bundle_id": "watch-extension",
                            "certs": ["distribution"],
                            "devices": []
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
    fn multiplatform_template_scaffolds_universal_asc_bundle() {
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbi.exampleapp".to_owned(),
                template: InitTemplate::AppleMultiplatform,
                asc: test_init_asc(vec![
                    test_init_device(
                        ASC_IOS_DEVICE_ID,
                        DeviceFamily::Ios,
                        TEST_IOS_UDID,
                        "Your iPhone",
                    ),
                    test_init_device(
                        ASC_MAC_DEVICE_ID,
                        DeviceFamily::Macos,
                        TEST_MAC_UDID,
                        "This Mac",
                    ),
                ]),
            },
            "/tmp/.orbi/schemas/apple-app.v1.json",
        );

        assert_eq!(
            plan.manifest["asc"]["_description"],
            json!(ASC_MANIFEST_DESCRIPTION)
        );
        assert_eq!(
            plan.manifest["asc"]["bundle_ids"]["app"]["platform"],
            "universal"
        );
        assert_eq!(
            plan.manifest["asc"]["profiles"]["developer-id"]["type"],
            "mac_app_direct"
        );
        assert_eq!(
            plan.manifest["asc"]["profiles"]["ios-development"]["devices"],
            json!([ASC_IOS_DEVICE_ID])
        );
        assert_eq!(
            plan.manifest["asc"]["profiles"]["mac-development"]["devices"],
            json!([ASC_MAC_DEVICE_ID])
        );
    }

    #[test]
    fn visionos_template_scaffolds_release_only_asc_until_device_profiles_exist() {
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Vision Example".to_owned(),
                bundle_id: "dev.orbi.visionexample".to_owned(),
                template: InitTemplate::Visionos,
                asc: test_init_asc(vec![]),
            },
            "/tmp/.orbi/schemas/apple-app.v1.json",
        );

        assert_eq!(plan.manifest["asc"]["bundle_ids"]["app"]["platform"], "ios");
        assert_eq!(
            plan.manifest["asc"]["_description"],
            json!(ASC_MANIFEST_DESCRIPTION)
        );
        assert_eq!(
            plan.manifest["asc"]["profiles"]["app-store"]["type"],
            "ios_app_store"
        );
        assert!(
            plan.manifest["asc"]["devices"]
                .as_object()
                .expect("devices is an object")
                .is_empty()
        );
        assert_eq!(
            plan.manifest["asc"]["certs"]
                .as_object()
                .expect("certs is an object")
                .len(),
            1
        );
    }

    #[test]
    fn create_scaffold_writes_manifest_directories_and_sources() {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = temp.path().join("nested/orbi.json");
        let plan = scaffold_plan(
            &InitAnswers {
                ecosystem: InitEcosystem::Apple,
                name: "Example App".to_owned(),
                bundle_id: "dev.orbi.exampleapp".to_owned(),
                template: InitTemplate::AppleMultiplatform,
                asc: test_init_asc(vec![
                    test_init_device(
                        ASC_IOS_DEVICE_ID,
                        DeviceFamily::Ios,
                        TEST_IOS_UDID,
                        "Your iPhone",
                    ),
                    test_init_device(
                        ASC_MAC_DEVICE_ID,
                        DeviceFamily::Macos,
                        TEST_MAC_UDID,
                        "This Mac",
                    ),
                ]),
            },
            "/tmp/.orbi/schemas/apple-app.v1.json",
        );

        create_scaffold(manifest_path.parent().unwrap(), &manifest_path, &plan).unwrap();

        let contents = std::fs::read_to_string(&manifest_path).unwrap();
        let resources_index = contents.find("\"resources\"").unwrap();
        let asc_index = contents.find("\"asc\"").unwrap();
        assert!(resources_index < asc_index);

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
        let gitignore =
            std::fs::read_to_string(manifest_path.parent().unwrap().join(".gitignore")).unwrap();
        assert_eq!(gitignore, ".bsp/\n.orbi/\n");
    }
}
