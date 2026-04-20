use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use plist::Value as PlistValue;
use semver::Version;
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::apple::xcode::validate_requested_xcode_version;

use super::authoring::{
    AccessorySetupSupport, AppManifest, BroadcastUploadProcessMode, DependencySpec,
    EntitlementsManifest, ExportedTypeDeclarationConfig, ExtensionConfig, ExtensionKind,
    FileProviderActionConfig, InfoManifest, PhotoProjectCategory,
};
use super::entitlements::build_entitlements_dictionary;
use super::{
    ApplePlatform, ExtensionEntry, ExtensionManifest, ExtensionRuntime, IosTargetManifest,
    PlatformManifest, PushManifest, ResolvedManifest, SwiftPackageDependency, SwiftPackageSource,
    TargetKind, TargetManifest, XcframeworkDependency,
};
use crate::util::ensure_dir;

struct NormalizedExternalDependencies {
    frameworks: Vec<String>,
    weak_frameworks: Vec<String>,
    system_libraries: Vec<String>,
    xcframeworks: Vec<XcframeworkDependency>,
    swift_packages: Vec<SwiftPackageDependency>,
}

struct NormalizedExtensionKind {
    target_kind: TargetKind,
    runtime: ExtensionRuntime,
    point_identifier: String,
    default_entry: DefaultExtensionEntry,
}

#[derive(Default)]
struct NormalizedExtensionMetadata {
    info_plist_extra: BTreeMap<String, JsonValue>,
    extension_extra: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Copy)]
