use std::collections::BTreeMap;

use plist::Value;
use serde_json::json;
use tempfile::TempDir;

use super::capability_sync::{
    asc_capability_settings, plan_asc_capability_mutations, validate_push_setup_with_api_key,
};
use super::cleanup::{ProjectEntitlementIdentifiers, project_entitlement_identifiers};
use super::device_selection::{
    current_profile_for_target, format_cached_device_label, missing_registered_devices,
    profile_udids, same_udid_set,
};
use super::{
    ASC_OPTION_APPLE_ID_PRIMARY_CONSENT, ASC_OPTION_DATA_PROTECTION_COMPLETE,
    ASC_OPTION_PUSH_BROADCAST, CertificateOrigin, ManagedCertificate, ManagedProfile,
    ProfileManifest, SigningState, clean_local_signing_state, load_state,
    materialize_signing_entitlements, profile_covers_requested_ids, resolve_local_team_id_if_known,
    save_state, target_is_app_clip, team_signing_paths,
};
use crate::apple::capabilities::{CapabilityRelationships, CapabilityUpdate, RemoteCapability};
use crate::apple::device::CachedDevice;
use crate::context::{AppContext, GlobalPaths, ProjectContext, ProjectPaths};
use crate::manifest::{
    ApplePlatform, BuildConfiguration, DistributionKind, HooksManifest, ManifestSchema,
    PlatformManifest, QualityManifest, ResolvedManifest, TargetKind, TargetManifest, TestsManifest,
};

