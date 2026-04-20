use std::collections::BTreeMap;
use std::path::PathBuf;

use plist::{Dictionary, Value};
use serde_json::json;
use tempfile::TempDir;

use super::compile::relocate_bundle_debug_artifacts;
#[cfg(target_os = "macos")]
use super::info_plist::write_info_plist;
use super::info_plist::{extension_plist, json_to_plist, merge_extension_attributes};
use super::resources::merge_partial_info_plist;
use super::{
    ApplePlatform, BuildConfiguration, DestinationKind, DistributionKind, ExtensionManifest,
    ProfileManifest, TargetKind, Toolchain, build_requires_signing, embedded_dependency_root,
};
use crate::apple::build::external::{
    SwiftPackageManifest, SwiftPackageProduct, SwiftPackageTarget, SwiftPackageTargetDependency,
    XcframeworkLibrary, ordered_package_targets, select_xcframework_library,
};
use crate::context::{AppContext, GlobalPaths, ProjectContext, ProjectPaths};
use crate::manifest::{ExtensionEntry, ExtensionRuntime, ManifestSchema, ResolvedManifest};
#[cfg(target_os = "macos")]
use crate::manifest::{
    IosDeviceFamily, IosInterfaceOrientation, IosSupportedOrientationsManifest, IosTargetManifest,
};

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn project_for_fixture(path: &str) -> (TempDir, ProjectContext) {
    let temp = tempfile::tempdir().unwrap();
    let manifest_path = fixture(path);
    let root = manifest_path.parent().unwrap().to_path_buf();
    let data_dir = temp.path().join("data");
    let cache_dir = temp.path().join("cache");
    let orbi_dir = temp.path().join("orbi");
    let build_dir = orbi_dir.join("build");
    let artifacts_dir = orbi_dir.join("artifacts");
    let receipts_dir = orbi_dir.join("receipts");
    let manifest = ResolvedManifest::load(&manifest_path, &orbi_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::create_dir_all(&artifacts_dir).unwrap();
    std::fs::create_dir_all(&receipts_dir).unwrap();

    let project = ProjectContext {
        app: AppContext {
            cwd: root.clone(),
            interactive: false,
            verbose: false,
            manifest_env: None,
            global_paths: GlobalPaths {
                data_dir: data_dir.clone(),
                cache_dir,
                schema_dir: data_dir.join("schemas"),
            },
        },
        root,
        manifest_path,
        manifest_schema: ManifestSchema::AppleAppV1,
        resolved_manifest: manifest,
        selected_xcode: None,
        project_paths: ProjectPaths {
            orbi_dir,
            build_dir,
            artifacts_dir,
            receipts_dir,
        },
    };
    (temp, project)
}

#[cfg(target_os = "macos")]
#[test]
fn writes_ios_app_defaults_without_scene_manifest_inference() {
    let (temp, project) = project_for_fixture("examples/ios-simulator-app/orbi.json");
    let target = project
        .resolved_manifest
        .resolve_target(Some("ExampleIOSApp"))
        .unwrap()
        .clone();
    let bundle_root = temp.path().join("ExampleIOSApp.app");
    std::fs::create_dir_all(&bundle_root).unwrap();
    let toolchain = Toolchain {
        platform: ApplePlatform::Ios,
        destination: DestinationKind::Device,
        sdk_name: "iphoneos".to_owned(),
        sdk_path: PathBuf::from("/tmp/iphoneos.sdk"),
        deployment_target: "18.0".to_owned(),
        architecture: "arm64".to_owned(),
        target_triple: "arm64-apple-ios18.0".to_owned(),
        selected_xcode: None,
    };

    write_info_plist(&project, &toolchain, &target, &bundle_root).unwrap();

    let plist = Value::from_file(bundle_root.join("Info.plist")).unwrap();
    let dict = plist.as_dictionary().unwrap();
    let device_family = dict
        .get("UIDeviceFamily")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(
        device_family
            .iter()
            .filter_map(Value::as_signed_integer)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        dict.get("CFBundleDevelopmentRegion")
            .and_then(Value::as_string),
        Some("en")
    );
    assert_eq!(
        dict.get("DTPlatformName").and_then(Value::as_string),
        Some("iphoneos")
    );
    assert!(dict.contains_key("DTSDKName"));
    assert!(dict.contains_key("DTXcode"));
    assert!(dict.contains_key("DTXcodeBuild"));
    assert!(!dict.contains_key("UIApplicationSceneManifest"));
    assert_eq!(
        dict.get("UIRequiredDeviceCapabilities")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_string)
                    .collect::<Vec<_>>()
            }),
        Some(vec!["arm64"])
    );
    assert_eq!(
        dict.get("UILaunchScreen")
            .and_then(Value::as_dictionary)
            .and_then(|launch_screen| launch_screen.get("UILaunchScreen"))
            .and_then(Value::as_dictionary)
            .map(Dictionary::is_empty),
        Some(true)
    );
    assert!(dict.contains_key("UISupportedInterfaceOrientations~iphone"));
    assert_eq!(
        dict.get("UIApplicationSupportsIndirectInputEvents")
            .and_then(Value::as_boolean),
        Some(true)
    );
    assert_eq!(
        std::fs::read_to_string(bundle_root.join("PkgInfo")).unwrap(),
        "APPL????"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn writes_extension_top_level_info_plist_extra() {
    let (temp, project) = project_for_fixture("examples/ios-app-extension/orbi.json");
    let mut target = project
        .resolved_manifest
        .resolve_target(Some("TunnelExtension"))
        .unwrap()
        .clone();
    target.extension.as_mut().unwrap().info_plist_extra.insert(
        "CFBundleDocumentTypes".to_owned(),
        json!([{
            "CFBundleTypeRole": "Editor",
            "LSItemContentTypes": ["dev.orbi.examples.extensionapp.tunnel-document"]
        }]),
    );
    let bundle_root = temp.path().join("TunnelExtension.appex");
    std::fs::create_dir_all(&bundle_root).unwrap();
    let toolchain = Toolchain {
        platform: ApplePlatform::Ios,
        destination: DestinationKind::Device,
        sdk_name: "iphoneos".to_owned(),
        sdk_path: PathBuf::from("/tmp/iphoneos.sdk"),
        deployment_target: "18.0".to_owned(),
        architecture: "arm64".to_owned(),
        target_triple: "arm64-apple-ios18.0".to_owned(),
        selected_xcode: None,
    };

    write_info_plist(&project, &toolchain, &target, &bundle_root).unwrap();

    let plist = Value::from_file(bundle_root.join("Info.plist")).unwrap();
    let dict = plist.as_dictionary().unwrap();
    assert_eq!(
        dict.get("CFBundleDocumentTypes"),
        Some(&Value::Array(vec![Value::Dictionary(
            Dictionary::from_iter([
                (
                    "CFBundleTypeRole".to_owned(),
                    Value::String("Editor".to_owned()),
                ),
                (
                    "LSItemContentTypes".to_owned(),
                    Value::Array(vec![Value::String(
                        "dev.orbi.examples.extensionapp.tunnel-document".to_owned(),
                    )]),
                ),
            ])
        )]))
    );
}

#[cfg(target_os = "macos")]
#[test]
fn applies_manifest_driven_ios_plist_metadata() {
    let (temp, project) = project_for_fixture("examples/ios-simulator-app/orbi.json");
    let mut target = project
        .resolved_manifest
        .resolve_target(Some("ExampleIOSApp"))
        .unwrap()
        .clone();
    target.display_name = Some("Orbi Example".to_owned());
    target.build_number = Some("42".to_owned());
    target.info_plist.insert(
        "NSCameraUsageDescription".to_owned(),
        json!("Camera access is required."),
    );
    target.info_plist.insert(
        "UIStatusBarStyle".to_owned(),
        json!("UIStatusBarStyleLightContent"),
    );
    target.ios = Some(IosTargetManifest {
        device_families: Some(vec![IosDeviceFamily::Iphone]),
        supported_orientations: Some(IosSupportedOrientationsManifest {
            iphone: Some(vec![IosInterfaceOrientation::Portrait]),
            ipad: Some(vec![IosInterfaceOrientation::LandscapeLeft]),
        }),
        required_device_capabilities: Some(vec!["arm64".to_owned(), "metal".to_owned()]),
        launch_screen: Some(BTreeMap::from([(
            "UIColorName".to_owned(),
            json!("LaunchBackground"),
        )])),
    });

    let bundle_root = temp.path().join("ExampleIOSApp.app");
    std::fs::create_dir_all(&bundle_root).unwrap();
    let toolchain = Toolchain {
        platform: ApplePlatform::Ios,
        destination: DestinationKind::Device,
        sdk_name: "iphoneos".to_owned(),
        sdk_path: PathBuf::from("/tmp/iphoneos.sdk"),
        deployment_target: "18.0".to_owned(),
        architecture: "arm64".to_owned(),
        target_triple: "arm64-apple-ios18.0".to_owned(),
        selected_xcode: None,
    };

    write_info_plist(&project, &toolchain, &target, &bundle_root).unwrap();

    let plist = Value::from_file(bundle_root.join("Info.plist")).unwrap();
    let dict = plist.as_dictionary().unwrap();
    assert_eq!(
        dict.get("CFBundleDisplayName").and_then(Value::as_string),
        Some("Orbi Example")
    );
    assert_eq!(
        dict.get("CFBundleVersion").and_then(Value::as_string),
        Some("42")
    );
    assert_eq!(
        dict.get("UIDeviceFamily")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_signed_integer)
                    .collect::<Vec<_>>()
            }),
        Some(vec![1])
    );
    assert_eq!(
        dict.get("UIRequiredDeviceCapabilities")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_string)
                    .collect::<Vec<_>>()
            }),
        Some(vec!["arm64", "metal"])
    );
    assert_eq!(
        dict.get("UISupportedInterfaceOrientations~iphone")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_string)
                    .collect::<Vec<_>>()
            }),
        Some(vec!["UIInterfaceOrientationPortrait"])
    );
    assert!(!dict.contains_key("UISupportedInterfaceOrientations~ipad"));
    assert_eq!(
        dict.get("UILaunchScreen")
            .and_then(Value::as_dictionary)
            .and_then(|launch_screen| launch_screen.get("UILaunchScreen"))
            .and_then(Value::as_dictionary)
            .and_then(|launch_screen| launch_screen.get("UIColorName"))
            .and_then(Value::as_string),
        Some("LaunchBackground")
    );
    assert_eq!(
        dict.get("NSCameraUsageDescription")
            .and_then(Value::as_string),
        Some("Camera access is required.")
    );
    assert_eq!(
        dict.get("UIStatusBarStyle").and_then(Value::as_string),
        Some("UIStatusBarStyleLightContent")
    );
    assert_eq!(
        dict.get("DTPlatformName").and_then(Value::as_string),
        Some("iphoneos")
    );
    assert!(dict.contains_key("BuildMachineOSBuild"));
    assert!(dict.contains_key("DTPlatformBuild"));
    assert!(dict.contains_key("DTSDKBuild"));
    assert!(dict.contains_key("DTSDKName"));
    assert!(dict.contains_key("DTXcode"));
    assert!(dict.contains_key("DTXcodeBuild"));
}

