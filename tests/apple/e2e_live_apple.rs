#![cfg(target_os = "macos")]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orbit::apple::asc_api::AscClient;
use orbit::apple::auth::ApiKeyAuth;
use orbit::apple::developer_services::DeveloperServicesClient;
use orbit::apple::provisioning::ProvisioningClient;
use orbit::context::AppContext;
use serde_json::Value;

use crate::support::{
    LiveAppleConfig, LiveAscConfig, LiveCleanupGuard, create_live_workspace,
    create_live_workspace_with_manifest, latest_receipt_path, live_asc_command, live_command,
    live_command_without_team_state, remote_asc_certificates_for_type,
    remote_capabilities_for_bundle_id, remote_certificates_for_type, remote_devices_for_platform,
    remote_profiles_for_bundle_id, require_live_apple_config, require_live_asc_config,
    run_and_capture, wait_for_remote_app_group_count, wait_for_remote_capability_state,
    wait_for_remote_cloud_container_count, wait_for_remote_merchant_id_count,
    wait_for_remote_profile_count,
};

fn metric_value(output: &str, key: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim() != key {
            return None;
        }
        value.trim().parse::<u32>().ok()
    })
}

fn current_machine_provisioning_udid() -> Option<String> {
    let output = Command::new("system_profiler")
        .args(["-json", "SPHardwareDataType"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    value
        .get("SPHardwareDataType")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|entry| entry.get("provisioning_UDID"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn ready_profile_id(output: &std::process::Output) -> Option<String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}\n{stderr}").lines().find_map(|line| {
        if !line.contains("Provisioning profile ready for target `") {
            return None;
        }
        let (_, profile_id) = line.rsplit_once(": ")?;
        Some(profile_id.trim().trim_end_matches('.').to_owned())
    })
}

fn write_manifest(workspace: &Path, manifest: &Value) {
    fs::write(
        workspace.join("orbit.json"),
        serde_json::to_vec_pretty(manifest).unwrap(),
    )
    .unwrap();
}

fn write_swift_source(workspace: &Path, relative_path: &str, contents: &str) {
    let path = workspace.join(relative_path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn ios_manifest(
    config: &LiveAppleConfig,
    app_name: &str,
    bundle_id: &str,
    build: u64,
    entitlements: Value,
) -> Value {
    let mut manifest = serde_json::json!({
        "$schema": config.schema_path,
        "name": app_name,
        "bundle_id": bundle_id,
        "version": "1.0.0",
        "build": build,
        "team_id": config.team_id,
        "platforms": {
            "ios": "18.0"
        },
        "sources": ["Sources/App"],
        "entitlements": entitlements
    });
    if let Some(provider_id) = &config.provider_id {
        manifest["provider_id"] = serde_json::Value::String(provider_id.clone());
    }
    manifest
}

fn ios_container_entitlements(
    app_group_id: &str,
    merchant_id: &str,
    cloud_container_id: &str,
) -> Value {
    serde_json::json!({
        "app_groups": [app_group_id],
        "merchant_ids": [merchant_id],
        "cloud_containers": [cloud_container_id],
        "icloud_services": ["CloudDocuments"]
    })
}

fn build_ios_distribution(
    workspace: &Path,
    config: &LiveAppleConfig,
    distribution: &str,
    extra_args: &[&str],
) -> std::process::Output {
    build_distribution_with_command(
        live_command(workspace, config),
        workspace,
        "ios",
        distribution,
        extra_args,
    )
}

fn build_ios_distribution_without_team_state(
    workspace: &Path,
    config: &LiveAppleConfig,
    distribution: &str,
    extra_args: &[&str],
) -> std::process::Output {
    build_distribution_with_command(
        live_command_without_team_state(workspace, config),
        workspace,
        "ios",
        distribution,
        extra_args,
    )
}

fn build_distribution_with_command(
    mut command: Command,
    workspace: &Path,
    platform: &str,
    distribution: &str,
    extra_args: &[&str],
) -> std::process::Output {
    let manifest_path = workspace.join("orbit.json");
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "build",
        "--platform",
        platform,
        "--distribution",
        distribution,
    ]);
    command.args(extra_args);
    run_and_capture(&mut command)
}

fn signing_state_path(workspace: &Path, config: &LiveAppleConfig) -> PathBuf {
    LiveAppleConfig::orbit_data_dir(workspace)
        .join("teams")
        .join(&config.team_id)
        .join("signing.json")
}

fn read_signing_state(workspace: &Path, config: &LiveAppleConfig) -> Value {
    serde_json::from_slice(&fs::read(signing_state_path(workspace, config)).unwrap()).unwrap()
}

fn write_signing_state(workspace: &Path, config: &LiveAppleConfig, state: &Value) {
    fs::write(
        signing_state_path(workspace, config),
        serde_json::to_vec_pretty(state).unwrap(),
    )
    .unwrap();
}

fn certificate_id_for_bundle(state: &Value, bundle_id: &str) -> String {
    state["profiles"]
        .as_array()
        .expect("expected signing state profiles array")
        .iter()
        .find(|profile| profile["bundle_id"].as_str() == Some(bundle_id))
        .and_then(|profile| profile["certificate_ids"].as_array())
        .and_then(|certificate_ids| certificate_ids.first())
        .and_then(Value::as_str)
        .expect("expected signing state profile to reference a signing certificate")
        .to_owned()
}

fn certificate_p12_path_for_id(state: &Value, certificate_id: &str) -> PathBuf {
    PathBuf::from(
        state["certificates"]
            .as_array()
            .expect("expected signing state certificates array")
            .iter()
            .find(|certificate| certificate["id"].as_str() == Some(certificate_id))
            .and_then(|certificate| certificate["p12_path"].as_str())
            .expect("expected signing state certificate to include a p12_path"),
    )
}

fn profile_id_for_bundle(state: &Value, bundle_id: &str) -> String {
    state["profiles"]
        .as_array()
        .expect("expected signing state profiles array")
        .iter()
        .find(|profile| profile["bundle_id"].as_str() == Some(bundle_id))
        .and_then(|profile| profile["id"].as_str())
        .expect("expected signing state profile to include an id")
        .to_owned()
}

