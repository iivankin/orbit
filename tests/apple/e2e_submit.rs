use std::fs;

#[cfg(target_os = "macos")]
use crate::support::write_executable;
#[cfg(target_os = "macos")]
use crate::support::{
    base_command, create_api_key, create_build_xcrun_mock, create_home,
    create_macos_app_store_workspace, create_signing_workspace, latest_receipt_path, read_log,
    run_and_capture, spawn_asc_mock,
};

#[test]
fn submit_uses_existing_receipt_without_rebuilding() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    create_api_key(&api_key_path);
    let artifact_path = workspace.join("ExampleApp.ipa");
    let bundle_path = workspace.join("ExampleApp.app");
    fs::write(&artifact_path, b"ipa").unwrap();
    fs::create_dir_all(&bundle_path).unwrap();
    fs::write(
        bundle_path.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
</dict>
</plist>
"#,
    )
    .unwrap();
    let receipt_dir = workspace.join(".orbi/receipts");
    fs::create_dir_all(&receipt_dir).unwrap();
    let receipt_path = receipt_dir.join("receipt.json");
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": "receipt-1",
            "target": "ExampleApp",
            "platform": "ios",
            "configuration": "release",
            "distribution": "app-store",
            "destination": "device",
            "bundle_id": "dev.orbi.fixture",
            "bundle_path": bundle_path,
            "artifact_path": artifact_path,
            "created_at_unix": 1,
            "submit_eligible": true
        }))
        .unwrap(),
    )
    .unwrap();

    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbi.fixture",
        "ExampleApp",
        true,
        true,
    );
    let api_base_url = format!("{}/v1", server.base_url);
    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ORBI_ASC_BASE_URL", &api_base_url);
    submit.env("ASC_KEY_ID", "KEY1234567");
    submit.env("ASC_ISSUER_ID", "00000000-0000-0000-0000-000000000000");
    submit.env("ASC_PRIVATE_KEY_PATH", &api_key_path);
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "submit",
        "--receipt",
        latest_receipt_path(&workspace).to_str().unwrap(),
    ]);
    let submit_output = run_and_capture(&mut submit);
    let requests = server.requests();
    server.shutdown();

    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );
    let log = read_log(&log_path);
    assert!(log.contains("xcrun altool --upload-app -f"));
    assert!(!log.contains("swiftc"));
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /v1/bundleIds"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /v1/apps"))
    );
}

#[cfg(target_os = "macos")]
#[test]
fn developer_id_submit_uses_xcode_like_notary_flow() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("mac-submit-workspace");
    fs::create_dir_all(workspace.join("Sources/App")).unwrap();
    fs::write(
        workspace.join("Sources/App/App.swift"),
        "import SwiftUI\n@main struct ExampleMacApp: App { var body: some Scene { WindowGroup { Text(\"Mac\") } } }\n",
    )
    .unwrap();
    fs::write(
        workspace.join("orbi.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": "/tmp/.orbi/schemas/apple-app.v1.json",
            "name": "ExampleMacApp",
            "bundle_id": "dev.orbi.fixture.mac",
            "version": "0.1.0",
            "build": 1,
            "platforms": {
                "macos": "15.0"
            },
            "sources": ["Sources/App"],
            "asc": {
                "team_id": "TEAM123456",
                "bundle_ids": {
                    "app": {
                        "bundle_id": "dev.orbi.fixture.mac",
                        "name": "ExampleMacApp",
                        "platform": "mac_os"
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();
    write_executable(
        &mock_bin.join("xcrun"),
        r#"#!/bin/sh
set -eu
echo "xcrun $@" >> "$MOCK_LOG"
if [ "$#" -ge 2 ] && [ "$1" = "stapler" ] && [ "$2" = "staple" ]; then
  exit 0
fi
if [ "$#" -ge 2 ] && [ "$1" = "stapler" ] && [ "$2" = "validate" ]; then
  exit 0
fi
if [ "$#" -ge 2 ] && [ "$1" = "notarytool" ] && [ "$2" = "submit" ]; then
  printf '%s\n' '{"id":"submission-1","status":"Accepted"}'
  exit 0
fi
echo "unexpected xcrun command: $@" >&2
exit 1
"#,
    );
    let bundle_path = workspace.join("ExampleMacApp.app");
    fs::create_dir_all(&bundle_path).unwrap();
    let artifact_path = workspace.join("ExampleMacApp-DeveloperId.dmg");
    fs::write(&artifact_path, b"developer-id-dmg").unwrap();
    let receipt_dir = workspace.join(".orbi/receipts");
    fs::create_dir_all(&receipt_dir).unwrap();
    let receipt_path = receipt_dir.join("developer-id-receipt.json");
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": "receipt-mac-1",
            "target": "ExampleMacApp",
            "platform": "macos",
            "configuration": "release",
            "distribution": "developer-id",
            "destination": "device",
            "bundle_id": "dev.orbi.fixture.mac",
            "bundle_path": bundle_path,
            "artifact_path": artifact_path,
            "created_at_unix": 1,
            "submit_eligible": true
        }))
        .unwrap(),
    )
    .unwrap();

    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    create_api_key(&api_key_path);
    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ASC_KEY_ID", "KEY1234567");
    submit.env("ASC_ISSUER_ID", "00000000-0000-0000-0000-000000000000");
    submit.env("ASC_PRIVATE_KEY_PATH", &api_key_path);
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "submit",
        "--receipt",
        receipt_path.to_str().unwrap(),
        "--wait",
    ]);
    let submit_output = run_and_capture(&mut submit);

    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );
    let log = read_log(&log_path);
    assert!(log.contains("xcrun notarytool submit"));
    assert!(log.contains("xcrun stapler staple"));
}

#[cfg(target_os = "macos")]
#[test]
fn mac_app_store_submit_uploads_app_bundle_from_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_macos_app_store_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    create_api_key(&api_key_path);
    let artifact_path = workspace.join("ExampleMacApp-MacAppStore.app");
    fs::create_dir_all(&artifact_path).unwrap();
    let bundle_path = workspace.join("ExampleMacApp.app");
    fs::create_dir_all(&bundle_path).unwrap();
    let receipt_dir = workspace.join(".orbi/receipts");
    fs::create_dir_all(&receipt_dir).unwrap();
    let receipt_path = receipt_dir.join("mac-app-store-receipt.json");
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": "receipt-mac-store-1",
            "target": "ExampleMacApp",
            "platform": "macos",
            "configuration": "release",
            "distribution": "mac-app-store",
            "destination": "device",
            "bundle_id": "dev.orbi.fixture.macos-store",
            "bundle_path": bundle_path,
            "artifact_path": artifact_path,
            "created_at_unix": 1,
            "submit_eligible": true
        }))
        .unwrap(),
    )
    .unwrap();

    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbi.fixture.macos-store",
        "ExampleMacApp",
        true,
        true,
    );
    let api_base_url = format!("{}/v1", server.base_url);
    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ORBI_ASC_BASE_URL", &api_base_url);
    submit.env("ASC_KEY_ID", "KEY1234567");
    submit.env("ASC_ISSUER_ID", "00000000-0000-0000-0000-000000000000");
    submit.env("ASC_PRIVATE_KEY_PATH", &api_key_path);
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "submit",
        "--receipt",
        receipt_path.to_str().unwrap(),
    ]);
    let submit_output = run_and_capture(&mut submit);
    let requests = server.requests();
    server.shutdown();

    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );
    let log = read_log(&log_path);
    assert!(log.contains("xcrun altool --upload-app -f"));
    assert!(!log.contains("swiftc"));
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /v1/bundleIds"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /v1/apps"))
    );
}