#[cfg(target_os = "macos")]
#[test]
fn defaults_bundle_display_name_to_target_name() {
    let (temp, project) = project_for_fixture("examples/macos-app/orbi.json");
    let target = project
        .resolved_manifest
        .resolve_target(Some("ExampleMacApp"))
        .unwrap()
        .clone();
    let bundle_root = temp.path().join("ExampleMacApp.app");
    std::fs::create_dir_all(&bundle_root).unwrap();
    let toolchain = Toolchain {
        platform: ApplePlatform::Macos,
        destination: DestinationKind::Device,
        sdk_name: "macosx".to_owned(),
        sdk_path: PathBuf::from("/tmp/macosx.sdk"),
        deployment_target: "14.0".to_owned(),
        architecture: "arm64".to_owned(),
        target_triple: "arm64-apple-macosx14.0".to_owned(),
        selected_xcode: None,
    };

    write_info_plist(&project, &toolchain, &target, &bundle_root).unwrap();

    let plist = Value::from_file(bundle_root.join("Contents").join("Info.plist")).unwrap();
    let dict = plist.as_dictionary().unwrap();
    assert_eq!(
        dict.get("CFBundleName").and_then(Value::as_string),
        Some("ExampleMacApp")
    );
    assert_eq!(
        dict.get("CFBundleDisplayName").and_then(Value::as_string),
        Some("ExampleMacApp")
    );
}

