mod support;

use std::fs;
use std::process::Command;

use orbit::apple::developer_services::DeveloperServicesClient;
use orbit::context::AppContext;
use serde_json::Value;

use support::{
    LiveCleanupGuard, create_live_workspace, create_live_workspace_with_manifest,
    latest_receipt_path, live_command, remote_capabilities_for_bundle_id,
    require_live_apple_config, run_and_capture,
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

#[test]
#[ignore = "manual live Apple account test"]
fn live_developer_services_lists_configured_team() {
    let config = require_live_apple_config("ORBIT_RUN_LIVE_APPLE_E2E");
    let app = AppContext::new(true, false).unwrap();
    let mut developer_services = DeveloperServicesClient::authenticate(&app).unwrap();
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
fn live_entitlements_change_updates_remote_capabilities() {
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

    // Xcode does not emit a matching Developer Services disable mutation when
    // users remove Associated Domains in Signing & Capabilities. The important
    // behavior to preserve is that the second build still succeeds after the
    // local entitlement change.
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