fn test_project() -> (TempDir, ProjectContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let data_dir = temp.path().join("data");
    let cache_dir = temp.path().join("cache");
    let orbit_dir = root.join(".orbit");
    let build_dir = orbit_dir.join("build");
    let artifacts_dir = orbit_dir.join("artifacts");
    let receipts_dir = orbit_dir.join("receipts");
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::create_dir_all(&artifacts_dir).unwrap();
    std::fs::create_dir_all(&receipts_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manifest = ResolvedManifest {
        name: "OrbitFixture".to_owned(),
        version: "0.1.0".to_owned(),
        xcode: None,
        team_id: Some("TEAM123456".to_owned()),
        provider_id: None,
        hooks: HooksManifest::default(),
        tests: TestsManifest::default(),
        quality: QualityManifest::default(),
        platforms: BTreeMap::from([(
            ApplePlatform::Ios,
            PlatformManifest {
                deployment_target: "18.0".to_owned(),
                universal_binary: false,
            },
        )]),
        targets: vec![TargetManifest {
            name: "ExampleApp".to_owned(),
            kind: TargetKind::App,
            bundle_id: "dev.orbit.fixture".to_owned(),
            display_name: None,
            build_number: None,
            platforms: vec![ApplePlatform::Ios],
            sources: vec![root.join("Sources/App")],
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
        }],
    };
    let manifest_path = root.join("orbit.json");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let app = AppContext {
        cwd: root.clone(),
        interactive: false,
        verbose: false,
        global_paths: GlobalPaths {
            data_dir: data_dir.clone(),
            cache_dir,
            schema_dir: data_dir.join("schemas"),
            auth_state_path: data_dir.join("auth.json"),
            device_cache_path: data_dir.join("devices.json"),
            keychain_path: data_dir.join("orbit.keychain-db"),
        },
    };
    let project = ProjectContext {
        app,
        root: root.clone(),
        manifest_path,
        manifest_schema: ManifestSchema::AppleAppV1,
        resolved_manifest: manifest,
        selected_xcode: None,
        project_paths: ProjectPaths {
            orbit_dir,
            build_dir,
            artifacts_dir,
            receipts_dir,
        },
    };
    (temp, project)
}

#[test]
fn local_cleanup_removes_only_current_project_profiles_and_unused_certs() {
    let (_temp, project) = test_project();
    let team_id = "TEAM123456";
    let team_paths = team_signing_paths(&project, team_id);
    std::fs::create_dir_all(&team_paths.certificates_dir).unwrap();
    std::fs::create_dir_all(&team_paths.profiles_dir).unwrap();

    let current_profile_path = team_paths.profiles_dir.join("current.mobileprovision");
    let other_profile_path = team_paths.profiles_dir.join("other.mobileprovision");
    let current_key_path = team_paths.certificates_dir.join("current.key.pem");
    let current_cer_path = team_paths.certificates_dir.join("current.cer");
    let current_p12_path = team_paths.certificates_dir.join("current.p12");
    let other_key_path = team_paths.certificates_dir.join("other.key.pem");
    let other_cer_path = team_paths.certificates_dir.join("other.cer");
    let other_p12_path = team_paths.certificates_dir.join("other.p12");
    for path in [
        &current_profile_path,
        &other_profile_path,
        &current_key_path,
        &current_cer_path,
        &current_p12_path,
        &other_key_path,
        &other_cer_path,
        &other_p12_path,
    ] {
        std::fs::write(path, b"fixture").unwrap();
    }

    let state = SigningState {
        certificates: vec![
            ManagedCertificate {
                id: "CERT-CURRENT".to_owned(),
                certificate_type: "83Q87W3TGH".to_owned(),
                serial_number: "CURRENT".to_owned(),
                origin: CertificateOrigin::Generated,
                display_name: None,
                system_keychain_path: None,
                system_signing_identity: None,
                private_key_path: current_key_path.clone(),
                certificate_der_path: current_cer_path.clone(),
                p12_path: current_p12_path.clone(),
                p12_password_account: "current-password".to_owned(),
            },
            ManagedCertificate {
                id: "CERT-OTHER".to_owned(),
                certificate_type: "83Q87W3TGH".to_owned(),
                serial_number: "OTHER".to_owned(),
                origin: CertificateOrigin::Generated,
                display_name: None,
                system_keychain_path: None,
                system_signing_identity: None,
                private_key_path: other_key_path.clone(),
                certificate_der_path: other_cer_path.clone(),
                p12_path: other_p12_path.clone(),
                p12_password_account: "other-password".to_owned(),
            },
        ],
        profiles: vec![
            ManagedProfile {
                id: "PROFILE-CURRENT".to_owned(),
                profile_type: "limited".to_owned(),
                bundle_id: "dev.orbit.fixture".to_owned(),
                path: current_profile_path.clone(),
                uuid: None,
                certificate_ids: vec!["CERT-CURRENT".to_owned()],
                device_ids: Vec::new(),
            },
            ManagedProfile {
                id: "PROFILE-OTHER".to_owned(),
                profile_type: "limited".to_owned(),
                bundle_id: "dev.orbit.other".to_owned(),
                path: other_profile_path.clone(),
                uuid: None,
                certificate_ids: vec!["CERT-OTHER".to_owned()],
                device_ids: Vec::new(),
            },
        ],
    };
    save_state(&project, team_id, &state).unwrap();

    let summary = clean_local_signing_state(&project).unwrap();
    assert_eq!(summary.removed_profiles, 1);
    assert_eq!(summary.removed_certificates, 1);
    assert!(!current_profile_path.exists());
    assert!(!current_p12_path.exists());
    assert!(other_profile_path.exists());
    assert!(other_p12_path.exists());

    let cleaned = load_state(&project, team_id).unwrap();
    assert_eq!(cleaned.profiles.len(), 1);
    assert_eq!(cleaned.profiles[0].id, "PROFILE-OTHER");
    assert_eq!(cleaned.certificates.len(), 1);
    assert_eq!(cleaned.certificates[0].id, "CERT-OTHER");
}

#[test]
fn local_team_resolution_ignores_global_auth_team_selection() {
    let (_temp, mut project) = test_project();
    project.resolved_manifest.team_id = None;
    std::fs::write(
        &project.manifest_path,
        serde_json::to_vec_pretty(&json!({
            "name": "OrbitFixture",
            "bundle_id": "dev.orbit.fixture",
            "version": "0.1.0",
            "build": 1,
            "platforms": { "ios": "18.0" },
            "sources": ["Sources/App"]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        &project.app.global_paths.auth_state_path,
        serde_json::to_vec_pretty(&json!({
            "last_mode": "user",
            "user": {
                "apple_id": "dev@example.com",
                "team_id": "TEAM999999",
                "provider_id": null,
                "provider_name": "Old Team",
                "last_validated_at_unix": 123
            },
            "api_key": null
        }))
        .unwrap(),
    )
    .unwrap();

    assert_ne!(
        resolve_local_team_id_if_known(&project).unwrap().as_deref(),
        Some("TEAM999999")
    );
}

#[test]
fn local_team_resolution_observes_manifest_saved_after_project_load() {
    let (_temp, mut project) = test_project();
    project.resolved_manifest.team_id = None;
    std::fs::write(
        &project.manifest_path,
        serde_json::to_vec_pretty(&json!({
            "name": "OrbitFixture",
            "bundle_id": "dev.orbit.fixture",
            "version": "0.1.0",
            "build": 1,
            "team_id": "TEAM123456",
            "platforms": { "ios": "18.0" },
            "sources": ["Sources/App"]
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        resolve_local_team_id_if_known(&project).unwrap().as_deref(),
        Some("TEAM123456")
    );
}

#[test]
fn profile_reuse_accepts_remote_device_superset() {
    assert!(profile_covers_requested_ids(
        &["MAC-1".to_owned(), "MAC-2".to_owned()],
        &["MAC-2".to_owned()]
    ));
    assert!(!profile_covers_requested_ids(
        &["MAC-1".to_owned()],
        &["MAC-1".to_owned(), "MAC-2".to_owned()]
    ));
    assert!(profile_covers_requested_ids(&[], &[]));
}

#[test]
fn collects_project_identifier_cleanup_inputs_from_entitlements() {
    let (_temp, mut project) = test_project();
    let entitlements_path = project.root.join("App.entitlements");
    let entitlements = Value::Dictionary(plist::Dictionary::from_iter([
        (
            "com.apple.security.application-groups".to_owned(),
            Value::Array(vec![Value::String("group.dev.orbit.fixture".to_owned())]),
        ),
        (
            "com.apple.developer.in-app-payments".to_owned(),
            Value::Array(vec![Value::String("merchant.dev.orbit.fixture".to_owned())]),
        ),
        (
            "com.apple.developer.icloud-container-identifiers".to_owned(),
            Value::Array(vec![Value::String("iCloud.dev.orbit.fixture".to_owned())]),
        ),
    ]));
    entitlements.to_file_xml(&entitlements_path).unwrap();
    project.resolved_manifest.targets[0].entitlements = Some("App.entitlements".into());

    let identifiers = project_entitlement_identifiers(&project).unwrap();
    assert_eq!(
        identifiers,
        ProjectEntitlementIdentifiers {
            app_groups: vec!["group.dev.orbit.fixture".to_owned()],
            merchant_ids: vec!["merchant.dev.orbit.fixture".to_owned()],
            cloud_containers: vec!["iCloud.dev.orbit.fixture".to_owned()],
        }
    );
}

#[test]
fn materializes_app_clip_entitlements_for_clip_and_host_app() {
    let (_temp, mut project) = test_project();
    let clip_entitlements_path = project.root.join("Clip.entitlements");
    let profile_path = project.root.join("Clip.mobileprovision");

    Value::Dictionary(plist::Dictionary::from_iter([(
        "com.apple.developer.parent-application-identifiers".to_owned(),
        Value::Array(vec![Value::String(
            "$(AppIdentifierPrefix)dev.orbit.fixture".to_owned(),
        )]),
    )]))
    .to_file_xml(&clip_entitlements_path)
    .unwrap();
    Value::Dictionary(plist::Dictionary::from_iter([(
        "ApplicationIdentifierPrefix".to_owned(),
        Value::Array(vec![Value::String("TEAM123456".to_owned())]),
    )]))
    .to_file_xml(&profile_path)
    .unwrap();

    project.resolved_manifest.targets[0]
        .dependencies
        .push("ExampleClip".to_owned());
    project.resolved_manifest.targets.push(TargetManifest {
        name: "ExampleClip".to_owned(),
        kind: TargetKind::App,
        bundle_id: "dev.orbit.fixture.clip".to_owned(),
        display_name: None,
        build_number: None,
        platforms: vec![ApplePlatform::Ios],
        sources: vec![project.root.join("Sources/Clip")],
        resources: Vec::new(),
        dependencies: Vec::new(),
        frameworks: Vec::new(),
        weak_frameworks: Vec::new(),
        system_libraries: Vec::new(),
        xcframeworks: Vec::new(),
        swift_packages: Vec::new(),
        info_plist: BTreeMap::new(),
        ios: None,
        entitlements: Some("Clip.entitlements".into()),
        push: None,
        extension: None,
    });

    let clip = project
        .resolved_manifest
        .resolve_target(Some("ExampleClip"))
        .unwrap();
    assert!(target_is_app_clip(&project, clip).unwrap());
    let clip_entitlements = materialize_signing_entitlements(&project, clip, &profile_path)
        .unwrap()
        .unwrap();
    let clip_dictionary = plist::Value::from_file(&clip_entitlements)
        .unwrap()
        .into_dictionary()
        .unwrap();
    assert_eq!(
        clip_dictionary
            .get("com.apple.developer.parent-application-identifiers")
            .and_then(plist::Value::as_array)
            .unwrap()[0]
            .as_string()
            .unwrap(),
        "TEAM123456.dev.orbit.fixture"
    );
    assert_eq!(
        clip_dictionary
            .get("com.apple.developer.on-demand-install-capable")
            .and_then(plist::Value::as_boolean),
        Some(true)
    );

    let host = project
        .resolved_manifest
        .resolve_target(Some("ExampleApp"))
        .unwrap();
    let host_entitlements = materialize_signing_entitlements(&project, host, &profile_path)
        .unwrap()
        .unwrap();
    let host_dictionary = plist::Value::from_file(&host_entitlements)
        .unwrap()
        .into_dictionary()
        .unwrap();
    assert_eq!(
        host_dictionary
            .get("com.apple.developer.associated-appclip-app-identifiers")
            .and_then(plist::Value::as_array)
            .unwrap()[0]
            .as_string()
            .unwrap(),
        "TEAM123456.dev.orbit.fixture.clip"
    );
}

#[test]
fn materializes_profile_entitlements_when_target_has_no_entitlements_file() {
    let (_temp, project) = test_project();
    let profile_path = project.root.join("Example.mobileprovision");
    Value::Dictionary(plist::Dictionary::from_iter([
        (
            "ApplicationIdentifierPrefix".to_owned(),
            Value::Array(vec![Value::String("TEAM123456".to_owned())]),
        ),
        (
            "Entitlements".to_owned(),
            Value::Dictionary(plist::Dictionary::from_iter([
                (
                    "application-identifier".to_owned(),
                    Value::String("TEAM123456.dev.orbit.fixture".to_owned()),
                ),
                (
                    "com.apple.developer.team-identifier".to_owned(),
                    Value::String("TEAM123456".to_owned()),
                ),
                ("get-task-allow".to_owned(), Value::Boolean(true)),
                (
                    "keychain-access-groups".to_owned(),
                    Value::Array(vec![Value::String("TEAM123456.*".to_owned())]),
                ),
                (
                    "com.apple.developer.game-center".to_owned(),
                    Value::Boolean(true),
                ),
            ])),
        ),
    ]))
    .to_file_xml(&profile_path)
    .unwrap();

    let target = project
        .resolved_manifest
        .resolve_target(Some("ExampleApp"))
        .unwrap();
    let generated = materialize_signing_entitlements(&project, target, &profile_path)
        .unwrap()
        .unwrap();
    let dictionary = plist::Value::from_file(&generated)
        .unwrap()
        .into_dictionary()
        .unwrap();
    assert_eq!(
        dictionary
            .get("application-identifier")
            .and_then(plist::Value::as_string),
        Some("TEAM123456.dev.orbit.fixture")
    );
    assert_eq!(
        dictionary
            .get("com.apple.developer.team-identifier")
            .and_then(plist::Value::as_string),
        Some("TEAM123456")
    );
    assert_eq!(
        dictionary
            .get("get-task-allow")
            .and_then(plist::Value::as_boolean),
        Some(true)
    );
}

#[test]
fn merges_managed_profile_entitlements_into_explicit_entitlements() {
    let (_temp, mut project) = test_project();
    let entitlements_path = project.root.join("App.entitlements");
    let profile_path = project.root.join("Example.mobileprovision");

    Value::Dictionary(plist::Dictionary::from_iter([(
        "aps-environment".to_owned(),
        Value::String("development".to_owned()),
    )]))
    .to_file_xml(&entitlements_path)
    .unwrap();
    Value::Dictionary(plist::Dictionary::from_iter([
        (
            "ApplicationIdentifierPrefix".to_owned(),
            Value::Array(vec![Value::String("TEAM123456".to_owned())]),
        ),
        (
            "Entitlements".to_owned(),
            Value::Dictionary(plist::Dictionary::from_iter([
                (
                    "application-identifier".to_owned(),
                    Value::String("TEAM123456.dev.orbit.fixture".to_owned()),
                ),
                (
                    "com.apple.developer.team-identifier".to_owned(),
                    Value::String("TEAM123456".to_owned()),
                ),
                ("get-task-allow".to_owned(), Value::Boolean(true)),
                (
                    "keychain-access-groups".to_owned(),
                    Value::Array(vec![Value::String("TEAM123456.*".to_owned())]),
                ),
            ])),
        ),
    ]))
    .to_file_xml(&profile_path)
    .unwrap();
    project.resolved_manifest.targets[0].entitlements = Some("App.entitlements".into());

    let target = project
        .resolved_manifest
        .resolve_target(Some("ExampleApp"))
        .unwrap();
    let generated = materialize_signing_entitlements(&project, target, &profile_path)
        .unwrap()
        .unwrap();
    let dictionary = plist::Value::from_file(&generated)
        .unwrap()
        .into_dictionary()
        .unwrap();
    assert_eq!(
        dictionary
            .get("aps-environment")
            .and_then(plist::Value::as_string),
        Some("development")
    );
    assert_eq!(
        dictionary
            .get("application-identifier")
            .and_then(plist::Value::as_string),
        Some("TEAM123456.dev.orbit.fixture")
    );
    assert_eq!(
        dictionary
            .get("keychain-access-groups")
            .and_then(plist::Value::as_array)
            .map(|values| values.len()),
        Some(1)
    );
}

#[test]
fn api_key_capability_mutations_fail_for_identifier_linking() {
    let error = plan_asc_capability_mutations(
        &[CapabilityUpdate {
            capability_type: "APP_GROUPS".to_owned(),
            option: "ON".to_owned(),
            relationships: CapabilityRelationships {
                app_groups: Some(vec!["group.dev.orbit.fixture".to_owned()]),
                merchant_ids: None,
                cloud_containers: None,
            },
        }],
        &[],
    )
    .unwrap_err();

    assert!(error.to_string().contains("cannot link App Groups"));
}

#[test]
fn api_key_capability_mutations_fail_for_broadcast_push() {
    let error = asc_capability_settings(&CapabilityUpdate {
        capability_type: "PUSH_NOTIFICATIONS".to_owned(),
        option: ASC_OPTION_PUSH_BROADCAST.to_owned(),
        relationships: CapabilityRelationships::default(),
    })
    .unwrap_err();

    assert!(error.to_string().contains("broadcast push"));
}

#[test]
fn api_key_capability_mutations_build_expected_settings() {
    let remote = vec![RemoteCapability {
        id: "CAP-APPLE-ID".to_owned(),
        capability_type: "APPLE_ID_AUTH".to_owned(),
        enabled: Some(true),
        settings: Vec::new(),
    }];
    let updates = vec![
        CapabilityUpdate {
            capability_type: "APPLE_ID_AUTH".to_owned(),
            option: ASC_OPTION_APPLE_ID_PRIMARY_CONSENT.to_owned(),
            relationships: CapabilityRelationships::default(),
        },
        CapabilityUpdate {
            capability_type: "DATA_PROTECTION".to_owned(),
            option: ASC_OPTION_DATA_PROTECTION_COMPLETE.to_owned(),
            relationships: CapabilityRelationships::default(),
        },
    ];

    let mutations = plan_asc_capability_mutations(&updates, &remote).unwrap();
    assert_eq!(mutations.len(), 2);
    assert_eq!(mutations[0].remote_id.as_deref(), Some("CAP-APPLE-ID"));
    assert_eq!(mutations[0].settings[0].key, "APPLE_ID_AUTH_APP_CONSENT");
    assert_eq!(
        mutations[0].settings[0].options[0].key,
        "PRIMARY_APP_CONSENT"
    );
    assert_eq!(
        mutations[1].settings[0].key,
        "DATA_PROTECTION_PERMISSION_LEVEL"
    );
    assert_eq!(
        mutations[1].settings[0].options[0].key,
        "COMPLETE_PROTECTION"
    );
}

#[test]
fn api_key_profile_type_uses_ios_profiles_for_watch_and_vision_targets() {
    let profile = ProfileManifest::new(BuildConfiguration::Release, DistributionKind::AppStore);

    assert_eq!(
        super::profile_types::asc_profile_type(ApplePlatform::Watchos, &profile).unwrap(),
        "IOS_APP_STORE"
    );
    assert_eq!(
        super::profile_types::asc_profile_type(ApplePlatform::Visionos, &profile).unwrap(),
        "IOS_APP_STORE"
    );
}

#[test]
fn plain_push_flag_is_allowed_with_api_key_auth() {
    let (_temp, mut project) = test_project();
    project.resolved_manifest.targets[0].push = Some(crate::manifest::PushManifest {
        broadcast_for_live_activities: false,
    });
    let target = &project.resolved_manifest.targets[0];
    let options = validate_push_setup_with_api_key(target);
    assert!(options.uses_push_notifications);
    assert!(!options.uses_broadcast_push_notifications);
}

#[test]
fn api_key_path_warns_and_skips_broadcast_push_setting() {
    let (_temp, mut project) = test_project();
    project.resolved_manifest.targets[0].push = Some(crate::manifest::PushManifest {
        broadcast_for_live_activities: true,
    });

    let target = &project.resolved_manifest.targets[0];
    let options = validate_push_setup_with_api_key(target);
    assert!(options.uses_push_notifications);
    assert!(!options.uses_broadcast_push_notifications);
}

#[test]
fn ad_hoc_device_helpers_match_eas_style_reuse_logic() {
    let devices = vec![
        CachedDevice {
            id: "DEV-1".to_owned(),
            name: "Alice iPhone".to_owned(),
            udid: "UDID-1".to_owned(),
            platform: "IOS".to_owned(),
            status: "ENABLED".to_owned(),
            device_class: Some("IPHONE".to_owned()),
            model: Some("iPhone17,1".to_owned()),
            created_at: Some("2026-03-30T00:00:00Z".to_owned()),
        },
        CachedDevice {
            id: "DEV-2".to_owned(),
            name: "Bob iPad".to_owned(),
            udid: "UDID-2".to_owned(),
            platform: "IOS".to_owned(),
            status: "ENABLED".to_owned(),
            device_class: Some("IPAD".to_owned()),
            model: Some("iPad16,3".to_owned()),
            created_at: None,
        },
    ];
    let profile = ManagedProfile {
        id: "PROFILE".to_owned(),
        profile_type: "IOS_APP_ADHOC".to_owned(),
        bundle_id: "dev.orbit.fixture".to_owned(),
        path: std::path::PathBuf::from("/tmp/profile.mobileprovision"),
        uuid: None,
        certificate_ids: vec!["CERT".to_owned()],
        device_ids: vec!["DEV-1".to_owned()],
    };

    assert_eq!(profile_udids(&profile, &devices), vec!["UDID-1".to_owned()]);
    assert!(!same_udid_set(
        &devices
            .iter()
            .map(|device| device.udid.clone())
            .collect::<Vec<_>>(),
        &profile_udids(&profile, &devices),
    ));
    let missing = missing_registered_devices(&devices, &profile_udids(&profile, &devices));
    assert_eq!(missing.len(), 1);
    assert!(format_cached_device_label(&devices[0]).contains("UDID-1"));
    assert!(format_cached_device_label(&devices[0]).contains("Alice iPhone"));
}

#[test]
fn current_profile_lookup_prefers_latest_matching_profile() {
    let first = ManagedProfile {
        id: "PROFILE-OLD".to_owned(),
        profile_type: "IOS_APP_ADHOC".to_owned(),
        bundle_id: "dev.orbit.fixture".to_owned(),
        path: std::path::PathBuf::from("/tmp/old.mobileprovision"),
        uuid: None,
        certificate_ids: vec!["CERT".to_owned()],
        device_ids: vec!["DEV-1".to_owned()],
    };
    let second = ManagedProfile {
        id: "PROFILE-NEW".to_owned(),
        profile_type: "IOS_APP_ADHOC".to_owned(),
        bundle_id: "dev.orbit.fixture".to_owned(),
        path: std::path::PathBuf::from("/tmp/new.mobileprovision"),
        uuid: None,
        certificate_ids: vec!["CERT".to_owned()],
        device_ids: vec!["DEV-2".to_owned()],
    };
    std::fs::write(&first.path, b"old").unwrap();
    std::fs::write(&second.path, b"new").unwrap();
    let state = SigningState {
        certificates: Vec::new(),
        profiles: vec![first, second],
    };

    let profile =
        current_profile_for_target(&state, "dev.orbit.fixture", "IOS_APP_ADHOC", "CERT").unwrap();
    assert_eq!(profile.id, "PROFILE-NEW");
}