fn certificate_id_for_type(state: &Value, certificate_type: &str) -> String {
    state["certificates"]
        .as_array()
        .expect("expected signing state certificates array")
        .iter()
        .find(|certificate| certificate["certificate_type"].as_str() == Some(certificate_type))
        .and_then(|certificate| certificate["id"].as_str())
        .expect("expected signing state to include a certificate for the requested type")
        .to_owned()
}

fn asc_client(config: &LiveAscConfig) -> AscClient {
    AscClient::new(ApiKeyAuth {
        api_key_path: config.api_key_path.clone(),
        key_id: config.key_id.clone(),
        issuer_id: config.issuer_id.clone(),
        team_id: Some(config.apple.team_id.clone()),
    })
    .unwrap()
}

fn assert_single_ios_app_store_profile(config: &LiveAppleConfig, bundle_id: &str) {
    let profiles = remote_profiles_for_bundle_id(config, bundle_id, Some("IOS_APP_STORE"));
    assert_eq!(
        profiles.len(),
        1,
        "expected exactly one IOS_APP_STORE profile for `{bundle_id}`"
    );
}

fn assert_single_remote_profile(config: &LiveAppleConfig, bundle_id: &str, profile_type: &str) {
    let profiles = remote_profiles_for_bundle_id(config, bundle_id, Some(profile_type));
    assert_eq!(
        profiles.len(),
        1,
        "expected exactly one {profile_type} profile for `{bundle_id}`"
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_developer_services_lists_configured_team() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let app = AppContext::new(true, false, None).unwrap();
    let developer_services = DeveloperServicesClient::authenticate(&app).unwrap();
    let teams = developer_services.list_teams().unwrap();
    assert!(
        teams.iter().any(|team| team.team_id == config.team_id),
        "expected configured team {} to be present in Developer Services team list",
        config.team_id
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_build_sign_provision_and_clean_remote_state() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("BuildClean");
    let workspace = create_live_workspace(temp.path(), &config, &app_name, &bundle_id);
    let mut cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let mut build = live_command(&workspace, &config);
    build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "ios",
        "--distribution",
        "app-store",
        "--release",
    ]);
    let build_output = run_and_capture(&mut build);
    assert!(
        build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let receipt_path = latest_receipt_path(&workspace);
    assert!(receipt_path.exists(), "missing build receipt");
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    let artifact_path = receipt["artifact_path"].as_str().unwrap();
    assert!(
        fs::metadata(artifact_path).is_ok(),
        "missing built artifact at {artifact_path}"
    );

    let mut clean = live_command(&workspace, &config);
    clean.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "clean",
        "--all",
    ]);
    let clean_output = run_and_capture(&mut clean);
    assert!(
        clean_output.status.success(),
        "{}",
        String::from_utf8_lossy(&clean_output.stderr)
    );
    assert!(
        !workspace.join(".orbit").exists(),
        "local orbit state should be removed by clean --all"
    );

    let stdout = String::from_utf8_lossy(&clean_output.stdout);
    assert_eq!(metric_value(&stdout, "removed_remote_profiles"), Some(1));
    assert_eq!(metric_value(&stdout, "removed_remote_apps"), Some(1));
    cleanup.disarm();
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_device_register_current_machine_uses_apple_id_auth() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let current_udid = match current_machine_provisioning_udid() {
        Some(udid) => udid,
        None => {
            eprintln!(
                "skipping device register live test: current Mac does not expose provisioning_UDID"
            );
            return;
        }
    };
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("DeviceRegister");
    let workspace = create_live_workspace(temp.path(), &config, &app_name, &bundle_id);

    let mut list = live_command(&workspace, &config);
    list.args(["--non-interactive", "apple", "device", "list", "--refresh"]);
    let list_output = run_and_capture(&mut list);
    assert!(
        list_output.status.success(),
        "{}",
        String::from_utf8_lossy(&list_output.stderr)
    );
    let listed_devices = String::from_utf8_lossy(&list_output.stdout);
    let already_registered = listed_devices.contains(&current_udid);
    if !already_registered && std::env::var_os("ORBIT_RUN_LIVE_APPLE_DEVICE_MUTATION_E2E").is_none()
    {
        eprintln!(
            "skipping device register mutation: current Mac is not registered; set ORBIT_RUN_LIVE_APPLE_DEVICE_MUTATION_E2E=1 to allow creating and removing a real device record"
        );
        return;
    }

    let mut register = live_command(&workspace, &config);
    register.args([
        "--non-interactive",
        "apple",
        "device",
        "register",
        "--current-machine",
        "--platform",
        "macos",
    ]);
    let register_output = run_and_capture(&mut register);
    assert!(
        register_output.status.success(),
        "{}",
        String::from_utf8_lossy(&register_output.stderr)
    );
    let register_stdout = String::from_utf8_lossy(&register_output.stdout);
    let fields = register_stdout.trim().split('\t').collect::<Vec<_>>();
    assert!(
        fields.len() >= 5,
        "unexpected device register output: {register_stdout}"
    );
    assert!(
        fields[0] == "reused" || fields[0] == "created",
        "unexpected device register status: {register_stdout}"
    );
    assert_eq!(
        fields[3], current_udid,
        "device register returned wrong UDID"
    );
    if already_registered {
        assert_eq!(
            fields[0], "reused",
            "expected existing Mac device to be reused"
        );
        return;
    }

    let mut remove = live_command(&workspace, &config);
    remove.args([
        "--non-interactive",
        "apple",
        "device",
        "remove",
        "--id",
        fields[1],
    ]);
    let remove_output = run_and_capture(&mut remove);
    assert!(
        remove_output.status.success(),
        "{}",
        String::from_utf8_lossy(&remove_output.stderr)
    );
}