#[test]
fn loads_macos_universal_binary_opt_in() {
    let (_temp, project) = project_for_fixture("examples/macos-app/orbi.json");
    let macos = project
        .resolved_manifest
        .platforms
        .get(&ApplePlatform::Macos)
        .expect("macos platform manifest");

    assert!(macos.universal_binary);
}

#[test]
fn device_builds_require_signing_in_development() {
    let profile = ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development);
    assert!(build_requires_signing(&profile, DestinationKind::Device));
    assert!(!build_requires_signing(
        &profile,
        DestinationKind::Simulator
    ));
}

#[test]
fn macos_development_device_builds_require_signing() {
    let profile = ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development);
    assert!(build_requires_signing(&profile, DestinationKind::Device));
}

#[cfg(target_os = "macos")]
#[test]
fn writes_macos_app_metadata_under_contents() {
    let (temp, project) = project_for_fixture("examples/macos-app/orbi.json");
    let target = project
        .resolved_manifest
        .resolve_target(Some("ExampleMacApp"))
        .unwrap()
        .clone();
    let bundle_root = temp.path().join("ExampleMacApp.app");
    std::fs::create_dir_all(&bundle_root).unwrap();
    let toolchain = Toolchain {
        platform: ApplePlatform::Macos,
        destination: DestinationKind::Device,
        sdk_name: "macosx".to_owned(),
        sdk_path: PathBuf::from("/tmp/macosx.sdk"),
        deployment_target: "14.0".to_owned(),
        architecture: "arm64".to_owned(),
        target_triple: "arm64-apple-macosx14.0".to_owned(),
        selected_xcode: None,
    };

    write_info_plist(&project, &toolchain, &target, &bundle_root).unwrap();

    assert!(bundle_root.join("Contents").join("Info.plist").exists());
    assert!(bundle_root.join("Contents").join("PkgInfo").exists());
    assert!(!bundle_root.join("Info.plist").exists());
    assert!(!bundle_root.join("PkgInfo").exists());
}

