use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use plist::Value as PlistValue;
use serde_json::Value as JsonValue;

use super::authoring::{
    AppManifest, DependencySpec, EntitlementsManifest, ExtensionConfig, ExtensionKind,
    InfoManifest, PushConfig,
};
use super::entitlements::build_entitlements_dictionary;
use super::{
    ApplePlatform, ExtensionManifest, IosTargetManifest, Manifest, PlatformManifest, PushManifest,
    SwiftPackageDependency, TargetKind, TargetManifest, XcframeworkDependency,
};
use crate::util::ensure_dir;

pub fn load_manifest(path: &Path, orbit_dir: &Path) -> Result<Manifest> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let manifest: AppManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    normalize_manifest(path, orbit_dir, manifest)
}

fn normalize_manifest(_path: &Path, orbit_dir: &Path, app: AppManifest) -> Result<Manifest> {
    validate_semver_version(&app.version)?;
    validate_root_manifest(&app)?;

    let generated_entitlements_dir = orbit_dir.join("manifest").join("entitlements");
    ensure_dir(&generated_entitlements_dir)?;

    let non_watch_platforms = app
        .platforms
        .keys()
        .copied()
        .filter(|platform| *platform != ApplePlatform::Watchos)
        .collect::<Vec<_>>();

    let mut manifest = Manifest {
        name: app.name.clone(),
        version: app.version.clone(),
        team_id: app.team_id.clone(),
        provider_id: app.provider_id.clone(),
        platforms: app
            .platforms
            .iter()
            .map(|(platform, deployment_target)| {
                (
                    *platform,
                    PlatformManifest {
                        deployment_target: deployment_target.clone(),
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

    let root_entitlements = write_entitlements_if_needed(
        &generated_entitlements_dir,
        "app",
        &app.entitlements,
        app.push.as_ref(),
        None,
    )?;
    let (frameworks, weak_frameworks, system_libraries, xcframeworks, swift_packages) =
        normalize_external_dependencies(&app.dependencies)?;

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
        frameworks,
        weak_frameworks,
        system_libraries,
        xcframeworks,
        swift_packages,
        info_plist: normalize_info_plist(&app.info),
        ios: Some(IosTargetManifest::default()),
        entitlements: root_entitlements,
        push: app.push.as_ref().map(normalize_push),
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
    let non_watch_platforms = app
        .platforms
        .keys()
        .filter(|platform| **platform != ApplePlatform::Watchos)
        .count();
    if non_watch_platforms == 0 {
        bail!("the root app must declare at least one non-watch platform");
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
    let entry = extension
        .entry
        .as_ref()
        .context("extensions currently require an `entry.class`")?;
    let platforms = if extension.platforms.is_empty() {
        default_platforms.to_vec()
    } else {
        extension.platforms.clone()
    };
    let (kind, point_identifier) = normalize_extension_kind(extension.kind);
    let (frameworks, weak_frameworks, system_libraries, xcframeworks, swift_packages) =
        normalize_external_dependencies(&extension.dependencies)?;

    Ok(TargetManifest {
        name: target_name.to_owned(),
        kind,
        bundle_id: format!("{}.{}", app.bundle_id, id),
        display_name: None,
        build_number: Some(app.build.to_string()),
        platforms,
        sources: extension.sources.clone(),
        resources: extension.resources.clone(),
        dependencies: Vec::new(),
        frameworks,
        weak_frameworks,
        system_libraries,
        xcframeworks,
        swift_packages,
        info_plist: normalize_info_plist(&extension.info),
        ios: None,
        entitlements: write_entitlements_if_needed(
            generated_entitlements_dir,
            id,
            &extension.entitlements,
            extension.push.as_ref(),
            None,
        )?,
        push: extension.push.as_ref().map(normalize_push),
        extension: Some(ExtensionManifest {
            point_identifier: point_identifier.to_owned(),
            principal_class: entry.class.clone(),
            extra: BTreeMap::new(),
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
    let (frameworks, weak_frameworks, system_libraries, xcframeworks, swift_packages) =
        normalize_external_dependencies(&watch.dependencies)?;
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
        frameworks,
        weak_frameworks,
        system_libraries,
        xcframeworks,
        swift_packages,
        info_plist: normalize_info_plist(&watch.info),
        ios: None,
        entitlements: write_entitlements_if_needed(
            generated_entitlements_dir,
            "watch-app",
            &watch.entitlements,
            None,
            None,
        )?,
        push: None,
        extension: None,
    })
}

fn build_watch_extension_target(
    app: &AppManifest,
    target_name: &str,
    generated_entitlements_dir: &Path,
) -> Result<TargetManifest> {
    let watch = app.watch.as_ref().context("watch manifest missing")?;
    let (frameworks, weak_frameworks, system_libraries, xcframeworks, swift_packages) =
        normalize_external_dependencies(&watch.extension.dependencies)?;
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
        frameworks,
        weak_frameworks,
        system_libraries,
        xcframeworks,
        swift_packages,
        info_plist: normalize_info_plist(&watch.extension.info),
        ios: None,
        entitlements: write_entitlements_if_needed(
            generated_entitlements_dir,
            "watch-extension",
            &watch.extension.entitlements,
            None,
            None,
        )?,
        push: None,
        extension: Some(ExtensionManifest {
            point_identifier: "com.apple.watchkit".to_owned(),
            principal_class: watch.extension.entry.class.clone(),
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
    let (frameworks, weak_frameworks, system_libraries, xcframeworks, swift_packages) =
        normalize_external_dependencies(&app_clip.dependencies)?;
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
        frameworks,
        weak_frameworks,
        system_libraries,
        xcframeworks,
        swift_packages,
        info_plist: normalize_info_plist(&app_clip.info),
        ios: Some(IosTargetManifest::default()),
        entitlements: write_entitlements_if_needed(
            generated_entitlements_dir,
            "app-clip",
            &app_clip.entitlements,
            app_clip.push.as_ref(),
            Some(&app.bundle_id),
        )?,
        push: app_clip.push.as_ref().map(normalize_push),
        extension: None,
    })
}

fn normalize_extension_kind(kind: ExtensionKind) -> (TargetKind, &'static str) {
    match kind {
        ExtensionKind::PacketTunnel => (
            TargetKind::AppExtension,
            "com.apple.networkextension.packet-tunnel",
        ),
        ExtensionKind::Widget => (TargetKind::WidgetExtension, "com.apple.widgetkit-extension"),
        ExtensionKind::Share => (TargetKind::AppExtension, "com.apple.share-services"),
        ExtensionKind::Safari => (TargetKind::AppExtension, "com.apple.Safari.extension"),
        ExtensionKind::SafariWeb => (TargetKind::AppExtension, "com.apple.Safari.web-extension"),
    }
}

fn normalize_external_dependencies(
    dependencies: &BTreeMap<String, DependencySpec>,
) -> Result<(
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<XcframeworkDependency>,
    Vec<SwiftPackageDependency>,
)> {
    let mut frameworks = Vec::new();
    let mut weak_frameworks = Vec::new();
    let system_libraries = Vec::new();
    let mut xcframeworks = Vec::new();
    let mut swift_packages = Vec::new();

    for (name, dependency) in dependencies {
        let choices = [
            dependency.path.is_some(),
            dependency.framework == Some(true),
            dependency.xcframework.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if choices != 1 {
            bail!(
                "dependency `{name}` must set exactly one of `path`, `framework`, or `xcframework`"
            );
        }
        if let Some(path) = &dependency.path {
            swift_packages.push(SwiftPackageDependency {
                product: name.clone(),
                path: path.clone(),
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

    Ok((
        frameworks,
        weak_frameworks,
        system_libraries,
        xcframeworks,
        swift_packages,
    ))
}

fn normalize_info_plist(info: &InfoManifest) -> BTreeMap<String, JsonValue> {
    info.extra.clone()
}

fn normalize_push(push: &PushConfig) -> PushManifest {
    PushManifest {
        broadcast: push.broadcast,
        credential: push.credential,
    }
}

fn write_entitlements_if_needed(
    generated_dir: &Path,
    slug: &str,
    entitlements: &EntitlementsManifest,
    push: Option<&PushConfig>,
    app_clip_parent_bundle_id: Option<&str>,
) -> Result<Option<PathBuf>> {
    let dictionary = build_entitlements_dictionary(entitlements, push, app_clip_parent_bundle_id)?;
    let Some(dictionary) = dictionary else {
        return Ok(None);
    };
    let path = generated_dir.join(format!("{}.entitlements", sanitize_slug(slug)));
    PlistValue::Dictionary(dictionary)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(Some(path))
}

fn validate_internal_manifest(manifest: &Manifest) -> Result<()> {
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