enum DefaultExtensionEntry {
    None,
    PrincipalClassRequired,
    MainStoryboard(&'static str),
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn load_manifest(path: &Path, orbi_dir: &Path) -> Result<ResolvedManifest> {
    load_manifest_with_env(path, orbi_dir, None)
}

pub fn load_manifest_with_env(
    path: &Path,
    orbi_dir: &Path,
    env: Option<&str>,
) -> Result<ResolvedManifest> {
    let manifest_value = crate::manifest::read_manifest_value(path, env)?;
    let manifest: AppManifest = serde_json::from_value(manifest_value)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    normalize_manifest(path, orbi_dir, manifest)
}

fn normalize_manifest(_path: &Path, orbi_dir: &Path, app: AppManifest) -> Result<ResolvedManifest> {
    validate_semver_version(&app.version)?;
    validate_root_manifest(&app)?;

    let generated_entitlements_dir = orbi_dir.join("manifest").join("entitlements");
    ensure_dir(&generated_entitlements_dir)?;

    let non_watch_platforms = app
        .platforms
        .keys()
        .copied()
        .filter(|platform| *platform != ApplePlatform::Watchos)
        .collect::<Vec<_>>();

    let mut manifest = ResolvedManifest {
        name: app.name.clone(),
        version: app.version.clone(),
        xcode: app.xcode.clone(),
        hooks: app.hooks.clone().unwrap_or_default(),
        tests: app.tests.clone().unwrap_or_default(),
        quality: app.quality.clone(),
        platforms: app
            .platforms
            .iter()
            .map(|(platform, deployment_target)| {
                (
                    *platform,
                    PlatformManifest {
                        deployment_target: deployment_target.clone(),
                        universal_binary: *platform == ApplePlatform::Macos
                            && app.macos.universal_binary,
                    },
                )
            })
            .collect(),
        targets: Vec::new(),
    };

    let mut root_dependencies = Vec::new();

    for (id, extension) in &app.extensions {
        let target_name = format!("{}Extension", pascal_case(id));
        root_dependencies.push(target_name.clone());
        manifest.targets.push(build_extension_target(
            &app,
            id,
            extension,
            &target_name,
            &generated_entitlements_dir,
            &non_watch_platforms,
        )?);
    }

    if let Some(watch) = &app.watch {
        let watch_extension_name = "WatchExtension".to_owned();
        let watch_app_name = "WatchApp".to_owned();
        root_dependencies.push(watch_app_name.clone());

        manifest.targets.push(build_watch_extension_target(
            &app,
            &watch_extension_name,
            &generated_entitlements_dir,
        )?);
        manifest.targets.push(build_watch_app_target(
            &app,
            &watch_app_name,
            &watch_extension_name,
            watch,
            &generated_entitlements_dir,
        )?);
    }

    if app.app_clip.is_some() {
        let target_name = "AppClip".to_owned();
        root_dependencies.push(target_name.clone());
        manifest.targets.push(build_app_clip_target(
            &app,
            &target_name,
            &generated_entitlements_dir,
        )?);
    }

    let root_entitlements =
        write_entitlements_if_needed(&generated_entitlements_dir, "app", &app.entitlements, None)?;
    let external_dependencies = normalize_external_dependencies(&app.dependencies)?;

    manifest.targets.push(TargetManifest {
        name: app.name.clone(),
        kind: TargetKind::App,
        bundle_id: app.bundle_id.clone(),
        display_name: app.display_name.clone(),
        build_number: Some(app.build.to_string()),
        platforms: non_watch_platforms,
        sources: app.sources.clone(),
        resources: app.resources.clone(),
        dependencies: root_dependencies,
        frameworks: external_dependencies.frameworks,
        weak_frameworks: external_dependencies.weak_frameworks,
        system_libraries: external_dependencies.system_libraries,
        xcframeworks: external_dependencies.xcframeworks,
        swift_packages: external_dependencies.swift_packages,
        info_plist: normalize_info_plist(&app.info),
        ios: Some(IosTargetManifest::default()),
        entitlements: root_entitlements,
        push: normalize_push(
            app.entitlements.push_notifications,
            app.push_broadcast_for_live_activities,
        ),
        extension: None,
    });

    validate_internal_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_root_manifest(app: &AppManifest) -> Result<()> {
    if app.name.trim().is_empty() {
        bail!("manifest must declare a non-empty `name`");
    }
    if app.bundle_id.trim().is_empty() {
        bail!("manifest must declare a non-empty `bundle_id`");
    }
    if app.platforms.is_empty() {
        bail!("manifest must declare at least one platform");
    }
    if let Some(version) = app.xcode.as_deref() {
        validate_requested_xcode_version(version)?;
    }
    if app.watch.is_some() {
        if !app.platforms.contains_key(&ApplePlatform::Ios) {
            bail!("`watch` requires the root app to include the `ios` platform");
        }
        if !app.platforms.contains_key(&ApplePlatform::Watchos) {
            bail!("`watch` requires `platforms.watchos`");
        }
    }
    if app.app_clip.is_some() && !app.platforms.contains_key(&ApplePlatform::Ios) {
        bail!("`app_clip` requires the root app to include the `ios` platform");
    }
    if app.macos.universal_binary && !app.platforms.contains_key(&ApplePlatform::Macos) {
        bail!("`macos.universal_binary` requires `platforms.macos`");
    }
    let non_watch_platforms = app
        .platforms
        .keys()
        .filter(|platform| **platform != ApplePlatform::Watchos)
        .count();
    if non_watch_platforms == 0 {
        bail!("the root app must declare at least one non-watch platform");
    }
    validate_test_target_manifest(
        "tests.unit",
        app.tests.as_ref().and_then(|tests| tests.unit.as_deref()),
    )?;
    validate_test_target_manifest(
        "tests.ui",
        app.tests.as_ref().and_then(|tests| tests.ui.as_deref()),
    )?;
    validate_embedded_asc_config(app.asc.as_ref())?;
    Ok(())
}

fn validate_embedded_asc_config(config: Option<&JsonValue>) -> Result<()> {
    let Some(config) = config else {
        return Ok(());
    };
    let parsed: asc_sync::config::Config =
        serde_json::from_value(config.clone()).context("failed to parse manifest `asc` section")?;
    parsed
        .validate()
        .context("manifest `asc` section is invalid")
}

fn validate_test_target_manifest(field_name: &str, sources: Option<&[PathBuf]>) -> Result<()> {
    if let Some(sources) = sources
        && sources.is_empty()
    {
        bail!("`{field_name}` must declare at least one source root");
    }
    Ok(())
}

fn validate_semver_version(version: &str) -> Result<()> {
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()))
    {
        bail!("`version` must use Apple-friendly `x.y.z` numeric format");
    }
    Ok(())
}

fn build_extension_target(
    app: &AppManifest,
    id: &str,
    extension: &ExtensionConfig,
    target_name: &str,
    generated_entitlements_dir: &Path,
    default_platforms: &[ApplePlatform],
) -> Result<TargetManifest> {
    let platforms = if extension.platforms.is_empty() {
        default_platforms.to_vec()
    } else {
        extension.platforms.clone()
    };
    let bundle_id = format!("{}.{}", app.bundle_id, id);
    let normalized_kind = normalize_extension_kind(extension.kind);
    let entry = resolve_extension_entry(
        extension.entry.as_ref(),
        &normalized_kind,
        format!("extensions.{id}.entry"),
    )?;
    let metadata = build_extension_metadata(id, extension, &bundle_id)?;
    let external_dependencies = normalize_external_dependencies(&extension.dependencies)?;

    Ok(TargetManifest {
        name: target_name.to_owned(),
        kind: normalized_kind.target_kind,
        bundle_id,
        display_name: None,
        build_number: Some(app.build.to_string()),
        platforms,
        sources: extension.sources.clone(),
        resources: extension.resources.clone(),
        dependencies: Vec::new(),
        frameworks: external_dependencies.frameworks,
        weak_frameworks: external_dependencies.weak_frameworks,
        system_libraries: external_dependencies.system_libraries,
        xcframeworks: external_dependencies.xcframeworks,
        swift_packages: external_dependencies.swift_packages,
        info_plist: normalize_info_plist(&extension.info),
        ios: None,
        entitlements: write_entitlements_if_needed(
            generated_entitlements_dir,
            id,
            &extension.entitlements,
            None,
        )?,
        push: normalize_push(extension.entitlements.push_notifications, false),
        extension: Some(ExtensionManifest {
            runtime: normalized_kind.runtime,
            entry,
            point_identifier: normalized_kind.point_identifier,
            info_plist_extra: metadata.info_plist_extra,
            extra: metadata.extension_extra,
        }),
    })
}

fn build_watch_app_target(
    app: &AppManifest,
    target_name: &str,
    watch_extension_name: &str,
    watch: &super::authoring::WatchConfig,
    generated_entitlements_dir: &Path,
) -> Result<TargetManifest> {
    let external_dependencies = normalize_external_dependencies(&watch.dependencies)?;
    Ok(TargetManifest {
        name: target_name.to_owned(),
        kind: TargetKind::WatchApp,
        bundle_id: format!("{}.watchkitapp", app.bundle_id),
        display_name: None,
        build_number: Some(app.build.to_string()),
        platforms: vec![ApplePlatform::Watchos],
        sources: watch.sources.clone(),
        resources: watch.resources.clone(),
        dependencies: vec![watch_extension_name.to_owned()],
        frameworks: external_dependencies.frameworks,
        weak_frameworks: external_dependencies.weak_frameworks,
        system_libraries: external_dependencies.system_libraries,
        xcframeworks: external_dependencies.xcframeworks,
        swift_packages: external_dependencies.swift_packages,
        info_plist: normalize_info_plist(&watch.info),
        ios: None,
        entitlements: write_entitlements_if_needed(
            generated_entitlements_dir,
            "watch-app",
            &watch.entitlements,
            None,
        )?,
        push: normalize_push(watch.entitlements.push_notifications, false),
        extension: None,
    })
}

fn build_watch_extension_target(
    app: &AppManifest,
    target_name: &str,
    generated_entitlements_dir: &Path,
) -> Result<TargetManifest> {
    let watch = app.watch.as_ref().context("watch manifest missing")?;
    let external_dependencies = normalize_external_dependencies(&watch.extension.dependencies)?;
    let principal_class =
        required_entry_class(&watch.extension.entry, "watch.extension.entry.class")?;
    Ok(TargetManifest {
        name: target_name.to_owned(),
        kind: TargetKind::WatchExtension,
        bundle_id: format!("{}.watchkitapp.watchkitextension", app.bundle_id),
        display_name: None,
        build_number: Some(app.build.to_string()),
        platforms: vec![ApplePlatform::Watchos],
        sources: watch.extension.sources.clone(),
        resources: watch.extension.resources.clone(),
        dependencies: Vec::new(),
        frameworks: external_dependencies.frameworks,
        weak_frameworks: external_dependencies.weak_frameworks,
        system_libraries: external_dependencies.system_libraries,
        xcframeworks: external_dependencies.xcframeworks,
        swift_packages: external_dependencies.swift_packages,
        info_plist: normalize_info_plist(&watch.extension.info),
        ios: None,
        entitlements: write_entitlements_if_needed(
            generated_entitlements_dir,
            "watch-extension",
            &watch.extension.entitlements,
            None,
        )?,
        push: normalize_push(watch.extension.entitlements.push_notifications, false),
        extension: Some(ExtensionManifest {
            runtime: ExtensionRuntime::NsExtension,
            entry: ExtensionEntry::PrincipalClass(principal_class),
            point_identifier: "com.apple.watchkit".to_owned(),
            info_plist_extra: BTreeMap::new(),
            extra: BTreeMap::new(),
        }),
    })
}

fn build_app_clip_target(
    app: &AppManifest,
    target_name: &str,
    generated_entitlements_dir: &Path,
) -> Result<TargetManifest> {
    let app_clip = app.app_clip.as_ref().context("app clip manifest missing")?;
    let external_dependencies = normalize_external_dependencies(&app_clip.dependencies)?;
    Ok(TargetManifest {
        name: target_name.to_owned(),
        kind: TargetKind::App,
        bundle_id: format!("{}.clip", app.bundle_id),
        display_name: None,
        build_number: Some(app.build.to_string()),
        platforms: vec![ApplePlatform::Ios],
        sources: app_clip.sources.clone(),
        resources: app_clip.resources.clone(),
        dependencies: Vec::new(),
        frameworks: external_dependencies.frameworks,
        weak_frameworks: external_dependencies.weak_frameworks,
        system_libraries: external_dependencies.system_libraries,
        xcframeworks: external_dependencies.xcframeworks,
        swift_packages: external_dependencies.swift_packages,
        info_plist: normalize_info_plist(&app_clip.info),
        ios: Some(IosTargetManifest::default()),
        entitlements: write_entitlements_if_needed(
            generated_entitlements_dir,
            "app-clip",
            &app_clip.entitlements,
            Some(&app.bundle_id),
        )?,
        push: normalize_push(app_clip.entitlements.push_notifications, false),
        extension: None,
    })
}

fn normalize_extension_kind(kind: ExtensionKind) -> NormalizedExtensionKind {
    match kind {
        ExtensionKind::PacketTunnel => nsextension(
            "com.apple.networkextension.packet-tunnel",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::AppProxy => nsextension(
            "com.apple.networkextension.app-proxy",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::FilterControl => nsextension(
            "com.apple.networkextension.filter-control",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::FilterData => nsextension(
            "com.apple.networkextension.filter-data",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::DnsProxy => nsextension(
            "com.apple.networkextension.dns-proxy",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::AccountAuthenticationModification => nsextension(
            "com.apple.authentication-services-account-authentication-modification-ui",
            DefaultExtensionEntry::MainStoryboard("MainInterface"),
        ),
        ExtensionKind::Widget => NormalizedExtensionKind {
            target_kind: TargetKind::WidgetExtension,
            runtime: ExtensionRuntime::NsExtension,
            point_identifier: "com.apple.widgetkit-extension".to_owned(),
            default_entry: DefaultExtensionEntry::None,
        },
        ExtensionKind::Share => nsextension(
            "com.apple.share-services",
            DefaultExtensionEntry::MainStoryboard("MainInterface"),
        ),
        ExtensionKind::Safari => nsextension(
            "com.apple.Safari.extension",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::SafariWeb => nsextension(
            "com.apple.Safari.web-extension",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::QuickLookPreview => nsextension(
            "com.apple.quicklook.preview",
            DefaultExtensionEntry::MainStoryboard("MainInterface"),
        ),
        ExtensionKind::SpotlightImport => nsextension(
            "com.apple.spotlight.import",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::Thumbnail => nsextension(
            "com.apple.quicklook.thumbnail",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::CoreSpotlightDelegate => nsextension(
            "com.apple.spotlight.index",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::PersistentToken => {
            nsextension("com.apple.ctk-tokens", DefaultExtensionEntry::None)
        }
        ExtensionKind::XcodeSourceEditor => nsextension(
            "com.apple.dt.Xcode.extension.source-editor",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::Mail => nsextension(
            "com.apple.email.extension",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::BroadcastUpload => nsextension(
            "com.apple.broadcast-services-upload",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::BroadcastSetupUi => nsextension(
            "com.apple.broadcast-services-setupui",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::VirtualConferenceProvider => nsextension(
            "com.apple.calendar.virtualconference",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::NotificationService => nsextension(
            "com.apple.usernotifications.service",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::NotificationContent => nsextension(
            "com.apple.usernotifications.content-extension",
            DefaultExtensionEntry::MainStoryboard("MainInterface"),
        ),
        ExtensionKind::PhotoEditing => nsextension(
            "com.apple.photo-editing",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::PhotoProject => nsextension(
            "com.apple.photo-project",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::AuthenticationServices => nsextension(
            "com.apple.AppSSO.idp-extension",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::ActionUi => nsextension(
            "com.apple.ui-services",
            DefaultExtensionEntry::MainStoryboard("MainInterface"),
        ),
        ExtensionKind::ActionService => nsextension(
            "com.apple.services",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::ContentBlocker => nsextension(
            "com.apple.Safari.content-blocker",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::ClasskitContextProvider => nsextension(
            "com.apple.classkit.context-provider",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::FileProvider => nsextension(
            "com.apple.fileprovider-nonui",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::FileProviderUi => nsextension(
            "com.apple.fileprovider-actionsui",
            DefaultExtensionEntry::MainStoryboard("MainInterface"),
        ),
        ExtensionKind::FinderSync => nsextension(
            "com.apple.FinderSync",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::AutofillCredentialProvider => nsextension(
            "com.apple.authentication-services-credential-provider-ui",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::CallDirectory => nsextension(
            "com.apple.callkit.call-directory",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::MessageFilter => nsextension(
            "com.apple.identitylookup.message-filter",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::UnwantedCommunicationReporting => nsextension(
            "com.apple.identitylookup.classification-ui",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::Intents => nsextension(
            "com.apple.intents-service",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::IntentsUi => nsextension(
            "com.apple.intents-ui-service",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::Matter => nsextension(
            "com.apple.matter.support.extension.device-setup",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::LocationPushService => nsextension(
            "com.apple.location.push.service",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::PrintService => nsextension(
            "com.apple.printing.discovery",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::Messages => nsextension(
            "com.apple.message-payload-provider",
            DefaultExtensionEntry::MainStoryboard("MainInterface"),
        ),
        ExtensionKind::AudioUnit => nsextension(
            "com.apple.AudioUnit",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::AudioUnitUi => nsextension(
            "com.apple.AudioUnit-UI",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::TvTopShelf => nsextension(
            "com.apple.tv-top-shelf",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::CustomKeyboard => nsextension(
            "com.apple.keyboard-service",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::SmartCardToken => nsextension(
            "com.apple.ctk-tokens",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::DeviceActivityMonitor => nsextension(
            "com.apple.deviceactivity.monitor-extension",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::ShieldAction => nsextension(
            "com.apple.ManagedSettings.shield-action-service",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::ShieldConfiguration => nsextension(
            "com.apple.ManagedSettingsUI.shield-configuration-service",
            DefaultExtensionEntry::PrincipalClassRequired,
        ),
        ExtensionKind::AppIntents => extensionkit(
            "com.apple.appintents-extension",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::BackgroundDelivery => extensionkit(
            "com.apple.financekit.background-delivery",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::BackgroundResourceUpload => extensionkit(
            "com.apple.photos.background-upload",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::BackgroundDownload => extensionkit(
            "com.apple.background-asset-downloader-extension",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::MediaDeviceDiscovery => {
            extensionkit("com.apple.discovery-extension", DefaultExtensionEntry::None)
        }
        ExtensionKind::ContactProvider => extensionkit(
            "com.apple.contact.provider.extension",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::TranslationProvider => extensionkit(
            "com.apple.public.translation-ui-provider",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::AppMigration => {
            extensionkit("com.apple.app-migration", DefaultExtensionEntry::None)
        }
        ExtensionKind::Capture => {
            extensionkit("com.apple.securecapture", DefaultExtensionEntry::None)
        }
        ExtensionKind::HotspotEvaluationProvider => extensionkit(
            "com.apple.networkextension.hotspot-evaluation",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::HotspotAuthenticationProvider => extensionkit(
            "com.apple.networkextension.hotspot-authentication",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::AccessorySetup => extensionkit(
            "com.apple.accessory-setup-extension",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::AccessoryDataTransport => extensionkit(
            "com.apple.accessory-transport-extension",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::DeviceActivityReport => extensionkit(
            "com.apple.deviceactivityui.report-extension",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::UrlFilterNetwork => extensionkit(
            "com.apple.networkextension.url-filter-control",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::LiveCallerIdLookup => {
            extensionkit("com.apple.live-lookup", DefaultExtensionEntry::None)
        }
        ExtensionKind::IdentityDocumentProvider => extensionkit(
            "com.apple.identity-document-services.document-provider-ui",
            DefaultExtensionEntry::None,
        ),
        ExtensionKind::FileSystem => {
            extensionkit("com.apple.fskit.fsmodule", DefaultExtensionEntry::None)
        }
    }
}

fn nsextension(
    point_identifier: &'static str,
    default_entry: DefaultExtensionEntry,
) -> NormalizedExtensionKind {
    NormalizedExtensionKind {
        target_kind: TargetKind::AppExtension,
        runtime: ExtensionRuntime::NsExtension,
        point_identifier: point_identifier.to_owned(),
        default_entry,
    }
}

fn extensionkit(
    point_identifier: &'static str,
    default_entry: DefaultExtensionEntry,
) -> NormalizedExtensionKind {
    NormalizedExtensionKind {
        target_kind: TargetKind::AppExtension,
        runtime: ExtensionRuntime::ExtensionKit,
        point_identifier: point_identifier.to_owned(),
        default_entry,
    }
}

fn resolve_extension_entry(
    entry: Option<&super::authoring::EntryConfig>,
    normalized_kind: &NormalizedExtensionKind,
    field_name: String,
) -> Result<ExtensionEntry> {
    let configured = match entry {
        Some(config) => match (config.class.as_deref(), config.storyboard.as_deref()) {
            (Some(class), None) if !class.trim().is_empty() => {
                Some(ExtensionEntry::PrincipalClass(class.trim().to_owned()))
            }
            (None, Some(storyboard)) if !storyboard.trim().is_empty() => {
                Some(ExtensionEntry::MainStoryboard(storyboard.trim().to_owned()))
            }
            (None, None) => bail!("`{field_name}` must set either `class` or `storyboard`"),
            (Some(_), Some(_)) => {
                bail!("`{field_name}` cannot set both `class` and `storyboard`")
            }
            (Some(_), None) => bail!("`{field_name}.class` must be a non-empty string"),
            (None, Some(_)) => bail!("`{field_name}.storyboard` must be a non-empty string"),
        },
        None => None,
    };

    match normalized_kind.runtime {
        ExtensionRuntime::NsExtension => match normalized_kind.default_entry {
            DefaultExtensionEntry::None => {
                if configured.is_some() {
                    bail!("`{field_name}` is not supported for this extension kind");
                }
                Ok(ExtensionEntry::None)
            }
            DefaultExtensionEntry::MainStoryboard(storyboard) => match configured {
                Some(ExtensionEntry::PrincipalClass(_)) => {
                    bail!("`{field_name}.class` is not supported for this extension kind")
                }
                Some(ExtensionEntry::MainStoryboard(storyboard)) => {
                    Ok(ExtensionEntry::MainStoryboard(storyboard))
                }
                Some(ExtensionEntry::None) | None => {
                    Ok(ExtensionEntry::MainStoryboard(storyboard.to_owned()))
                }
            },
            DefaultExtensionEntry::PrincipalClassRequired => match configured {
                Some(ExtensionEntry::PrincipalClass(class)) => {
                    Ok(ExtensionEntry::PrincipalClass(class))
                }
                Some(ExtensionEntry::MainStoryboard(_)) => {
                    bail!("`{field_name}.storyboard` is not supported for this extension kind")
                }
                Some(ExtensionEntry::None) | None => {
                    bail!("`{field_name}.class` is required for this extension kind")
                }
            },
        },
        ExtensionRuntime::ExtensionKit => match configured {
            Some(ExtensionEntry::MainStoryboard(_)) => {
                bail!("`{field_name}.storyboard` is not supported for ExtensionKit extension kinds")
            }
            Some(ExtensionEntry::PrincipalClass(class)) => {
                Ok(ExtensionEntry::PrincipalClass(class))
            }
            Some(ExtensionEntry::None) | None => Ok(ExtensionEntry::None),
        },
    }
}

fn required_entry_class(entry: &super::authoring::EntryConfig, field_name: &str) -> Result<String> {
    match (entry.class.as_deref(), entry.storyboard.as_deref()) {
        (Some(class), None) if !class.trim().is_empty() => Ok(class.trim().to_owned()),
        (None, None) => bail!("`{field_name}` is required"),
        (Some(_), Some(_)) => {
            bail!("`{field_name}` cannot be combined with `watch.extension.entry.storyboard`")
        }
        (None, Some(_)) => bail!("`watch.extension.entry.storyboard` is not supported"),
        (Some(_), None) => bail!("`{field_name}` must be a non-empty string"),
    }
}

fn build_extension_metadata(
    extension_id: &str,
    extension: &ExtensionConfig,
    bundle_id: &str,
) -> Result<NormalizedExtensionMetadata> {
    validate_extension_dsl_blocks(extension_id, extension)?;

    let mut metadata = NormalizedExtensionMetadata::default();
    match extension.kind {
        ExtensionKind::Share
        | ExtensionKind::ActionUi
        | ExtensionKind::ActionService
        | ExtensionKind::BroadcastSetupUi => {
            let mut attributes = default_action_attributes(extension.kind);
            if let Some(config) = &extension.action {
                attributes.insert(
                    "NSExtensionActivationRule".to_owned(),
                    config.activation_rule.clone(),
                );
                if let Some(file) = normalize_optional_string(
                    config.javascript_preprocessing_file.as_deref(),
                    &format!("extensions.{extension_id}.action.javascript_preprocessing_file"),
                )? {
                    attributes.insert(
                        "NSExtensionJavaScriptPreprocessingFile".to_owned(),
                        JsonValue::String(file),
                    );
                }
            }
            insert_extension_attributes(&mut metadata.extension_extra, attributes);
        }
        ExtensionKind::AccountAuthenticationModification => {
            let config = extension
                .account_authentication_modification
                .as_ref()
                .cloned()
                .unwrap_or(super::authoring::AccountAuthenticationModificationConfig {
                    supports_upgrade_to_sign_in_with_apple: true,
                    supports_strong_password_change: true,
                });
            insert_extension_attributes(
                &mut metadata.extension_extra,
                BTreeMap::from([
                    (
                        "ASAccountAuthenticationModificationSupportsUpgradeToSignInWithApple"
                            .to_owned(),
                        JsonValue::Bool(config.supports_upgrade_to_sign_in_with_apple),
                    ),
                    (
                        "ASAccountAuthenticationModificationSupportsStrongPasswordChange"
                            .to_owned(),
                        JsonValue::Bool(config.supports_strong_password_change),
                    ),
                ]),
            );
        }
        ExtensionKind::BroadcastUpload => {
            let process_mode = extension
                .broadcast_upload
                .as_ref()
                .map(|config| config.process_mode)
                .unwrap_or(BroadcastUploadProcessMode::SampleBuffer);
            metadata.extension_extra.insert(
                "RPBroadcastProcessMode".to_owned(),
                JsonValue::String(broadcast_upload_process_mode_name(process_mode).to_owned()),
            );
        }
        ExtensionKind::CoreSpotlightDelegate => {
            let label = extension
                .core_spotlight_delegate
                .as_ref()
                .and_then(|config| config.label.as_deref())
                .map(|value| {
                    require_non_empty_string(
                        value,
                        &format!("extensions.{extension_id}.core_spotlight_delegate.label"),
                    )
                })
                .transpose()?
                .unwrap_or_else(|| extension_id.to_owned());
            metadata
                .info_plist_extra
                .insert("CSExtensionLabel".to_owned(), JsonValue::String(label));
        }
        ExtensionKind::FileProvider => {
            if let Some(config) = &extension.file_provider {
                let document_group = require_non_empty_string(
                    &config.document_group,
                    &format!("extensions.{extension_id}.file_provider.document_group"),
                )?;
                metadata.extension_extra.insert(
                    "NSExtensionFileProviderDocumentGroup".to_owned(),
                    JsonValue::String(document_group),
                );
                metadata.extension_extra.insert(
                    "NSExtensionFileProviderSupportsEnumeration".to_owned(),
                    JsonValue::Bool(config.supports_enumeration),
                );
                if !config.actions.is_empty() {
                    metadata.extension_extra.insert(
                        "NSExtensionFileProviderActions".to_owned(),
                        file_provider_actions_value(
                            &config.actions,
                            &format!("extensions.{extension_id}.file_provider.actions"),
                        )?,
                    );
                }
            }
        }
        ExtensionKind::FileProviderUi => {
            if let Some(config) = &extension.file_provider_ui
                && !config.actions.is_empty()
            {
                metadata.extension_extra.insert(
                    "NSExtensionFileProviderActions".to_owned(),
                    file_provider_actions_value(
                        &config.actions,
                        &format!("extensions.{extension_id}.file_provider_ui.actions"),
                    )?,
                );
            }
        }
        ExtensionKind::Intents | ExtensionKind::IntentsUi => {
            let config = extension.intents.clone().unwrap_or_default();
            let supported = config
                .supported
                .iter()
                .map(|intent| {
                    require_non_empty_string(
                        intent,
                        &format!("extensions.{extension_id}.intents.supported"),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let mut attributes = BTreeMap::from([(
                "IntentsSupported".to_owned(),
                JsonValue::Array(supported.into_iter().map(JsonValue::String).collect()),
            )]);
            if matches!(extension.kind, ExtensionKind::Intents) {
                let restricted = config
                    .restricted_while_locked
                    .iter()
                    .map(|intent| {
                        require_non_empty_string(
                            intent,
                            &format!("extensions.{extension_id}.intents.restricted_while_locked"),
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                attributes.insert(
                    "IntentsRestrictedWhileLocked".to_owned(),
                    JsonValue::Array(restricted.into_iter().map(JsonValue::String).collect()),
                );
            }
            insert_extension_attributes(&mut metadata.extension_extra, attributes);
        }
        ExtensionKind::CustomKeyboard => {
            let config = extension
                .keyboard
                .clone()
                .unwrap_or(super::authoring::KeyboardConfig {
                    primary_language: "en-US".to_owned(),
                    ascii_capable: false,
                    prefers_right_to_left: false,
                    requests_open_access: false,
                });
            insert_extension_attributes(
                &mut metadata.extension_extra,
                BTreeMap::from([
                    (
                        "PrimaryLanguage".to_owned(),
                        JsonValue::String(require_non_empty_string(
                            &config.primary_language,
                            &format!("extensions.{extension_id}.keyboard.primary_language"),
                        )?),
                    ),
                    (
                        "IsASCIICapable".to_owned(),
                        JsonValue::Bool(config.ascii_capable),
                    ),
                    (
                        "PrefersRightToLeft".to_owned(),
                        JsonValue::Bool(config.prefers_right_to_left),
                    ),
                    (
                        "RequestsOpenAccess".to_owned(),
                        JsonValue::Bool(config.requests_open_access),
                    ),
                ]),
            );
        }
        ExtensionKind::MessageFilter => {
            if let Some(config) = &extension.message_filter
                && let Some(network_url) = normalize_optional_string(
                    config.network_url.as_deref(),
                    &format!("extensions.{extension_id}.message_filter.network_url"),
                )?
            {
                insert_extension_attributes(
                    &mut metadata.extension_extra,
                    BTreeMap::from([(
                        "ILMessageFilterExtensionNetworkURL".to_owned(),
                        JsonValue::String(network_url),
                    )]),
                );
            }
        }
        ExtensionKind::NotificationContent => {
            if let Some(config) = &extension.notification_content {
                if config.categories.is_empty() {
                    bail!(
                        "`extensions.{extension_id}.notification_content.categories` must contain at least one category"
                    );
                }
                let categories = config
                    .categories
                    .iter()
                    .map(|category| {
                        require_non_empty_string(
                            category,
                            &format!("extensions.{extension_id}.notification_content.categories"),
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut attributes = BTreeMap::from([(
                    "UNNotificationExtensionCategory".to_owned(),
                    match categories.as_slice() {
                        [single] => JsonValue::String(single.clone()),
                        _ => JsonValue::Array(
                            categories.into_iter().map(JsonValue::String).collect(),
                        ),
                    },
                )]);
                if let Some(size_ratio) = config.initial_content_size_ratio {
                    if !size_ratio.is_finite() || size_ratio <= 0.0 {
                        bail!(
                            "`extensions.{extension_id}.notification_content.initial_content_size_ratio` must be a positive number"
                        );
                    }
                    attributes.insert(
                        "UNNotificationExtensionInitialContentSizeRatio".to_owned(),
                        json!(size_ratio),
                    );
                }
                insert_extension_attributes(&mut metadata.extension_extra, attributes);
            }
        }
        ExtensionKind::PersistentToken => {
            let config = extension.persistent_token.as_ref().with_context(|| {
                format!(
                    "`extensions.{extension_id}.persistent_token` is required for `persistent-token` extensions"
                )
            })?;
            let driver_class = require_non_empty_string(
                &config.driver_class,
                &format!("extensions.{extension_id}.persistent_token.driver_class"),
            )?;
            let class_id = config
                .class_id
                .as_deref()
                .map(|value| {
                    require_non_empty_string(
                        value,
                        &format!("extensions.{extension_id}.persistent_token.class_id"),
                    )
                })
                .transpose()?
                .unwrap_or_else(|| bundle_id.to_owned());
            insert_extension_attributes(
                &mut metadata.extension_extra,
                BTreeMap::from([
                    (
                        "com.apple.ctk.class-id".to_owned(),
                        JsonValue::String(class_id),
                    ),
                    (
                        "com.apple.ctk.driver-class".to_owned(),
                        JsonValue::String(driver_class),
                    ),
                ]),
            );
        }
        ExtensionKind::PhotoProject => {
            if let Some(config) = &extension.photo_project {
                if config.categories.is_empty() {
                    bail!(
                        "`extensions.{extension_id}.photo_project.categories` must contain at least one category"
                    );
                }
                let document_type_identifier = config
                    .document_type_identifier
                    .as_deref()
                    .map(|value| {
                        require_non_empty_string(
                            value,
                            &format!(
                                "extensions.{extension_id}.photo_project.document_type_identifier"
                            ),
                        )
                    })
                    .transpose()?
                    .unwrap_or_else(|| format!("{bundle_id}-document-type"));
                insert_extension_attributes(
                    &mut metadata.extension_extra,
                    BTreeMap::from([
                        (
                            "PHProjectExtensionDefinesProjectTypes".to_owned(),
                            JsonValue::Bool(config.defines_project_types),
                        ),
                        (
                            "PHProjectCategory".to_owned(),
                            JsonValue::Array(
                                config
                                    .categories
                                    .iter()
                                    .map(|category| {
                                        JsonValue::String(
                                            photo_project_category_name(*category).to_owned(),
                                        )
                                    })
                                    .collect(),
                            ),
                        ),
                    ]),
                );
                metadata.info_plist_extra.insert(
                    "CFBundleDocumentTypes".to_owned(),
                    JsonValue::Array(vec![json!({
                        "CFBundleTypeRole": "Editor",
                        "LSItemContentTypes": [document_type_identifier],
                    })]),
                );
            }
        }
        ExtensionKind::QuickLookPreview => {
            let config = extension.quick_look_preview.clone().unwrap_or_default();
            insert_extension_attributes(
                &mut metadata.extension_extra,
                BTreeMap::from([
                    (
                        "QLSupportedContentTypes".to_owned(),
                        validated_string_array(
                            &config.content_types,
                            &format!("extensions.{extension_id}.quick_look_preview.content_types"),
                        )?,
                    ),
                    (
                        "QLSupportsSearchableItems".to_owned(),
                        JsonValue::Bool(config.searchable_items),
                    ),
                    (
                        "QLIsDataBasedPreview".to_owned(),
                        JsonValue::Bool(config.data_based),
                    ),
                ]),
            );
        }
        ExtensionKind::SpotlightImport => {
            let config = extension.spotlight_import.clone().unwrap_or(
                super::authoring::SpotlightImportConfig {
                    label: None,
                    content_types: Vec::new(),
                },
            );
            let label = config
                .label
                .as_deref()
                .map(|value| {
                    require_non_empty_string(
                        value,
                        &format!("extensions.{extension_id}.spotlight_import.label"),
                    )
                })
                .transpose()?
                .unwrap_or_else(|| format!("{extension_id}Importer"));
            metadata
                .info_plist_extra
                .insert("CSExtensionLabel".to_owned(), JsonValue::String(label));
            insert_extension_attributes(
                &mut metadata.extension_extra,
                BTreeMap::from([(
                    "CSSupportedContentTypes".to_owned(),
                    validated_string_array(
                        &config.content_types,
                        &format!("extensions.{extension_id}.spotlight_import.content_types"),
                    )?,
                )]),
            );
        }
        ExtensionKind::Thumbnail => {
            let config = extension.thumbnail.clone().unwrap_or_default();
            insert_extension_attributes(
                &mut metadata.extension_extra,
                BTreeMap::from([
                    (
                        "QLSupportedContentTypes".to_owned(),
                        validated_string_array(
                            &config.content_types,
                            &format!("extensions.{extension_id}.thumbnail.content_types"),
                        )?,
                    ),
                    (
                        "QLThumbnailMinimumDimension".to_owned(),
                        json!(config.minimum_dimension),
                    ),
                ]),
            );
        }
        ExtensionKind::UnwantedCommunicationReporting => {
            if let Some(config) = &extension.unwanted_communication_reporting
                && let Some(destination) = normalize_optional_string(
                    config.sms_report_destination.as_deref(),
                    &format!(
                        "extensions.{extension_id}.unwanted_communication_reporting.sms_report_destination"
                    ),
                )?
            {
                insert_extension_attributes(
                    &mut metadata.extension_extra,
                    BTreeMap::from([(
                        "ILClassificationExtensionSMSReportDestination".to_owned(),
                        JsonValue::String(destination),
                    )]),
                );
            }
        }
        ExtensionKind::AccessorySetup => {
            if let Some(config) = &extension.accessory_setup {
                if !config.bluetooth_services.is_empty() {
                    metadata.info_plist_extra.insert(
                        "NSBluetoothServices".to_owned(),
                        validated_string_array(
                            &config.bluetooth_services,
                            &format!(
                                "extensions.{extension_id}.accessory_setup.bluetooth_services"
                            ),
                        )?,
                    );
                }
                if !config.exported_types.is_empty() {
                    metadata.info_plist_extra.insert(
                        "UTExportedTypeDeclarations".to_owned(),
                        exported_type_declarations_value(
                            &config.exported_types,
                            &format!("extensions.{extension_id}.accessory_setup.exported_types"),
                        )?,
                    );
                }
            }
        }
        ExtensionKind::AccessoryDataTransport => {
            let config = extension.accessory_data_transport.clone().unwrap_or(
                super::authoring::AccessoryDataTransportConfig {
                    bluetooth_services: Vec::new(),
                    supports: vec![AccessorySetupSupport::Bluetooth],
                    exported_types: Vec::new(),
                },
            );
            metadata.info_plist_extra.insert(
                "NSAccessorySetupKitSupports".to_owned(),
                JsonValue::Array(
                    config
                        .supports
                        .iter()
                        .map(|value| {
                            JsonValue::String(accessory_setup_support_name(*value).to_owned())
                        })
                        .collect(),
                ),
            );
            if !config.bluetooth_services.is_empty() {
                metadata.info_plist_extra.insert(
                    "NSAccessorySetupBluetoothServices".to_owned(),
                    validated_string_array(
                        &config.bluetooth_services,
                        &format!(
                            "extensions.{extension_id}.accessory_data_transport.bluetooth_services"
                        ),
                    )?,
                );
            }
            if !config.exported_types.is_empty() {
                metadata.info_plist_extra.insert(
                    "UTExportedTypeDeclarations".to_owned(),
                    exported_type_declarations_value(
                        &config.exported_types,
                        &format!(
                            "extensions.{extension_id}.accessory_data_transport.exported_types"
                        ),
                    )?,
                );
            }
        }
        ExtensionKind::BackgroundResourceUpload => {
            let config = extension.background_resource_upload.as_ref().with_context(|| {
                format!(
                    "`extensions.{extension_id}.background_resource_upload` is required for `background-resource-upload` extensions"
                )
            })?;
            metadata.info_plist_extra.insert(
                "BackgroundUploadURLBase".to_owned(),
                JsonValue::String(require_non_empty_string(
                    &config.url_base,
                    &format!("extensions.{extension_id}.background_resource_upload.url_base"),
                )?),
            );
        }
        _ => {}
    }

    Ok(metadata)
}

fn validate_extension_dsl_blocks(extension_id: &str, extension: &ExtensionConfig) -> Result<()> {
    validate_extension_block_support(
        extension_id,
        "action",
        extension.action.is_some(),
        matches!(
            extension.kind,
            ExtensionKind::Share
                | ExtensionKind::ActionUi
                | ExtensionKind::ActionService
                | ExtensionKind::BroadcastSetupUi
        ),
        "`share`, `action-ui`, `action-service`, or `broadcast-setup-ui`",
    )?;
    validate_extension_block_support(
        extension_id,
        "account_authentication_modification",
        extension.account_authentication_modification.is_some(),
        matches!(
            extension.kind,
            ExtensionKind::AccountAuthenticationModification
        ),
        "`account-authentication-modification`",
    )?;
    validate_extension_block_support(
        extension_id,
        "broadcast_upload",
        extension.broadcast_upload.is_some(),
        matches!(extension.kind, ExtensionKind::BroadcastUpload),
        "`broadcast-upload`",
    )?;
    validate_extension_block_support(
        extension_id,
        "core_spotlight_delegate",
        extension.core_spotlight_delegate.is_some(),
        matches!(extension.kind, ExtensionKind::CoreSpotlightDelegate),
        "`core-spotlight-delegate`",
    )?;
    validate_extension_block_support(
        extension_id,
        "file_provider",
        extension.file_provider.is_some(),
        matches!(extension.kind, ExtensionKind::FileProvider),
        "`file-provider`",
    )?;
    validate_extension_block_support(
        extension_id,
        "file_provider_ui",
        extension.file_provider_ui.is_some(),
        matches!(extension.kind, ExtensionKind::FileProviderUi),
        "`file-provider-ui`",
    )?;
    validate_extension_block_support(
        extension_id,
        "intents",
        extension.intents.is_some(),
        matches!(
            extension.kind,
            ExtensionKind::Intents | ExtensionKind::IntentsUi
        ),
        "`intents` or `intents-ui`",
    )?;
    validate_extension_block_support(
        extension_id,
        "keyboard",
        extension.keyboard.is_some(),
        matches!(extension.kind, ExtensionKind::CustomKeyboard),
        "`custom-keyboard`",
    )?;
    validate_extension_block_support(
        extension_id,
        "message_filter",
        extension.message_filter.is_some(),
        matches!(extension.kind, ExtensionKind::MessageFilter),
        "`message-filter`",
    )?;
    validate_extension_block_support(
        extension_id,
        "notification_content",
        extension.notification_content.is_some(),
        matches!(extension.kind, ExtensionKind::NotificationContent),
        "`notification-content`",
    )?;
    validate_extension_block_support(
        extension_id,
        "persistent_token",
        extension.persistent_token.is_some(),
        matches!(extension.kind, ExtensionKind::PersistentToken),
        "`persistent-token`",
    )?;
    validate_extension_block_support(
        extension_id,
        "photo_project",
        extension.photo_project.is_some(),
        matches!(extension.kind, ExtensionKind::PhotoProject),
        "`photo-project`",
    )?;
    validate_extension_block_support(
        extension_id,
        "quick_look_preview",
        extension.quick_look_preview.is_some(),
        matches!(extension.kind, ExtensionKind::QuickLookPreview),
        "`quick-look-preview`",
    )?;
    validate_extension_block_support(
        extension_id,
        "spotlight_import",
        extension.spotlight_import.is_some(),
        matches!(extension.kind, ExtensionKind::SpotlightImport),
        "`spotlight-import`",
    )?;
    validate_extension_block_support(
        extension_id,
        "thumbnail",
        extension.thumbnail.is_some(),
        matches!(extension.kind, ExtensionKind::Thumbnail),
        "`thumbnail`",
    )?;
    validate_extension_block_support(
        extension_id,
        "unwanted_communication_reporting",
        extension.unwanted_communication_reporting.is_some(),
        matches!(
            extension.kind,
            ExtensionKind::UnwantedCommunicationReporting
        ),
        "`unwanted-communication-reporting`",
    )?;
    validate_extension_block_support(
        extension_id,
        "accessory_setup",
        extension.accessory_setup.is_some(),
        matches!(extension.kind, ExtensionKind::AccessorySetup),
        "`accessory-setup`",
    )?;
    validate_extension_block_support(
        extension_id,
        "accessory_data_transport",
        extension.accessory_data_transport.is_some(),
        matches!(extension.kind, ExtensionKind::AccessoryDataTransport),
        "`accessory-data-transport`",
    )?;
    validate_extension_block_support(
        extension_id,
        "background_resource_upload",
        extension.background_resource_upload.is_some(),
        matches!(extension.kind, ExtensionKind::BackgroundResourceUpload),
        "`background-resource-upload`",
    )?;
    Ok(())
}

fn validate_extension_block_support(
    extension_id: &str,
    block_name: &str,
    present: bool,
    supported: bool,
    kind_description: &str,
) -> Result<()> {
    if present && !supported {
        bail!(
            "`extensions.{extension_id}.{block_name}` is only supported for {kind_description} extensions"
        );
    }
    Ok(())
}

fn require_non_empty_string(value: &str, field_name: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("`{field_name}` must be a non-empty string");
    }
    Ok(trimmed.to_owned())
}

fn normalize_optional_string(value: Option<&str>, field_name: &str) -> Result<Option<String>> {
    value
        .map(|value| require_non_empty_string(value, field_name))
        .transpose()
}

fn insert_extension_attributes(
    extension_extra: &mut BTreeMap<String, JsonValue>,
    attributes: BTreeMap<String, JsonValue>,
) {
    if attributes.is_empty() {
        return;
    }
    extension_extra.insert(
        "NSExtensionAttributes".to_owned(),
        JsonValue::Object(JsonMap::from_iter(attributes)),
    );
}

fn file_provider_actions_value(
    actions: &[FileProviderActionConfig],
    field_name: &str,
) -> Result<JsonValue> {
    let mut values = Vec::with_capacity(actions.len());
    for (index, action) in actions.iter().enumerate() {
        let identifier = require_non_empty_string(
            &action.identifier,
            &format!("{field_name}[{index}].identifier"),
        )?;
        let name = require_non_empty_string(&action.name, &format!("{field_name}[{index}].name"))?;
        values.push(json!({
            "NSExtensionFileProviderActionIdentifier": identifier,
            "NSExtensionFileProviderActionName": name,
            "NSExtensionFileProviderActionActivationRule": action.activation_rule.clone(),
        }));
    }
    Ok(JsonValue::Array(values))
}

fn default_action_attributes(kind: ExtensionKind) -> BTreeMap<String, JsonValue> {
    let activation_rule = match kind {
        ExtensionKind::BroadcastSetupUi => {
            json!({ "NSExtensionActivationSupportsReplayKitStreaming": true })
        }
        ExtensionKind::ActionService => json!({
            "NSExtensionActivationSupportsText": false,
            "NSExtensionActivationSupportsWebURLWithMaxCount": 1,
            "NSExtensionActivationSupportsImageWithMaxCount": 0,
            "NSExtensionActivationSupportsMovieWithMaxCount": 0,
            "NSExtensionActivationSupportsFileWithMaxCount": 0,
        }),
        _ => JsonValue::String("TRUEPREDICATE".to_owned()),
    };
    BTreeMap::from([("NSExtensionActivationRule".to_owned(), activation_rule)])
}

fn validated_string_array(values: &[String], field_name: &str) -> Result<JsonValue> {
    Ok(JsonValue::Array(
        values
            .iter()
            .map(|value| require_non_empty_string(value, field_name).map(JsonValue::String))
            .collect::<Result<Vec<_>>>()?,
    ))
}

fn accessory_setup_support_name(value: AccessorySetupSupport) -> &'static str {
    match value {
        AccessorySetupSupport::Bluetooth => "Bluetooth",
    }
}

fn exported_type_declarations_value(
    declarations: &[ExportedTypeDeclarationConfig],
    field_name: &str,
) -> Result<JsonValue> {
    let mut values = Vec::with_capacity(declarations.len());
    for (index, declaration) in declarations.iter().enumerate() {
        let item_field = format!("{field_name}[{index}]");
        let identifier =
            require_non_empty_string(&declaration.identifier, &format!("{item_field}.identifier"))?;
        let conforms_to = declaration
            .conforms_to
            .iter()
            .map(|value| require_non_empty_string(value, &format!("{item_field}.conforms_to")))
            .collect::<Result<Vec<_>>>()?;
        if conforms_to.is_empty() {
            bail!("`{item_field}.conforms_to` must contain at least one identifier");
        }
        let mut declaration_value = JsonMap::from_iter([
            ("UTTypeIdentifier".to_owned(), JsonValue::String(identifier)),
            (
                "UTTypeConformsTo".to_owned(),
                JsonValue::Array(conforms_to.into_iter().map(JsonValue::String).collect()),
            ),
        ]);
        if let Some(description) = normalize_optional_string(
            declaration.description.as_deref(),
            &format!("{item_field}.description"),
        )? {
            declaration_value.insert(
                "UTTypeDescription".to_owned(),
                JsonValue::String(description),
            );
        }
        if let Some(symbol_name) = normalize_optional_string(
            declaration.symbol_name.as_deref(),
            &format!("{item_field}.symbol_name"),
        )? {
            declaration_value.insert(
                "UTTypeIcons".to_owned(),
                json!({
                    "UTTypeSymbolName": symbol_name,
                }),
            );
        }
        values.push(JsonValue::Object(declaration_value));
    }
    Ok(JsonValue::Array(values))
}

fn broadcast_upload_process_mode_name(process_mode: BroadcastUploadProcessMode) -> &'static str {
    match process_mode {
        BroadcastUploadProcessMode::SampleBuffer => "RPBroadcastProcessModeSampleBuffer",
    }
}

fn photo_project_category_name(category: PhotoProjectCategory) -> &'static str {
    match category {
        PhotoProjectCategory::Book => "book",
        PhotoProjectCategory::Calendar => "calendar",
        PhotoProjectCategory::Card => "card",
        PhotoProjectCategory::Prints => "prints",
        PhotoProjectCategory::Slideshow => "slideshow",
        PhotoProjectCategory::Walldecor => "walldecor",
        PhotoProjectCategory::Other => "other",
        PhotoProjectCategory::Undefined => "undefined",
    }
}

fn normalize_external_dependencies(
    dependencies: &BTreeMap<String, DependencySpec>,
) -> Result<NormalizedExternalDependencies> {
    let mut frameworks = Vec::new();
    let mut weak_frameworks = Vec::new();
    let system_libraries = Vec::new();
    let mut xcframeworks = Vec::new();
    let mut swift_packages = Vec::new();

    for (name, dependency) in dependencies {
        let choices = [
            dependency.path.is_some(),
            dependency.git.is_some(),
            dependency.framework == Some(true),
            dependency.xcframework.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if choices != 1 {
            bail!(
                "dependency `{name}` must set exactly one of `path`, `git`, `framework`, or `xcframework`"
            );
        }
        if (dependency.revision.is_some() || dependency.version.is_some())
            && dependency.git.is_none()
        {
            bail!("dependency `{name}` can only set `version` or `revision` together with `git`");
        }
        if let Some(path) = &dependency.path {
            swift_packages.push(SwiftPackageDependency {
                product: name.clone(),
                source: SwiftPackageSource::Path { path: path.clone() },
            });
            continue;
        }
        if let Some(url) = &dependency.git {
            let version = dependency.version.clone();
            if let Some(version) = version.as_deref() {
                validate_git_version(name, version)?;
            }
            let revision = dependency.revision.clone();
            if let Some(revision) = revision.as_deref() {
                validate_git_revision(name, revision)?;
            }
            match (version.as_ref(), revision.as_ref()) {
                (None, None) => {
                    bail!(
                        "dependency `{name}` must declare exactly one of `version` or `revision` with `git`"
                    );
                }
                (Some(_), Some(_)) => {
                    bail!(
                        "dependency `{name}` cannot declare both `version` and `revision`; use `version` with .orbi/orbi.lock or `revision` directly"
                    );
                }
                _ => {}
            }
            swift_packages.push(SwiftPackageDependency {
                product: name.clone(),
                source: SwiftPackageSource::Git {
                    url: url.clone(),
                    version,
                    revision,
                },
            });
            continue;
        }
        if dependency.framework == Some(true) {
            frameworks.push(name.clone());
            continue;
        }
        if let Some(path) = &dependency.xcframework {
            xcframeworks.push(XcframeworkDependency {
                path: path.clone(),
                library: Some(name.clone()),
                embed: dependency.embed,
            });
            continue;
        }
        weak_frameworks.shrink_to_fit();
    }

    Ok(NormalizedExternalDependencies {
        frameworks,
        weak_frameworks,
        system_libraries,
        xcframeworks,
        swift_packages,
    })
}

fn validate_git_revision(name: &str, revision: &str) -> Result<()> {
    let is_full_sha = revision.len() == 40 && revision.chars().all(|ch| ch.is_ascii_hexdigit());
    if is_full_sha {
        return Ok(());
    }
    bail!("dependency `{name}` must use an exact 40-character git `revision` SHA")
}

fn validate_git_version(name: &str, version: &str) -> Result<()> {
    Version::parse(version)
        .with_context(|| format!("dependency `{name}` must use an exact semver `version`"))?;
    Ok(())
}

fn normalize_info_plist(info: &InfoManifest) -> BTreeMap<String, JsonValue> {
    info.extra.clone()
}

fn normalize_push(
    push_notifications: bool,
    push_broadcast_for_live_activities: bool,
) -> Option<PushManifest> {
    if !push_notifications && !push_broadcast_for_live_activities {
        return None;
    }
    Some(PushManifest {
        broadcast_for_live_activities: push_broadcast_for_live_activities,
    })
}

fn write_entitlements_if_needed(
    generated_dir: &Path,
    slug: &str,
    entitlements: &EntitlementsManifest,
    app_clip_parent_bundle_id: Option<&str>,
) -> Result<Option<PathBuf>> {
    let dictionary = build_entitlements_dictionary(entitlements, app_clip_parent_bundle_id)?;
    let Some(dictionary) = dictionary else {
        return Ok(None);
    };
    let path = generated_dir.join(format!("{}.entitlements", sanitize_slug(slug)));
    PlistValue::Dictionary(dictionary)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(Some(path))
}

fn validate_internal_manifest(manifest: &ResolvedManifest) -> Result<()> {
    if manifest.platforms.is_empty() {
        bail!("manifest must declare at least one Apple platform");
    }
    if manifest.targets.is_empty() {
        bail!("manifest must declare at least one target");
    }
    let target_names = manifest
        .targets
        .iter()
        .map(|target| target.name.as_str())
        .collect::<BTreeSet<_>>();
    if target_names.len() != manifest.targets.len() {
        bail!("generated target names must be unique");
    }
    for target in &manifest.targets {
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
    }
    Ok(())
}

fn pascal_case(value: &str) -> String {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<String>()
}

fn sanitize_slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if slug.is_empty() {
        "entitlements".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{ExtensionRuntime, TargetKind, load_manifest};
    use crate::manifest::ExtensionEntry;

    fn write_manifest(manifest: serde_json::Value) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempdir().unwrap();
        let manifest_path = temp.path().join("orbi.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        (temp, manifest_path)
    }

    #[test]
    fn share_kind_defaults_to_maininterface_storyboard() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "ShareExample",
            "bundle_id": "dev.orbi.examples.share",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "share": {
                    "kind": "share",
                    "sources": ["Sources/ShareExtension"]
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();

        let share = manifest
            .targets
            .iter()
            .find(|target| target.name == "ShareExtension")
            .unwrap();
        assert_eq!(share.kind, TargetKind::AppExtension);
        assert_eq!(
            share.extension.as_ref().unwrap().point_identifier.as_str(),
            "com.apple.share-services"
        );
        assert_eq!(
            share.extension.as_ref().unwrap().entry,
            ExtensionEntry::MainStoryboard("MainInterface".to_owned())
        );
        assert_eq!(
            share
                .extension
                .as_ref()
                .unwrap()
                .extra
                .get("NSExtensionAttributes"),
            Some(&json!({
                "NSExtensionActivationRule": "TRUEPREDICATE"
            }))
        );
    }

    #[test]
    fn packet_tunnel_kind_requires_class_entry() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "Example",
            "bundle_id": "dev.orbi.examples.tunnel",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "tunnel": {
                    "kind": "packet-tunnel",
                    "sources": ["Sources/TunnelExtension"]
                }
            }
        }));

        let error = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`extensions.tunnel.entry.class` is required"),
            "{error:#}"
        );
    }

    #[test]
    fn widget_kind_uses_widget_target_and_no_entry() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "WidgetExample",
            "bundle_id": "dev.orbi.examples.widget",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "widget": {
                    "kind": "widget",
                    "sources": ["Sources/WidgetExtension"]
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let widget = manifest
            .targets
            .iter()
            .find(|target| target.name == "WidgetExtension")
            .unwrap();

        assert_eq!(widget.kind, TargetKind::WidgetExtension);
        assert_eq!(
            widget.extension.as_ref().unwrap().point_identifier.as_str(),
            "com.apple.widgetkit-extension"
        );
        assert_eq!(
            widget.extension.as_ref().unwrap().entry,
            ExtensionEntry::None
        );
    }

    #[test]
    fn app_intents_kind_uses_extensionkit_runtime_and_no_entry() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "AppIntentsExample",
            "bundle_id": "dev.orbi.examples.app-intents",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "intents": {
                    "kind": "app-intents",
                    "sources": ["Sources/AppIntentsExtension"]
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let extension = manifest
            .targets
            .iter()
            .find(|target| target.name == "IntentsExtension")
            .unwrap();

        assert_eq!(extension.kind, TargetKind::AppExtension);
        assert_eq!(
            extension.extension.as_ref().unwrap().runtime,
            ExtensionRuntime::ExtensionKit
        );
        assert_eq!(
            extension.extension.as_ref().unwrap().entry,
            ExtensionEntry::None
        );
        assert_eq!(
            extension
                .extension
                .as_ref()
                .unwrap()
                .point_identifier
                .as_str(),
            "com.apple.appintents-extension"
        );
    }

    #[test]
    fn account_authentication_modification_defaults_to_maininterface_storyboard() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "AuthenticationExample",
            "bundle_id": "dev.orbi.examples.account-auth",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "auth": {
                    "kind": "account-authentication-modification",
                    "sources": ["Sources/AccountAuthenticationModificationExtension"]
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let extension = manifest
            .targets
            .iter()
            .find(|target| target.name == "AuthExtension")
            .unwrap();

        assert_eq!(
            extension
                .extension
                .as_ref()
                .unwrap()
                .point_identifier
                .as_str(),
            "com.apple.authentication-services-account-authentication-modification-ui"
        );
        assert_eq!(
            extension.extension.as_ref().unwrap().entry,
            ExtensionEntry::MainStoryboard("MainInterface".to_owned())
        );
    }

    #[test]
    fn intents_extension_dsl_populates_supported_and_restricted_intents() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "IntentsExample",
            "bundle_id": "dev.orbi.examples.intents",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "intents": {
                    "kind": "intents",
                    "sources": ["Sources/IntentsExtension"],
                    "entry": {
                        "class": "IntentHandler"
                    },
                    "intents": {
                        "supported": ["INSendMessageIntent"],
                        "restricted_while_locked": ["INSendMessageIntent"]
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let intents = manifest
            .targets
            .iter()
            .find(|target| target.name == "IntentsExtension")
            .unwrap();
        assert_eq!(
            intents
                .extension
                .as_ref()
                .unwrap()
                .extra
                .get("NSExtensionAttributes"),
            Some(&json!({
                "IntentsSupported": ["INSendMessageIntent"],
                "IntentsRestrictedWhileLocked": ["INSendMessageIntent"]
            }))
        );
    }

    #[test]
    fn custom_keyboard_defaults_match_template_shape() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "KeyboardExample",
            "bundle_id": "dev.orbi.examples.keyboard",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "keyboard": {
                    "kind": "custom-keyboard",
                    "sources": ["Sources/KeyboardExtension"],
                    "entry": {
                        "class": "KeyboardViewController"
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let keyboard = manifest
            .targets
            .iter()
            .find(|target| target.name == "KeyboardExtension")
            .unwrap();
        assert_eq!(
            keyboard
                .extension
                .as_ref()
                .unwrap()
                .extra
                .get("NSExtensionAttributes"),
            Some(&json!({
                "PrimaryLanguage": "en-US",
                "IsASCIICapable": false,
                "PrefersRightToLeft": false,
                "RequestsOpenAccess": false
            }))
        );
    }

    #[test]
    fn spotlight_import_dsl_populates_label_and_content_types() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "SpotlightExample",
            "bundle_id": "dev.orbi.examples.spotlight",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "importer": {
                    "kind": "spotlight-import",
                    "sources": ["Sources/SpotlightImportExtension"],
                    "entry": {
                        "class": "ImportExtension"
                    },
                    "spotlight_import": {
                        "label": "OrbiImporter",
                        "content_types": ["com.example.plain-text"]
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let importer = manifest
            .targets
            .iter()
            .find(|target| target.name == "ImporterExtension")
            .unwrap();
        let extension = importer.extension.as_ref().unwrap();
        assert_eq!(
            extension.info_plist_extra.get("CSExtensionLabel"),
            Some(&json!("OrbiImporter"))
        );
        assert_eq!(
            extension.extra.get("NSExtensionAttributes"),
            Some(&json!({
                "CSSupportedContentTypes": ["com.example.plain-text"]
            }))
        );
    }

    #[test]
    fn persistent_token_requires_persistent_token_block_and_uses_no_entry() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "TokenExample",
            "bundle_id": "dev.orbi.examples.token",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "token": {
                    "kind": "persistent-token",
                    "sources": ["Sources/PersistentTokenExtension"],
                    "persistent_token": {
                        "driver_class": "TokenDriver"
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let token = manifest
            .targets
            .iter()
            .find(|target| target.name == "TokenExtension")
            .unwrap();
        assert_eq!(
            token.extension.as_ref().unwrap().entry,
            ExtensionEntry::None
        );
        assert_eq!(
            token
                .extension
                .as_ref()
                .unwrap()
                .extra
                .get("NSExtensionAttributes"),
            Some(&json!({
                "com.apple.ctk.class-id": "dev.orbi.examples.token.token",
                "com.apple.ctk.driver-class": "TokenDriver"
            }))
        );
    }

    #[test]
    fn background_resource_upload_requires_url_base() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "BackgroundUploadExample",
            "bundle_id": "dev.orbi.examples.background-upload",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "upload": {
                    "kind": "background-resource-upload",
                    "sources": ["Sources/BackgroundUploadExtension"]
                }
            }
        }));

        let error = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap_err();
        assert!(
            error.to_string().contains(
                "`extensions.upload.background_resource_upload` is required for `background-resource-upload` extensions"
            ),
            "{error:#}"
        );
    }

    #[test]
    fn file_provider_extension_dsl_populates_provider_keys() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "FileProviderExample",
            "bundle_id": "dev.orbi.examples.file-provider",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "provider": {
                    "kind": "file-provider",
                    "sources": ["Sources/FileProviderExtension"],
                    "entry": {
                        "class": "FileProviderExtension"
                    },
                    "file_provider": {
                        "document_group": "group.dev.orbi.examples.file-provider",
                        "supports_enumeration": false,
                        "actions": [{
                            "identifier": "dev.orbi.examples.file-provider.reindex",
                            "name": "Reindex",
                            "activation_rule": "TRUEPREDICATE"
                        }]
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let provider = manifest
            .targets
            .iter()
            .find(|target| target.name == "ProviderExtension")
            .unwrap();
        let extra = &provider.extension.as_ref().unwrap().extra;

        assert_eq!(
            extra.get("NSExtensionFileProviderDocumentGroup"),
            Some(&json!("group.dev.orbi.examples.file-provider"))
        );
        assert_eq!(
            extra.get("NSExtensionFileProviderSupportsEnumeration"),
            Some(&json!(false))
        );
        assert_eq!(
            extra.get("NSExtensionFileProviderActions"),
            Some(&json!([{
                "NSExtensionFileProviderActionIdentifier": "dev.orbi.examples.file-provider.reindex",
                "NSExtensionFileProviderActionName": "Reindex",
                "NSExtensionFileProviderActionActivationRule": "TRUEPREDICATE"
            }]))
        );
    }

    #[test]
    fn action_service_dsl_populates_activation_rule_and_javascript_file() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "ActionServiceExample",
            "bundle_id": "dev.orbi.examples.action-service",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "action": {
                    "kind": "action-service",
                    "sources": ["Sources/ActionExtension"],
                    "entry": {
                        "class": "ActionRequestHandler"
                    },
                    "action": {
                        "activation_rule": {
                            "NSExtensionActivationSupportsWebURLWithMaxCount": 1
                        },
                        "javascript_preprocessing_file": "Action"
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let action = manifest
            .targets
            .iter()
            .find(|target| target.name == "ActionExtension")
            .unwrap();

        assert_eq!(
            action
                .extension
                .as_ref()
                .unwrap()
                .extra
                .get("NSExtensionAttributes"),
            Some(&json!({
                "NSExtensionActivationRule": {
                    "NSExtensionActivationSupportsWebURLWithMaxCount": 1
                },
                "NSExtensionJavaScriptPreprocessingFile": "Action"
            }))
        );
    }

    #[test]
    fn account_authentication_modification_dsl_populates_feature_flags() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "AuthenticationFlagsExample",
            "bundle_id": "dev.orbi.examples.account-auth-flags",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "auth": {
                    "kind": "account-authentication-modification",
                    "sources": ["Sources/AccountAuthenticationModificationExtension"],
                    "account_authentication_modification": {
                        "supports_upgrade_to_sign_in_with_apple": false,
                        "supports_strong_password_change": true
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let auth = manifest
            .targets
            .iter()
            .find(|target| target.name == "AuthExtension")
            .unwrap();

        assert_eq!(
            auth.extension
                .as_ref()
                .unwrap()
                .extra
                .get("NSExtensionAttributes"),
            Some(&json!({
                "ASAccountAuthenticationModificationSupportsUpgradeToSignInWithApple": false,
                "ASAccountAuthenticationModificationSupportsStrongPasswordChange": true
            }))
        );
    }

    #[test]
    fn broadcast_upload_defaults_to_sample_buffer_process_mode() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "BroadcastUploadExample",
            "bundle_id": "dev.orbi.examples.broadcast-upload",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "broadcast": {
                    "kind": "broadcast-upload",
                    "sources": ["Sources/BroadcastUploadExtension"],
                    "entry": {
                        "class": "SampleHandler"
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let broadcast = manifest
            .targets
            .iter()
            .find(|target| target.name == "BroadcastExtension")
            .unwrap();

        assert_eq!(
            broadcast
                .extension
                .as_ref()
                .unwrap()
                .extra
                .get("RPBroadcastProcessMode"),
            Some(&json!("RPBroadcastProcessModeSampleBuffer"))
        );
    }

    #[test]
    fn notification_content_extension_dsl_populates_categories() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "NotificationContentExample",
            "bundle_id": "dev.orbi.examples.notification-content",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "notification": {
                    "kind": "notification-content",
                    "sources": ["Sources/NotificationContentExtension"],
                    "notification_content": {
                        "categories": ["comment", "follow"],
                        "initial_content_size_ratio": 0.8
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let notification = manifest
            .targets
            .iter()
            .find(|target| target.name == "NotificationExtension")
            .unwrap();
        let attributes = notification
            .extension
            .as_ref()
            .unwrap()
            .extra
            .get("NSExtensionAttributes")
            .unwrap();

        assert_eq!(
            attributes,
            &json!({
                "UNNotificationExtensionCategory": ["comment", "follow"],
                "UNNotificationExtensionInitialContentSizeRatio": 0.8
            })
        );
    }

    #[test]
    fn photo_project_extension_dsl_populates_document_types() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "PhotoProjectExample",
            "bundle_id": "dev.orbi.examples.photo-project",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "macos": "15.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "project": {
                    "kind": "photo-project",
                    "sources": ["Sources/PhotoProjectExtension"],
                    "entry": {
                        "class": "PhotoProjectViewController"
                    },
                    "photo_project": {
                        "defines_project_types": true,
                        "categories": ["book", "prints"],
                        "document_type_identifier": "dev.orbi.examples.photo-project.book-document"
                    }
                }
            }
        }));

        let manifest = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap();
        let project = manifest
            .targets
            .iter()
            .find(|target| target.name == "ProjectExtension")
            .unwrap();
        let extension = project.extension.as_ref().unwrap();

        assert_eq!(
            extension.extra.get("NSExtensionAttributes"),
            Some(&json!({
                "PHProjectExtensionDefinesProjectTypes": true,
                "PHProjectCategory": ["book", "prints"]
            }))
        );
        assert_eq!(
            extension.info_plist_extra.get("CFBundleDocumentTypes"),
            Some(&json!([{
                "CFBundleTypeRole": "Editor",
                "LSItemContentTypes": ["dev.orbi.examples.photo-project.book-document"]
            }]))
        );
    }

    #[test]
    fn rejects_file_provider_block_on_wrong_kind() {
        let (temp, manifest_path) = write_manifest(json!({
            "$schema": "https://orbi.dev/schemas/apple-app.v1.json",
            "name": "InvalidExtensionDsl",
            "bundle_id": "dev.orbi.examples.invalid-dsl",
            "version": "1.0.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "share": {
                    "kind": "share",
                    "sources": ["Sources/ShareExtension"],
                    "file_provider": {
                        "document_group": "group.dev.orbi.examples.invalid-dsl"
                    }
                }
            }
        }));

        let error = load_manifest(&manifest_path, &temp.path().join(".orbi")).unwrap_err();
        assert!(
            error.to_string().contains(
                "`extensions.share.file_provider` is only supported for `file-provider` extensions"
            ),
            "{error:#}"
        );
    }
}