#[test]
fn relocates_bundle_dsym_out_of_app_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let target_dir = temp.path().join("ExampleIOSApp");
    let bundle_root = target_dir.join("ExampleIOSApp.app");
    std::fs::create_dir_all(&bundle_root).unwrap();
    let binary_path = bundle_root.join("ExampleIOSApp");
    std::fs::write(&binary_path, b"binary").unwrap();
    let bundle_dsym = binary_path.with_extension("dSYM");
    std::fs::create_dir_all(bundle_dsym.join("Contents")).unwrap();

    relocate_bundle_debug_artifacts(&target_dir, &bundle_root, &binary_path).unwrap();

    assert!(!bundle_dsym.exists());
    assert!(target_dir.join("ExampleIOSApp.dSYM").exists());
}

#[test]
fn merges_actool_partial_info_plist_into_bundle_info() {
    let temp = tempfile::tempdir().unwrap();
    let bundle_root = temp.path().join("Example.app");
    std::fs::create_dir_all(&bundle_root).unwrap();
    Value::Dictionary(Dictionary::from_iter([(
        "CFBundleIdentifier".to_owned(),
        Value::String("dev.orbi.example".to_owned()),
    )]))
    .to_file_xml(bundle_root.join("Info.plist"))
    .unwrap();
    let partial_path = temp.path().join("partial.plist");
    Value::Dictionary(Dictionary::from_iter([
        (
            "NSAccentColorName".to_owned(),
            Value::String("AccentColor".to_owned()),
        ),
        (
            "CFBundleIcons".to_owned(),
            Value::Dictionary(Dictionary::from_iter([(
                "CFBundlePrimaryIcon".to_owned(),
                Value::Dictionary(Dictionary::new()),
            )])),
        ),
    ]))
    .to_file_xml(&partial_path)
    .unwrap();

    merge_partial_info_plist(&bundle_root, &partial_path).unwrap();

    let merged = Value::from_file(bundle_root.join("Info.plist")).unwrap();
    let dict = merged.as_dictionary().unwrap();
    assert_eq!(
        dict.get("NSAccentColorName").and_then(Value::as_string),
        Some("AccentColor")
    );
    assert!(
        dict.get("CFBundleIcons")
            .and_then(Value::as_dictionary)
            .is_some()
    );
}