#[test]
#[ignore = "manual live Apple submit test"]
fn live_submit_uses_real_app_store_connect_account() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_SUBMIT_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("Submit");
    let workspace = create_live_workspace(temp.path(), &config, &app_name, &bundle_id);
    let mut cleanup = LiveCleanupGuard::local_only(&workspace, &config);

    let mut build = live_command(&workspace, &config);
    build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "ios",
        "--distribution",
        "app-store",
        "--release",
    ]);
    let build_output = run_and_capture(&mut build);
    assert!(
        build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let receipt_path = latest_receipt_path(&workspace);
    let mut submit = live_command(&workspace, &config);
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "submit",
        "--receipt",
        receipt_path.to_str().unwrap(),
    ]);
    let submit_output = run_and_capture(&mut submit);
    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );

    // After a real submit attempt Apple may keep the App Store Connect app record and may
    // refuse deleting the explicit App ID, so this test only clears local Orbit state.
    let mut clean = live_command(&workspace, &config);
    clean.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "clean",
        "--local",
    ]);
    let clean_output = run_and_capture(&mut clean);
    assert!(
        clean_output.status.success(),
        "{}",
        String::from_utf8_lossy(&clean_output.stderr)
    );
    assert!(
        !workspace.join(".orbit").exists(),
        "local orbit state should be removed by clean --local"
    );
    cleanup.disarm();
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_associated_domains_capability_removal_updates_remote_bundle_id() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("Entitlements");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "entitlements": {
                "associated_domains": ["applinks:live-e2e.orbit.dev"]
            }
        }),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let mut build = live_command(&workspace, &config);
    build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "ios",
        "--distribution",
        "app-store",
        "--release",
    ]);
    let first_build = run_and_capture(&mut build);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    let capability_types = remote_capabilities_for_bundle_id(&config, &bundle_id)
        .into_iter()
        .map(|capability| capability.capability_type)
        .collect::<Vec<_>>();
    assert!(
        capability_types
            .iter()
            .any(|capability| capability == "ASSOCIATED_DOMAINS"),
        "expected ASSOCIATED_DOMAINS capability after first build"
    );

    fs::write(
        workspace.join("orbit.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 2,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "entitlements": {}
        }))
        .unwrap(),
    )
    .unwrap();

    let mut second_build = live_command(&workspace, &config);
    second_build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "ios",
        "--distribution",
        "app-store",
        "--release",
    ]);
    let second_build_output = run_and_capture(&mut second_build);
    assert!(
        second_build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build_output.stderr)
    );

    let capability_types =
        wait_for_remote_capability_state(&config, &bundle_id, "ASSOCIATED_DOMAINS", false)
            .into_iter()
            .map(|capability| capability.capability_type)
            .collect::<Vec<_>>();
    assert!(
        !capability_types
            .iter()
            .any(|capability| capability == "ASSOCIATED_DOMAINS"),
        "expected ASSOCIATED_DOMAINS capability to be removed after entitlement deletion"
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_push_notifications_capability_syncs_to_bundle_id() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("Push");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "entitlements": {
                "push_notifications": true
            }
        }),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let mut build = live_command(&workspace, &config);
    build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "ios",
        "--distribution",
        "app-store",
        "--release",
    ]);
    let build_output = run_and_capture(&mut build);
    assert!(
        build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let capability_types = remote_capabilities_for_bundle_id(&config, &bundle_id)
        .into_iter()
        .map(|capability| capability.capability_type)
        .collect::<Vec<_>>();
    assert!(
        capability_types
            .iter()
            .any(|capability| capability == "PUSH_NOTIFICATIONS"),
        "expected PUSH_NOTIFICATIONS capability after build"
    );

    fs::write(
        workspace.join("orbit.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 2,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "entitlements": {}
        }))
        .unwrap(),
    )
    .unwrap();

    let mut second_build = live_command(&workspace, &config);
    second_build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "ios",
        "--distribution",
        "app-store",
        "--release",
    ]);
    let second_build_output = run_and_capture(&mut second_build);
    assert!(
        second_build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build_output.stderr)
    );

    let capability_types =
        wait_for_remote_capability_state(&config, &bundle_id, "PUSH_NOTIFICATIONS", false)
            .into_iter()
            .map(|capability| capability.capability_type)
            .collect::<Vec<_>>();
    assert!(
        !capability_types
            .iter()
            .any(|capability| capability == "PUSH_NOTIFICATIONS"),
        "expected PUSH_NOTIFICATIONS capability to be removed after entitlement deletion"
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_network_extension_capability_syncs_on_extension_bundle_id() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("NetworkExtension");
    let extension_bundle_id = format!("{bundle_id}.tunnel");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "tunnel": {
                    "kind": "packet-tunnel",
                    "sources": ["Sources/TunnelExtension"],
                    "entry": {
                        "class": "PacketTunnelProvider"
                    },
                    "entitlements": {
                        "network_extensions": ["packet-tunnel-provider"]
                    }
                }
            }
        }),
    );
    write_swift_source(
        &workspace,
        "Sources/TunnelExtension/PacketTunnelProvider.swift",
        "import Foundation\nfinal class PacketTunnelProvider: NSObject {}\n",
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    assert_single_ios_app_store_profile(&config, &bundle_id);
    assert_single_ios_app_store_profile(&config, &extension_bundle_id);
    wait_for_remote_capability_state(&config, &extension_bundle_id, "NETWORK_EXTENSIONS", true);

    write_manifest(
        &workspace,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 2,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "extensions": {
                "tunnel": {
                    "kind": "packet-tunnel",
                    "sources": ["Sources/TunnelExtension"],
                    "entry": {
                        "class": "PacketTunnelProvider"
                    },
                    "entitlements": {}
                }
            }
        }),
    );

    let second_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );

    let capability_types = wait_for_remote_capability_state(
        &config,
        &extension_bundle_id,
        "NETWORK_EXTENSIONS",
        false,
    )
    .into_iter()
    .map(|capability| capability.capability_type)
    .collect::<Vec<_>>();
    assert!(
        !capability_types
            .iter()
            .any(|capability| capability == "NETWORK_EXTENSIONS"),
        "expected NETWORK_EXTENSIONS capability to be removed after entitlement deletion"
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_file_provider_extension_syncs_testing_mode_capability() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("FileProvider");
    let extension_bundle_id = format!("{bundle_id}.provider");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
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
                    "entitlements": {
                        "fileprovider_testing_mode": true
                    }
                }
            }
        }),
    );
    write_swift_source(
        &workspace,
        "Sources/FileProviderExtension/FileProviderExtension.swift",
        "import Foundation\nfinal class FileProviderExtension: NSObject {}\n",
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    assert_single_ios_app_store_profile(&config, &bundle_id);
    assert_single_ios_app_store_profile(&config, &extension_bundle_id);
    wait_for_remote_capability_state(
        &config,
        &extension_bundle_id,
        "FILEPROVIDER_TESTINGMODE",
        true,
    );

    write_manifest(
        &workspace,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 2,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
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
                    "entitlements": {}
                }
            }
        }),
    );

    let second_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );

    let capability_types = wait_for_remote_capability_state(
        &config,
        &extension_bundle_id,
        "FILEPROVIDER_TESTINGMODE",
        false,
    )
    .into_iter()
    .map(|capability| capability.capability_type)
    .collect::<Vec<_>>();
    assert!(
        !capability_types
            .iter()
            .any(|capability| capability == "FILEPROVIDER_TESTINGMODE"),
        "expected FILEPROVIDER_TESTINGMODE capability to be removed after entitlement deletion"
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_container_backed_capabilities_sync_reuse_and_remove() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("ContainersLifecycle");
    let app_group_id = format!("group.{bundle_id}.shared");
    let merchant_id = format!("merchant.{bundle_id}.store");
    let cloud_container_id = format!("iCloud.{bundle_id}");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(
            &config,
            &app_name,
            &bundle_id,
            1,
            ios_container_entitlements(&app_group_id, &merchant_id, &cloud_container_id),
        ),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    wait_for_remote_capability_state(&config, &bundle_id, "APP_GROUPS", true);
    wait_for_remote_capability_state(&config, &bundle_id, "APPLE_PAY", true);
    wait_for_remote_capability_state(&config, &bundle_id, "ICLOUD", true);

    let first_app_group = wait_for_remote_app_group_count(&config, &app_group_id, 1)
        .remove(0)
        .id;
    let first_merchant = wait_for_remote_merchant_id_count(&config, &merchant_id, 1)
        .remove(0)
        .id;
    let first_cloud_container =
        wait_for_remote_cloud_container_count(&config, &cloud_container_id, 1)
            .remove(0)
            .id;

    let second_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );

    let second_app_group = wait_for_remote_app_group_count(&config, &app_group_id, 1)
        .remove(0)
        .id;
    let second_merchant = wait_for_remote_merchant_id_count(&config, &merchant_id, 1)
        .remove(0)
        .id;
    let second_cloud_container =
        wait_for_remote_cloud_container_count(&config, &cloud_container_id, 1)
            .remove(0)
            .id;
    assert_eq!(second_app_group, first_app_group);
    assert_eq!(second_merchant, first_merchant);
    assert_eq!(second_cloud_container, first_cloud_container);

    write_manifest(
        &workspace,
        &ios_manifest(&config, &app_name, &bundle_id, 2, serde_json::json!({})),
    );

    let removal_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        removal_build.status.success(),
        "{}",
        String::from_utf8_lossy(&removal_build.stderr)
    );

    let capability_types =
        wait_for_remote_capability_state(&config, &bundle_id, "APP_GROUPS", false)
            .into_iter()
            .map(|capability| capability.capability_type)
            .collect::<Vec<_>>();
    assert!(
        !capability_types
            .iter()
            .any(|capability| capability == "APP_GROUPS"),
        "expected APP_GROUPS capability to be removed after entitlement deletion"
    );
    let capability_types =
        wait_for_remote_capability_state(&config, &bundle_id, "APPLE_PAY", false)
            .into_iter()
            .map(|capability| capability.capability_type)
            .collect::<Vec<_>>();
    assert!(
        !capability_types
            .iter()
            .any(|capability| capability == "APPLE_PAY"),
        "expected APPLE_PAY capability to be removed after entitlement deletion"
    );
    let capability_types = wait_for_remote_capability_state(&config, &bundle_id, "ICLOUD", false)
        .into_iter()
        .map(|capability| capability.capability_type)
        .collect::<Vec<_>>();
    assert!(
        !capability_types
            .iter()
            .any(|capability| capability == "ICLOUD"),
        "expected ICLOUD capability to be removed after entitlement deletion"
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_clean_all_removes_remote_app_groups_and_merchants() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("IdentifiersClean");
    let app_group_id = format!("group.{bundle_id}.shared");
    let merchant_id = format!("merchant.{bundle_id}.store");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(
            &config,
            &app_name,
            &bundle_id,
            1,
            serde_json::json!({
                "app_groups": [app_group_id],
                "merchant_ids": [merchant_id]
            }),
        ),
    );
    let mut cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );
    let _first_profile_id = ready_profile_id(&first_build)
        .expect("expected first iOS app-store build to report a provisioning profile id");
    let first_app_group = wait_for_remote_app_group_count(&config, &app_group_id, 1)
        .remove(0)
        .id;
    let first_merchant = wait_for_remote_merchant_id_count(&config, &merchant_id, 1)
        .remove(0)
        .id;

    let second_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );
    let _second_profile_id = ready_profile_id(&second_build)
        .expect("expected second iOS app-store build to report a provisioning profile id");

    let second_app_group = wait_for_remote_app_group_count(&config, &app_group_id, 1)
        .remove(0)
        .id;
    let second_merchant = wait_for_remote_merchant_id_count(&config, &merchant_id, 1)
        .remove(0)
        .id;
    assert_eq!(second_app_group, first_app_group);
    assert_eq!(second_merchant, first_merchant);

    let mut clean = live_command(&workspace, &config);
    clean.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "clean",
        "--all",
    ]);
    let clean_output = run_and_capture(&mut clean);
    assert!(
        clean_output.status.success(),
        "{}",
        String::from_utf8_lossy(&clean_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&clean_output.stdout);
    assert_eq!(metric_value(&stdout, "removed_remote_profiles"), Some(1));
    assert_eq!(metric_value(&stdout, "removed_remote_apps"), Some(1));
    assert_eq!(metric_value(&stdout, "removed_remote_app_groups"), Some(1));
    assert_eq!(metric_value(&stdout, "removed_remote_merchants"), Some(1));
    assert_eq!(
        metric_value(&stdout, "removed_remote_cloud_containers"),
        Some(0)
    );

    assert!(
        !workspace.join(".orbit").exists(),
        "local orbit state should be removed by clean --all"
    );
    assert!(
        wait_for_remote_profile_count(&config, &bundle_id, None, 0).is_empty(),
        "expected clean --all to remove all remote profiles for the bundle id"
    );
    assert!(
        wait_for_remote_app_group_count(&config, &app_group_id, 0).is_empty(),
        "expected clean --all to remove the remote app group"
    );
    assert!(
        wait_for_remote_merchant_id_count(&config, &merchant_id, 0).is_empty(),
        "expected clean --all to remove the remote merchant id"
    );
    cleanup.disarm();
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_clean_all_skips_forbidden_cloud_container_cleanup() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("CloudContainerClean");
    let app_group_id = format!("group.{bundle_id}.shared");
    let merchant_id = format!("merchant.{bundle_id}.store");
    let cloud_container_id = format!("iCloud.{bundle_id}");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(
            &config,
            &app_name,
            &bundle_id,
            1,
            ios_container_entitlements(&app_group_id, &merchant_id, &cloud_container_id),
        ),
    );
    let mut cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    wait_for_remote_app_group_count(&config, &app_group_id, 1);
    wait_for_remote_merchant_id_count(&config, &merchant_id, 1);
    wait_for_remote_cloud_container_count(&config, &cloud_container_id, 1);

    let mut clean = live_command(&workspace, &config);
    clean.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "clean",
        "--all",
    ]);
    let clean_output = run_and_capture(&mut clean);
    assert!(
        clean_output.status.success(),
        "{}",
        String::from_utf8_lossy(&clean_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&clean_output.stdout);
    assert_eq!(metric_value(&stdout, "removed_remote_profiles"), Some(1));
    assert_eq!(metric_value(&stdout, "removed_remote_apps"), Some(1));
    assert_eq!(metric_value(&stdout, "removed_remote_app_groups"), Some(1));
    assert_eq!(metric_value(&stdout, "removed_remote_merchants"), Some(1));
    assert_eq!(
        metric_value(&stdout, "removed_remote_cloud_containers"),
        Some(0)
    );
    assert!(
        wait_for_remote_profile_count(&config, &bundle_id, None, 0).is_empty(),
        "expected clean --all to remove all remote profiles for the bundle id"
    );
    assert!(
        wait_for_remote_app_group_count(&config, &app_group_id, 0).is_empty(),
        "expected clean --all to remove the remote app group"
    );
    assert!(
        wait_for_remote_merchant_id_count(&config, &merchant_id, 0).is_empty(),
        "expected clean --all to remove the remote merchant id"
    );
    assert_eq!(
        wait_for_remote_cloud_container_count(&config, &cloud_container_id, 1).len(),
        1,
        "expected clean --all to leave the remote cloud container untouched when DS forbids deletion"
    );
    cleanup.disarm();
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_ios_app_clip_build_signs_host_and_clip_targets() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("AppClip");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "app_clip": {
                "sources": ["Sources/AppClip"]
            }
        }),
    );
    write_swift_source(
        &workspace,
        "Sources/AppClip/App.swift",
        "import SwiftUI\n@main struct ExampleAppClip: App { var body: some Scene { WindowGroup { Text(\"Clip\") } } }\n",
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    assert_single_ios_app_store_profile(&config, &bundle_id);
    assert_single_ios_app_store_profile(&config, &format!("{bundle_id}.clip"));
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_ios_extension_build_signs_host_and_extension_targets() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("ShareExtension");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
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
        }),
    );
    write_swift_source(
        &workspace,
        "Sources/ShareExtension/ShareViewController.swift",
        "import Foundation\nfinal class ShareViewController: NSObject {}\n",
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    assert_single_ios_app_store_profile(&config, &bundle_id);
    assert_single_ios_app_store_profile(&config, &format!("{bundle_id}.share"));
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_watch_companion_build_signs_host_watch_app_and_extension_targets() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("WatchCompanion");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "ios": "18.0",
                "watchos": "11.0"
            },
            "sources": ["Sources/App"],
            "watch": {
                "sources": ["Sources/WatchApp"],
                "extension": {
                    "sources": ["Sources/WatchExtension"],
                    "entry": {
                        "class": "WatchExtensionDelegate"
                    }
                }
            }
        }),
    );
    write_swift_source(
        &workspace,
        "Sources/WatchApp/App.swift",
        "import SwiftUI\n@main struct ExampleWatchApp: App { var body: some Scene { WindowGroup { Text(\"Watch\") } } }\n",
    );
    write_swift_source(
        &workspace,
        "Sources/WatchExtension/Extension.swift",
        "import SwiftUI\n@main struct ExampleWatchExtension: App { var body: some Scene { WindowGroup { Text(\"Watch Extension\") } } }\n",
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    assert_single_ios_app_store_profile(&config, &bundle_id);
    assert_single_ios_app_store_profile(&config, &format!("{bundle_id}.watchkitapp"));
    assert_single_ios_app_store_profile(
        &config,
        &format!("{bundle_id}.watchkitapp.watchkitextension"),
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_ios_development_build_reuses_provisioning_profile_when_ios_devices_exist() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    if remote_devices_for_platform(&config, "IOS").is_empty() {
        eprintln!(
            "skipping iOS development provisioning reuse live test: no registered iOS devices"
        );
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("IosDevProfileReuse");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(&config, &app_name, &bundle_id, 1, serde_json::json!({})),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build = build_ios_distribution(&workspace, &config, "development", &["--device"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );
    let first_profile_id = ready_profile_id(&first_build)
        .expect("expected first iOS development build to report a provisioning profile id");
    let first_remote_profiles =
        remote_profiles_for_bundle_id(&config, &bundle_id, Some("IOS_APP_DEVELOPMENT"));
    assert_eq!(
        first_remote_profiles.len(),
        1,
        "expected exactly one remote IOS_APP_DEVELOPMENT profile after first build"
    );
    assert_eq!(first_remote_profiles[0].id, first_profile_id);

    let second_build = build_ios_distribution(&workspace, &config, "development", &["--device"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );
    let second_profile_id = ready_profile_id(&second_build)
        .expect("expected second iOS development build to report a provisioning profile id");
    assert_eq!(
        second_profile_id, first_profile_id,
        "expected second iOS development build to reuse the first provisioning profile"
    );

    let second_remote_profiles =
        remote_profiles_for_bundle_id(&config, &bundle_id, Some("IOS_APP_DEVELOPMENT"));
    assert_eq!(
        second_remote_profiles.len(),
        1,
        "expected Orbit to leave only one remote IOS_APP_DEVELOPMENT profile for the bundle id"
    );
    assert_eq!(second_remote_profiles[0].id, first_profile_id);
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_ios_adhoc_build_reuses_provisioning_profile_when_ios_devices_exist() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    if remote_devices_for_platform(&config, "IOS").is_empty() {
        eprintln!("skipping iOS ad-hoc provisioning reuse live test: no registered iOS devices");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("IosAdHocProfileReuse");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(&config, &app_name, &bundle_id, 1, serde_json::json!({})),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build =
        build_ios_distribution(&workspace, &config, "ad-hoc", &["--device", "--release"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );
    let first_profile_id = ready_profile_id(&first_build)
        .expect("expected first iOS ad-hoc build to report a provisioning profile id");
    let first_remote_profiles =
        remote_profiles_for_bundle_id(&config, &bundle_id, Some("IOS_APP_ADHOC"));
    assert_eq!(
        first_remote_profiles.len(),
        1,
        "expected exactly one remote IOS_APP_ADHOC profile after first build"
    );
    assert_eq!(first_remote_profiles[0].id, first_profile_id);

    let second_build =
        build_ios_distribution(&workspace, &config, "ad-hoc", &["--device", "--release"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );
    let second_profile_id = ready_profile_id(&second_build)
        .expect("expected second iOS ad-hoc build to report a provisioning profile id");
    assert_eq!(
        second_profile_id, first_profile_id,
        "expected second iOS ad-hoc build to reuse the first provisioning profile"
    );

    let second_remote_profiles =
        remote_profiles_for_bundle_id(&config, &bundle_id, Some("IOS_APP_ADHOC"));
    assert_eq!(
        second_remote_profiles.len(),
        1,
        "expected Orbit to leave only one remote IOS_APP_ADHOC profile for the bundle id"
    );
    assert_eq!(second_remote_profiles[0].id, first_profile_id);
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_stale_local_certificate_state_recovers_on_next_build() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("StaleCertState");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(&config, &app_name, &bundle_id, 1, serde_json::json!({})),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    let mut signing_state = read_signing_state(&workspace, &config);
    let profiles = signing_state["profiles"]
        .as_array_mut()
        .expect("expected signing state profiles array");
    let profile = profiles
        .iter_mut()
        .find(|profile| profile["bundle_id"].as_str() == Some(bundle_id.as_str()))
        .expect("expected signing state profile for live app-store build");
    let original_certificate_id = profile["certificate_ids"][0]
        .as_str()
        .expect("expected signing state profile to reference a certificate id")
        .to_owned();
    let stale_certificate_id = format!("STALE-{original_certificate_id}");
    let certificates = signing_state["certificates"]
        .as_array_mut()
        .expect("expected signing state certificates array");
    let certificate = certificates
        .iter_mut()
        .find(|certificate| certificate["id"].as_str() == Some(original_certificate_id.as_str()))
        .expect("expected signing state to contain the referenced certificate");
    certificate["id"] = serde_json::Value::String(stale_certificate_id.clone());
    write_signing_state(&workspace, &config, &signing_state);

    let second_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );

    let repaired_state = read_signing_state(&workspace, &config);
    let profile = repaired_state["profiles"]
        .as_array()
        .expect("expected repaired signing state profiles array")
        .iter()
        .find(|profile| profile["bundle_id"].as_str() == Some(bundle_id.as_str()))
        .expect("expected repaired signing state profile for live app-store build");
    assert_eq!(
        profile["certificate_ids"][0].as_str(),
        Some(original_certificate_id.as_str()),
        "expected second build to repair the stale local certificate id"
    );
    let repaired_certificate_ids = repaired_state["certificates"]
        .as_array()
        .expect("expected repaired signing state certificates array")
        .iter()
        .filter_map(|certificate| certificate["id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        repaired_certificate_ids.contains(&original_certificate_id.as_str()),
        "expected repaired signing state to contain the original certificate id again"
    );
    assert!(
        !repaired_certificate_ids.contains(&stale_certificate_id.as_str()),
        "expected second build to remove the stale certificate id from local signing state"
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_missing_local_p12_recovers_on_next_build() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("MissingLocalP12");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(&config, &app_name, &bundle_id, 1, serde_json::json!({})),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    let signing_state = read_signing_state(&workspace, &config);
    let certificate_id = certificate_id_for_bundle(&signing_state, &bundle_id);
    let p12_path = certificate_p12_path_for_id(&signing_state, &certificate_id);
    assert!(
        p12_path.exists(),
        "expected first build to persist a local p12 at {}",
        p12_path.display()
    );
    fs::remove_file(&p12_path).unwrap();
    assert!(
        !p12_path.exists(),
        "expected test setup to remove the local p12 before the second build"
    );

    let second_build = build_ios_distribution(&workspace, &config, "app-store", &["--release"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );

    let repaired_state = read_signing_state(&workspace, &config);
    let repaired_certificate_id = certificate_id_for_bundle(&repaired_state, &bundle_id);
    assert_eq!(
        repaired_certificate_id, certificate_id,
        "expected the second build to recover the existing remote certificate instead of rotating ids"
    );
    let repaired_p12_path = certificate_p12_path_for_id(&repaired_state, &certificate_id);
    assert!(
        repaired_p12_path.exists(),
        "expected the second build to recreate a usable local p12 at {}",
        repaired_p12_path.display()
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_revoked_test_owned_remote_certificate_recovers_on_next_build() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("RevokedRemoteCert");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(&config, &app_name, &bundle_id, 1, serde_json::json!({})),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let certificates_before = remote_certificates_for_type(&config, "DISTRIBUTION")
        .into_iter()
        .map(|certificate| certificate.id)
        .collect::<HashSet<_>>();

    // Start from auth-only state so the test can detect whether this workspace had to
    // create its own cloud-managed certificate instead of reusing a shared team one.
    let first_build =
        build_ios_distribution_without_team_state(&workspace, &config, "app-store", &["--release"]);
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    let signing_state = read_signing_state(&workspace, &config);
    let certificate_id = certificate_id_for_bundle(&signing_state, &bundle_id);
    let certificates_after = remote_certificates_for_type(&config, "DISTRIBUTION")
        .into_iter()
        .map(|certificate| certificate.id)
        .collect::<HashSet<_>>();
    if !certificates_after.contains(&certificate_id)
        || certificates_before.contains(&certificate_id)
    {
        eprintln!(
            "skipping revoked remote certificate recovery live test: build reused an existing team distribution certificate"
        );
        return;
    }

    let app = AppContext::new(true, false, None).unwrap();
    let mut provisioning = ProvisioningClient::authenticate(&app, config.team_id.clone()).unwrap();
    for profile in remote_profiles_for_bundle_id(&config, &bundle_id, Some("IOS_APP_STORE")) {
        provisioning.delete_profile(&profile.id).unwrap();
    }
    provisioning.delete_certificate(&certificate_id).unwrap();

    let second_build =
        build_ios_distribution_without_team_state(&workspace, &config, "app-store", &["--release"]);
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );

    let repaired_state = read_signing_state(&workspace, &config);
    let repaired_certificate_id = certificate_id_for_bundle(&repaired_state, &bundle_id);
    assert_ne!(
        repaired_certificate_id, certificate_id,
        "expected the second build to replace the revoked remote certificate id"
    );
    let repaired_certificate_ids = remote_certificates_for_type(&config, "DISTRIBUTION")
        .into_iter()
        .map(|certificate| certificate.id)
        .collect::<HashSet<_>>();
    assert!(
        !repaired_certificate_ids.contains(&certificate_id),
        "expected the revoked remote certificate to stay deleted after recovery"
    );
    assert!(
        repaired_certificate_ids.contains(&repaired_certificate_id),
        "expected the second build to bind the app to a valid remote distribution certificate"
    );
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_tvos_app_store_build_signs_target() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("TvOsStore");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "tvos": "18.0"
            },
            "sources": ["Sources/App"]
        }),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let build = build_distribution_with_command(
        live_command(&workspace, &config),
        &workspace,
        "tvos",
        "app-store",
        &["--release"],
    );
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    assert_single_remote_profile(&config, &bundle_id, "TVOS_APP_STORE");
}

#[test]
#[ignore = "manual live Apple account test"]
fn live_visionos_app_store_build_signs_target() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("VisionOsStore");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "visionos": "2.0"
            },
            "sources": ["Sources/App"]
        }),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let build = build_distribution_with_command(
        live_command(&workspace, &config),
        &workspace,
        "visionos",
        "app-store",
        &["--release"],
    );
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    assert_single_remote_profile(&config, &bundle_id, "IOS_APP_STORE");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual live Apple account test"]
fn live_macos_developer_id_installer_signing_recovers_missing_local_p12() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("MacInstallerP12");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "macos": "15.0"
            },
            "sources": ["Sources/App"]
        }),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let first_build = build_distribution_with_command(
        live_command(&workspace, &config),
        &workspace,
        "macos",
        "developer-id",
        &["--release"],
    );
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    let first_pkg = latest_receipt_path(&workspace);
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&first_pkg).unwrap()).unwrap();
    let artifact_path = PathBuf::from(
        receipt["artifact_path"]
            .as_str()
            .expect("expected developer-id build receipt to contain artifact_path"),
    );
    assert_eq!(
        artifact_path.extension().and_then(|value| value.to_str()),
        Some("pkg"),
        "expected developer-id build to export a signed installer package"
    );
    assert!(
        artifact_path.exists(),
        "missing pkg artifact at {}",
        artifact_path.display()
    );

    let signing_state = read_signing_state(&workspace, &config);
    let installer_certificate_id = certificate_id_for_type(&signing_state, "OYVN2GW35E");
    let installer_p12_path = certificate_p12_path_for_id(&signing_state, &installer_certificate_id);
    assert!(installer_p12_path.exists());
    fs::remove_file(&installer_p12_path).unwrap();
    assert!(!installer_p12_path.exists());

    let mut manifest =
        serde_json::from_slice::<Value>(&fs::read(workspace.join("orbit.json")).unwrap()).unwrap();
    manifest["build"] = serde_json::json!(2);
    write_manifest(&workspace, &manifest);

    let second_build = build_distribution_with_command(
        live_command(&workspace, &config),
        &workspace,
        "macos",
        "developer-id",
        &["--release"],
    );
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );

    let repaired_state = read_signing_state(&workspace, &config);
    let repaired_installer_certificate_id = certificate_id_for_type(&repaired_state, "OYVN2GW35E");
    assert_eq!(
        repaired_installer_certificate_id, installer_certificate_id,
        "expected installer signing recovery to preserve the remote installer certificate id"
    );
    let repaired_p12_path =
        certificate_p12_path_for_id(&repaired_state, &repaired_installer_certificate_id);
    assert!(
        repaired_p12_path.exists(),
        "expected developer-id rebuild to recreate the installer signing p12"
    );
}

