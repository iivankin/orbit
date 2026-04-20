use std::collections::BTreeMap;

use plist::Value;
use serde_json::json;
use tempfile::TempDir;

use super::entitlements::materialize_signing_entitlements;
use super::prepare_signing;
use super::{
    CertificateOrigin, ManagedCertificate, ManagedProfile, SigningState, clean_local_signing_state,
    identifier_name, load_state, resolve_local_team_id_if_known, save_state, target_is_app_clip,
    team_signing_paths,
};
use crate::context::{AppContext, GlobalPaths, ProjectContext, ProjectPaths};
use crate::manifest::{
    ApplePlatform, BuildConfiguration, DistributionKind, HooksManifest, ManifestSchema,
    PlatformManifest, ProfileManifest, QualityManifest, ResolvedManifest, TargetKind,
    TargetManifest, TestsManifest,
};

fn test_project() -> (TempDir, ProjectContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let data_dir = temp.path().join("data");
    let cache_dir = temp.path().join("cache");
    let orbi_dir = root.join(".orbi");
    let build_dir = orbi_dir.join("build");
    let artifacts_dir = orbi_dir.join("artifacts");
    let receipts_dir = orbi_dir.join("receipts");
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::create_dir_all(&artifacts_dir).unwrap();
    std::fs::create_dir_all(&receipts_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manifest = ResolvedManifest {
        name: "OrbiFixture".to_owned(),
        version: "0.1.0".to_owned(),
        xcode: None,
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
            bundle_id: "dev.orbi.fixture".to_owned(),
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
    let manifest_path = root.join("orbi.json");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&json!({
            "$schema": crate::apple::manifest::SCHEMA_URL,
            "name": "OrbiFixture",
            "bundle_id": "dev.orbi.fixture",
            "version": "0.1.0",
            "build": 1,
            "platforms": { "ios": "18.0" },
            "sources": ["Sources/App"],
            "asc": {
                "team_id": "TEAM123456"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let app = AppContext {
        cwd: root.clone(),
        interactive: false,
        verbose: false,
        manifest_env: None,
        global_paths: GlobalPaths {
            data_dir: data_dir.clone(),
            cache_dir,
            schema_dir: data_dir.join("schemas"),
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
            orbi_dir,
            build_dir,
            artifacts_dir,
            receipts_dir,
        },
    };
    (temp, project)
}

fn configure_project_for_macos_local_signing(project: &mut ProjectContext) {
    project.resolved_manifest.platforms = BTreeMap::from([(
        ApplePlatform::Macos,
        PlatformManifest {
            deployment_target: "15.0".to_owned(),
            universal_binary: false,
        },
    )]);
    project.resolved_manifest.targets[0].platforms = vec![ApplePlatform::Macos];
    let manifest = json!({
        "$schema": crate::apple::manifest::SCHEMA_URL,
        "name": "OrbiFixture",
        "bundle_id": "dev.orbi.fixture",
        "version": "0.1.0",
        "build": 1,
        "platforms": { "macos": "15.0" },
        "sources": ["Sources/App"]
    });
    std::fs::write(
        &project.manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn identifier_name_normalizes_portal_identifiers() {
    assert_eq!(
        identifier_name("App Group", "group.dev.orbi.demo"),
        "App Group group dev orbi demo"
    );
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
                bundle_id: "dev.orbi.fixture".to_owned(),
                path: current_profile_path.clone(),
                uuid: None,
                certificate_ids: vec!["CERT-CURRENT".to_owned()],
                device_ids: Vec::new(),
            },
            ManagedProfile {
                id: "PROFILE-OTHER".to_owned(),
                profile_type: "limited".to_owned(),
                bundle_id: "dev.orbi.other".to_owned(),
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
    let (_temp, project) = test_project();
    std::fs::write(
        &project.manifest_path,
        serde_json::to_vec_pretty(&json!({
            "name": "OrbiFixture",
            "bundle_id": "dev.orbi.fixture",
            "version": "0.1.0",
            "build": 1,
            "platforms": { "ios": "18.0" },
            "sources": ["Sources/App"]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        project.app.global_paths.data_dir.join("auth.json"),
        serde_json::to_vec_pretty(&json!({
            "last_mode": "user",
            "user": {
                "apple_id": "dev@example.com",
                "team_id": "TEAM999999",
                "provider_name": "Old Team",
                "last_validated_at_unix": 123
            },
            "api_key": null
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(resolve_local_team_id_if_known(&project).unwrap(), None);
}

#[test]
fn local_team_resolution_observes_manifest_saved_after_project_load() {
    let (_temp, project) = test_project();
    std::fs::write(
        &project.manifest_path,
        serde_json::to_vec_pretty(&json!({
            "name": "OrbiFixture",
            "bundle_id": "dev.orbi.fixture",
            "version": "0.1.0",
            "build": 1,
            "platforms": { "ios": "18.0" },
            "sources": ["Sources/App"],
            "asc": {
                "team_id": "TEAM123456"
            }
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
fn materializes_app_clip_entitlements_for_clip_and_host_app() {
    let (_temp, mut project) = test_project();
    let clip_entitlements_path = project.root.join("Clip.entitlements");
    let profile_path = project.root.join("Clip.mobileprovision");

    Value::Dictionary(plist::Dictionary::from_iter([(
        "com.apple.developer.parent-application-identifiers".to_owned(),
        Value::Array(vec![Value::String(
            "$(AppIdentifierPrefix)dev.orbi.fixture".to_owned(),
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
        bundle_id: "dev.orbi.fixture.clip".to_owned(),
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
        "TEAM123456.dev.orbi.fixture"
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
        "TEAM123456.dev.orbi.fixture.clip"
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
                    Value::String("TEAM123456.dev.orbi.fixture".to_owned()),
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
        Some("TEAM123456.dev.orbi.fixture")
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
                    Value::String("TEAM123456.dev.orbi.fixture".to_owned()),
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
        Some("TEAM123456.dev.orbi.fixture")
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
fn macos_development_signing_without_embedded_asc_uses_ad_hoc_signature() {
    let (_temp, mut project) = test_project();
    configure_project_for_macos_local_signing(&mut project);

    let target = project
        .resolved_manifest
        .resolve_target(Some("ExampleApp"))
        .unwrap();
    let material = prepare_signing(
        &project,
        target,
        ApplePlatform::Macos,
        &ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development),
        None,
    )
    .unwrap();

    assert_eq!(material.signing_identity, "-");
    assert!(material.keychain_path.is_none());
    assert!(material.provisioning_profile_path.is_none());

    let entitlements_path = material
        .entitlements_path
        .expect("expected local macOS development entitlements");
    let entitlements = plist::Value::from_file(&entitlements_path)
        .unwrap()
        .into_dictionary()
        .unwrap();
    assert_eq!(
        entitlements
            .get("com.apple.security.get-task-allow")
            .and_then(plist::Value::as_boolean),
        Some(true)
    );
}

#[test]
fn macos_development_signing_without_embedded_asc_rejects_profile_backed_entitlements() {
    let (_temp, mut project) = test_project();
    configure_project_for_macos_local_signing(&mut project);

    let entitlements_path = project.root.join("App.entitlements");
    Value::Dictionary(plist::Dictionary::from_iter([(
        "com.apple.developer.associated-domains".to_owned(),
        Value::Array(vec![Value::String("applinks:example.com".to_owned())]),
    )]))
    .to_file_xml(&entitlements_path)
    .unwrap();
    project.resolved_manifest.targets[0].entitlements = Some("App.entitlements".into());

    let target = project
        .resolved_manifest
        .resolve_target(Some("ExampleApp"))
        .unwrap();
    let error = prepare_signing(
        &project,
        target,
        ApplePlatform::Macos,
        &ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development),
        None,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("local macOS development fallback only supports unrestricted `com.apple.security.*` entitlements")
    );
    assert!(
        error
            .to_string()
            .contains("com.apple.developer.associated-domains")
    );
}