#[test]
fn embeds_watch_children_into_expected_subdirectories() {
    let (_temp, project) = project_for_fixture("examples/ios-watch-app/orbi.json");
    let app = project
        .resolved_manifest
        .resolve_target(Some("ExampleCompanionApp"))
        .unwrap();
    let watch_app = project
        .resolved_manifest
        .resolve_target(Some("WatchApp"))
        .unwrap();
    let watch_extension = project
        .resolved_manifest
        .resolve_target(Some("WatchExtension"))
        .unwrap();
    assert_eq!(
        embedded_dependency_root(&project, ApplePlatform::Ios, app, watch_app).unwrap(),
        Some(PathBuf::from("Watch"))
    );
    assert_eq!(
        embedded_dependency_root(&project, ApplePlatform::Watchos, watch_app, watch_extension)
            .unwrap(),
        Some(PathBuf::from("PlugIns"))
    );
    assert_eq!(
        embedded_dependency_root(&project, ApplePlatform::Watchos, watch_app, watch_app).unwrap(),
        None
    );
    let framework = crate::manifest::TargetManifest {
        name: "OrbiFramework".to_owned(),
        kind: TargetKind::Framework,
        bundle_id: "dev.orbi.framework".to_owned(),
        display_name: None,
        build_number: None,
        platforms: vec![ApplePlatform::Watchos],
        sources: vec!["Sources/Framework".into()],
        resources: Vec::new(),
        dependencies: Vec::new(),
        frameworks: Vec::new(),
        weak_frameworks: Vec::new(),
        system_libraries: Vec::new(),
        xcframeworks: Vec::new(),
        swift_packages: Vec::new(),
        info_plist: BTreeMap::new(),
        ios: None,
        entitlements: None,
        push: None,
        extension: None,
    };
    assert_eq!(
        embedded_dependency_root(&project, ApplePlatform::Watchos, watch_app, &framework).unwrap(),
        Some(PathBuf::from("Frameworks"))
    );
}

#[test]
fn embeds_app_clips_into_appclips_directory() {
    let (_temp, project) = project_for_fixture("examples/ios-app-clip/orbi.json");
    let app = project
        .resolved_manifest
        .resolve_target(Some("ExampleApp"))
        .unwrap();
    let clip = project
        .resolved_manifest
        .resolve_target(Some("AppClip"))
        .unwrap();

    assert_eq!(
        embedded_dependency_root(&project, ApplePlatform::Ios, app, clip).unwrap(),
        Some(PathBuf::from("AppClips"))
    );
}

#[test]
fn preserves_extra_extension_entries() {
    let extension = ExtensionManifest {
        runtime: ExtensionRuntime::NsExtension,
        entry: ExtensionEntry::None,
        point_identifier: "com.apple.widgetkit-extension".to_owned(),
        info_plist_extra: BTreeMap::new(),
        extra: BTreeMap::from([(
            "NSExtensionAttributes".to_owned(),
            json!({
                "WKBackgroundModes": ["workout-processing"]
            }),
        )]),
    };
    let mut plist = extension_plist(&extension).unwrap();
    merge_extension_attributes(
        &mut plist,
        Dictionary::from_iter([(
            "WKAppBundleIdentifier".to_owned(),
            plist::Value::String("dev.orbi.examples.watch.watchkitapp".to_owned()),
        )]),
    );

    let attributes = plist
        .get("NSExtensionAttributes")
        .and_then(plist::Value::as_dictionary)
        .unwrap();
    assert_eq!(
        attributes
            .get("WKBackgroundModes")
            .and_then(plist::Value::as_array)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        attributes
            .get("WKAppBundleIdentifier")
            .and_then(plist::Value::as_string)
            .unwrap(),
        "dev.orbi.examples.watch.watchkitapp"
    );
}

