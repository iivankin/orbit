mod support;

use std::fs;

#[cfg(target_os = "macos")]
use support::notary_mock::{spawn_notary_mock, write_xcode_notary_auth_fixture};
use support::submit_mock::spawn_submit_mock;
#[cfg(target_os = "macos")]
use support::write_executable;
use support::{
    base_command, create_api_key, create_build_xcrun_mock, create_home, create_signing_workspace,
    create_submit_swinfo_mock, latest_receipt_path, read_log, run_and_capture, spawn_asc_mock,
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
    create_submit_swinfo_mock(&mock_bin);

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
    let receipt_dir = workspace.join(".orbit/receipts");
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
            "bundle_id": "dev.orbit.fixture",
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
        "dev.orbit.fixture",
        "ExampleApp",
        true,
    );
    let submit_server = spawn_submit_mock(temp.path(), "dev.orbit.fixture");
    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ORBIT_ASC_BASE_URL", &server.base_url);
    submit.env("ORBIT_CONTENT_DELIVERY_BASE_URL", &submit_server.base_url);
    submit.env("ORBIT_ASC_API_KEY_PATH", &api_key_path);
    submit.env("ORBIT_ASC_KEY_ID", "KEY1234567");
    submit.env(
        "ORBIT_ASC_ISSUER_ID",
        "00000000-0000-0000-0000-000000000000",
    );
    submit.env("ORBIT_APPLE_TEAM_ID", "TEAM123456");
    submit.env(
        "ORBIT_TRANSPORTER_SWINFO_PATH",
        mock_bin.join("swinfo").to_str().unwrap(),
    );
    submit.env("ORBIT_TRANSPORTER_USE_SWINFO_ASSET_DESCRIPTION", "1");
    submit.env("ORBIT_SUBMIT_HOST_OS_IDENTIFIER", "Mac OS X 26.0.0 (arm64)");
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "submit",
        "--receipt",
        latest_receipt_path(&workspace).to_str().unwrap(),
    ]);
    let submit_output = run_and_capture(&mut submit);
    let requests = server.requests();
    let submit_requests = submit_server.requests();
    server.shutdown();
    submit_server.shutdown();

    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );
    let log = read_log(&log_path);
    assert!(log.contains("swinfo -f"));
    assert!(!log.contains("altool"));
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
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /v1/apps"))
    );
    assert!(submit_requests.iter().any(|request| {
        request.starts_with("POST /WebObjects/MZLabelService.woa/json/MZITunesSoftwareService")
    }));
    assert!(submit_requests.iter().any(|request| {
        request.starts_with("POST /MZContentDeliveryService/iris/provider/provider-test/v1/builds")
    }));
    assert!(submit_requests.iter().any(|request| request.starts_with(
        "POST /MZContentDeliveryService/iris/provider/provider-test/v1/metricsAndLogging"
    )));
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
        workspace.join("orbit.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": "https://orbit.dev/schemas/apple-app.v1.json",
            "name": "ExampleMacApp",
            "bundle_id": "dev.orbit.fixture.mac",
            "version": "0.1.0",
            "build": 1,
            "team_id": "TEAM123456",
            "platforms": {
                "macos": "15.0"
            },
            "sources": ["Sources/App"]
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
echo "unexpected xcrun command: $@" >&2
exit 1
"#,
    );
    write_executable(
        &mock_bin.join("pkgutil"),
        r#"#!/bin/sh
set -eu
echo "pkgutil $@" >> "$MOCK_LOG"
printf 'Package "%s":\n' "$2"
printf '   Status: signed by a developer certificate issued by Apple for distribution\n'
printf '   1. Developer ID Installer: Example Team\n'
printf '   2. Developer ID Certification Authority\n'
printf '   3. Apple Root CA\n'
"#,
    );
    write_executable(
        &mock_bin.join("spctl"),
        r#"#!/bin/sh
set -eu
echo "spctl $@" >> "$MOCK_LOG"
printf '%s: accepted\n' "$5"
printf 'source=Notarized Developer ID\n'
"#,
    );
    write_executable(
        &mock_bin.join("codesign"),
        r#"#!/bin/sh
set -eu
echo "codesign $@" >> "$MOCK_LOG"
printf 'Executable=%s/Contents/MacOS/ExampleMacApp\n' "$3" >&2
printf 'Authority=Developer ID Application: Example Team\n' >&2
printf 'flags=0x10000(runtime)\n' >&2
"#,
    );

    let bundle_path = workspace.join("ExampleMacApp.app");
    fs::create_dir_all(&bundle_path).unwrap();
    let artifact_path = workspace.join("ExampleMacApp-DeveloperId.pkg");
    fs::write(&artifact_path, b"developer-id-pkg").unwrap();
    let receipt_dir = workspace.join(".orbit/receipts");
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
            "bundle_id": "dev.orbit.fixture.mac",
            "bundle_path": bundle_path,
            "artifact_path": artifact_path,
            "created_at_unix": 1,
            "submit_eligible": true
        }))
        .unwrap(),
    )
    .unwrap();

    let notary_server = spawn_notary_mock();
    let auth_fixture = write_xcode_notary_auth_fixture(temp.path(), "TEAM123456");
    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ORBIT_NOTARY_BASE_URL", &notary_server.base_url);
    submit.env("ORBIT_NOTARY_UPLOAD_BASE_URL", &notary_server.base_url);
    submit.env("ORBIT_XCODE_NOTARY_AUTH_PATH", &auth_fixture);
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "submit",
        "--receipt",
        receipt_path.to_str().unwrap(),
        "--wait",
    ]);
    let submit_output = run_and_capture(&mut submit);
    let requests = notary_server.requests();
    notary_server.shutdown();

    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );
    let log = read_log(&log_path);
    assert!(!log.contains("notarytool"));
    assert!(log.contains("xcrun stapler staple"));
    assert!(log.contains("xcrun stapler validate"));
    assert!(log.contains("pkgutil --check-signature"));
    assert!(log.contains("spctl -a -vvv --type install"));
    assert!(log.contains("codesign -dv --verbose=4"));
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /ci/auth/auth/authkit"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /notary/v2/submissions"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("PUT /uploads/submission-1.pkg"))
    );
    assert!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /notary/v2/submissions/submission-1"))
            .count()
            >= 2
    );
}