#[test]
#[ignore = "manual live ASC API key test"]
fn live_asc_ios_app_store_build_signs_target() {
    let config = require_live_asc_config("ORBIT_RUN_LIVE_ASC_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.apple.unique_app_identity("AscIosStore");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(
            &config.apple,
            &app_name,
            &bundle_id,
            1,
            serde_json::json!({}),
        ),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config.apple);

    let build = build_distribution_with_command(
        live_asc_command(&workspace, &config),
        &workspace,
        "ios",
        "app-store",
        &["--release"],
    );
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let signing_state = read_signing_state(&workspace, &config.apple);
    let profile_id = profile_id_for_bundle(&signing_state, &bundle_id);
    assert!(
        !profile_id.is_empty(),
        "expected ASC build to persist a provisioning profile id"
    );
}

#[test]
#[ignore = "manual live ASC API key test"]
fn live_asc_distribution_certificate_rotation_recovers_after_remote_delete() {
    let config = require_live_asc_config("ORBIT_RUN_LIVE_ASC_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.apple.unique_app_identity("AscCertRotate");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &ios_manifest(
            &config.apple,
            &app_name,
            &bundle_id,
            1,
            serde_json::json!({}),
        ),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config.apple);

    let certificates_before = remote_asc_certificates_for_type(&config, "IOS_DISTRIBUTION")
        .into_iter()
        .map(|certificate| certificate.id)
        .collect::<HashSet<_>>();

    let first_build = build_distribution_with_command(
        live_asc_command(&workspace, &config),
        &workspace,
        "ios",
        "app-store",
        &["--release"],
    );
    assert!(
        first_build.status.success(),
        "{}",
        String::from_utf8_lossy(&first_build.stderr)
    );

    let signing_state = read_signing_state(&workspace, &config.apple);
    let certificate_id = certificate_id_for_bundle(&signing_state, &bundle_id);
    let profile_id = profile_id_for_bundle(&signing_state, &bundle_id);
    assert!(
        !certificates_before.contains(&certificate_id),
        "expected fresh ASC live build to create a test-owned IOS_DISTRIBUTION certificate"
    );

    let client = asc_client(&config);
    client.delete_profile(&profile_id).unwrap();
    client.delete_certificate(&certificate_id).unwrap();

    let mut manifest =
        serde_json::from_slice::<Value>(&fs::read(workspace.join("orbit.json")).unwrap()).unwrap();
    manifest["build"] = serde_json::json!(2);
    write_manifest(&workspace, &manifest);

    let second_build = build_distribution_with_command(
        live_asc_command(&workspace, &config),
        &workspace,
        "ios",
        "app-store",
        &["--release"],
    );
    assert!(
        second_build.status.success(),
        "{}",
        String::from_utf8_lossy(&second_build.stderr)
    );

    let repaired_state = read_signing_state(&workspace, &config.apple);
    let repaired_certificate_id = certificate_id_for_bundle(&repaired_state, &bundle_id);
    assert_ne!(
        repaired_certificate_id, certificate_id,
        "expected second ASC build to rotate to a new distribution certificate after remote deletion"
    );

    let remote_certificate_ids = remote_asc_certificates_for_type(&config, "IOS_DISTRIBUTION")
        .into_iter()
        .map(|certificate| certificate.id)
        .collect::<HashSet<_>>();
    assert!(
        !remote_certificate_ids.contains(&certificate_id),
        "expected deleted ASC distribution certificate to remain absent after recovery"
    );
    assert!(
        remote_certificate_ids.contains(&repaired_certificate_id),
        "expected recovered ASC build to publish a replacement distribution certificate"
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual live Apple account test"]
fn live_macos_development_build_reuses_provisioning_profile() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("MacProfileReuse");
    let mut manifest = serde_json::json!({
        "$schema": config.schema_path,
        "name": app_name,
        "bundle_id": bundle_id,
        "version": "1.0.0",
        "build": 1,
        "team_id": config.team_id,
        "platforms": {
            "macos": "15.0"
        },
        "sources": ["Sources/App"]
    });
    if let Some(provider_id) = &config.provider_id {
        manifest["provider_id"] = serde_json::Value::String(provider_id.clone());
    }
    let workspace = create_live_workspace_with_manifest(temp.path(), &app_name, &manifest);
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);
    let manifest_path = workspace.join("orbit.json");
    let manifest_path = manifest_path.to_str().unwrap();

    let build_args = [
        "--non-interactive",
        "--manifest",
        manifest_path,
        "build",
        "--platform",
        "macos",
        "--distribution",
        "development",
    ];

    let mut first_build = live_command(&workspace, &config);
    first_build.args(build_args);
    let first_output = run_and_capture(&mut first_build);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    let first_profile_id = ready_profile_id(&first_output)
        .expect("expected first macOS development build to report a provisioning profile id");
    let first_remote_profiles =
        remote_profiles_for_bundle_id(&config, &bundle_id, Some("MAC_APP_DEVELOPMENT"));
    assert_eq!(
        first_remote_profiles.len(),
        1,
        "expected exactly one remote MAC_APP_DEVELOPMENT profile after first build"
    );
    assert_eq!(first_remote_profiles[0].id, first_profile_id);

    let mut second_build = live_command(&workspace, &config);
    second_build.args(build_args);
    let second_output = run_and_capture(&mut second_build);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );
    let second_profile_id = ready_profile_id(&second_output)
        .expect("expected second macOS development build to report a provisioning profile id");
    assert_eq!(
        second_profile_id, first_profile_id,
        "expected second macOS development build to reuse the first provisioning profile"
    );

    let second_remote_profiles =
        remote_profiles_for_bundle_id(&config, &bundle_id, Some("MAC_APP_DEVELOPMENT"));
    assert_eq!(
        second_remote_profiles.len(),
        1,
        "expected Orbit to leave only one remote MAC_APP_DEVELOPMENT profile for the bundle id"
    );
    assert_eq!(second_remote_profiles[0].id, first_profile_id);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual live Apple account test"]
fn live_macos_developer_id_build_and_submit() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let temp = tempfile::tempdir().unwrap();
    let (app_name, bundle_id) = config.unique_app_identity("MacDeveloperId");
    let workspace = create_live_workspace_with_manifest(
        temp.path(),
        &app_name,
        &serde_json::json!({
            "$schema": config.schema_path,
            "name": app_name,
            "bundle_id": bundle_id,
            "version": "1.0.0",
            "build": 1,
            "team_id": config.team_id,
            "provider_id": config.provider_id,
            "platforms": {
                "macos": "15.0"
            },
            "sources": ["Sources/App"]
        }),
    );
    let _cleanup = LiveCleanupGuard::remote_and_local(&workspace, &config);

    let mut build = live_command(&workspace, &config);
    build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "macos",
        "--distribution",
        "developer-id",
        "--release",
    ]);
    let build_output = run_and_capture(&mut build);
    assert!(
        build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let receipt_path = latest_receipt_path(&workspace);
    let mut submit = live_command(&workspace, &config);
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "submit",
        "--receipt",
        receipt_path.to_str().unwrap(),
    ]);
    let submit_output = run_and_capture(&mut submit);
    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );
}