#[test]
fn serializes_extensionkit_entries_into_ex_app_extension_attributes() {
    let extension = ExtensionManifest {
        runtime: ExtensionRuntime::ExtensionKit,
        entry: ExtensionEntry::PrincipalClass("AppIntentsExtension".to_owned()),
        point_identifier: "com.apple.appintents-extension".to_owned(),
        info_plist_extra: BTreeMap::new(),
        extra: BTreeMap::new(),
    };

    let plist = extension_plist(&extension).unwrap();

    assert_eq!(
        plist
            .get("EXExtensionPointIdentifier")
            .and_then(plist::Value::as_string),
        Some("com.apple.appintents-extension")
    );
    assert_eq!(
        plist
            .get("EXPrincipalClass")
            .and_then(plist::Value::as_string),
        Some("AppIntentsExtension")
    );
    assert!(plist.get("NSExtensionPointIdentifier").is_none());
}

#[test]
fn converts_nested_json_values_into_plist_values() {
    let value = json_to_plist(&json!({
        "Enabled": true,
        "Count": 3,
        "Items": ["one", "two"]
    }))
    .unwrap();
    let dictionary = value.as_dictionary().unwrap();
    assert!(
        dictionary
            .get("Enabled")
            .and_then(plist::Value::as_boolean)
            .unwrap()
    );
    assert_eq!(
        dictionary
            .get("Items")
            .and_then(plist::Value::as_array)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn selects_matching_xcframework_slice_for_target_platform() {
    let toolchain = Toolchain {
        platform: ApplePlatform::Ios,
        destination: DestinationKind::Simulator,
        sdk_name: "iphonesimulator".to_owned(),
        sdk_path: "/tmp/sdk".into(),
        deployment_target: "18.0".to_owned(),
        architecture: "arm64".to_owned(),
        target_triple: "arm64-apple-ios18.0-simulator".to_owned(),
        selected_xcode: None,
    };
    let slices = vec![
        XcframeworkLibrary {
            library_identifier: "ios-arm64".to_owned(),
            library_path: "Orbi.framework".to_owned(),
            headers_path: None,
            supported_platform: "ios".to_owned(),
            supported_platform_variant: None,
            supported_architectures: vec!["arm64".to_owned()],
        },
        XcframeworkLibrary {
            library_identifier: "ios-arm64_x86_64-simulator".to_owned(),
            library_path: "Orbi.framework".to_owned(),
            headers_path: None,
            supported_platform: "ios".to_owned(),
            supported_platform_variant: Some("simulator".to_owned()),
            supported_architectures: vec!["arm64".to_owned(), "x86_64".to_owned()],
        },
    ];

    let selected = select_xcframework_library(&toolchain, &slices).unwrap();
    assert_eq!(selected.library_identifier, "ios-arm64_x86_64-simulator");
}

#[test]
fn orders_swift_package_targets_by_local_dependencies() {
    let package = SwiftPackageManifest {
        name: "FeaturePackage".to_owned(),
        products: vec![SwiftPackageProduct {
            name: "Feature".to_owned(),
            targets: vec!["Feature".to_owned()],
        }],
        targets: vec![
            SwiftPackageTarget {
                name: "Core".to_owned(),
                path: None,
                dependencies: Vec::new(),
                kind: Some("regular".to_owned()),
            },
            SwiftPackageTarget {
                name: "Feature".to_owned(),
                path: None,
                dependencies: vec![SwiftPackageTargetDependency::ByName {
                    by_name: ("Core".to_owned(), None),
                }],
                kind: Some("regular".to_owned()),
            },
        ],
    };

    let ordered = ordered_package_targets(&package, &["Feature".to_owned()]).unwrap();
    assert_eq!(ordered, vec!["Core".to_owned(), "Feature".to_owned()]);
}
