use std::fs;
use std::path::Path;

use crate::support::{
    base_command, create_api_key, create_build_xcrun_mock, create_codesign_mock, create_ditto_mock,
    create_home, create_lldb_attach_mock, create_passthrough_mock, create_security_mock,
    create_signing_workspace, create_sw_vers_mock, create_watch_workspace, create_watch_xcrun_mock,
    create_xcodebuild_mock, run_and_capture, spawn_asc_mock, write_executable,
};
use serde_json::Value as JsonValue;

#[test]
fn signed_build_runs_before_build_and_after_sign_hooks() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    let security_db = temp.path().join("security-db.txt");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);
    create_security_mock(&mock_bin, &security_db);
    create_codesign_mock(&mock_bin);
    create_ditto_mock(&mock_bin);
    fs::create_dir_all(workspace.join("scripts")).unwrap();
    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    create_api_key(&api_key_path);
    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbit.fixture",
        "ExampleApp",
        false,
    );

    let hook_trace = workspace.join(".hook-trace");
    write_executable(
        &workspace.join("scripts/before-build.sh"),
        &format!(
            "#!/bin/sh\nset -eu\nprintf 'before_build:%s:%s:%s\\n' \"$ORBIT_HOOK\" \"$ORBIT_TARGET_NAME\" \"$ORBIT_PLATFORM\" >> \"{}\"\n",
            hook_trace.display()
        ),
    );
    write_executable(
        &workspace.join("scripts/after-sign.sh"),
        &format!(
            "#!/bin/sh\nset -eu\nprintf 'after_sign:%s:%s:%s:%s\\n' \"$ORBIT_HOOK\" \"$ORBIT_TARGET_NAME\" \"$ORBIT_ARTIFACT_PATH\" \"$ORBIT_RECEIPT_PATH\" >> \"{}\"\n",
            hook_trace.display()
        ),
    );
    set_manifest_hooks(
        &workspace.join("orbit.json"),
        serde_json::json!({
            "before_build": ["./scripts/before-build.sh"],
            "after_sign": ["./scripts/after-sign.sh"]
        }),
    );

    let mut build = base_command(&workspace, &home, &mock_bin, &log_path);
    build.env("ORBIT_ASC_BASE_URL", &server.base_url);
    build.env("ORBIT_ASC_API_KEY_PATH", &api_key_path);
    build.env("ORBIT_ASC_KEY_ID", "KEY1234567");
    build.env(
        "ORBIT_ASC_ISSUER_ID",
        "00000000-0000-0000-0000-000000000000",
    );
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
    let output = run_and_capture(&mut build);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.shutdown();

    let trace = fs::read_to_string(&hook_trace).unwrap();
    let lines = trace.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "before_build:before_build:ExampleApp:ios");
    assert!(lines[1].starts_with("after_sign:after_sign:ExampleApp:"));
    assert!(lines[1].contains(".ipa:"));
    assert!(lines[1].contains(".orbit/receipts/"));
}

#[test]
fn run_executes_before_run_hook_after_build_context_is_available() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_watch_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    let developer_dir = temp.path().join("developer-dir");
    fs::create_dir_all(&mock_bin).unwrap();

    create_watch_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);
    create_lldb_attach_mock(&developer_dir);
    create_passthrough_mock(&mock_bin, "open");
    fs::create_dir_all(workspace.join("scripts")).unwrap();

    let hook_trace = workspace.join(".hook-trace");
    write_executable(
        &workspace.join("scripts/before-build.sh"),
        &format!(
            "#!/bin/sh\nset -eu\nprintf 'before_build:%s:%s\\n' \"$ORBIT_HOOK\" \"$ORBIT_PLATFORM\" >> \"{}\"\n",
            hook_trace.display()
        ),
    );
    write_executable(
        &workspace.join("scripts/before-run.sh"),
        &format!(
            "#!/bin/sh\nset -eu\nprintf 'before_run:%s:%s:%s:%s\\n' \"$ORBIT_HOOK\" \"$ORBIT_TARGET_NAME\" \"$ORBIT_DESTINATION\" \"$ORBIT_ARTIFACT_PATH\" >> \"{}\"\n",
            hook_trace.display()
        ),
    );
    set_manifest_hooks(
        &workspace.join("orbit.json"),
        serde_json::json!({
            "before_build": ["./scripts/before-build.sh"],
            "before_run": ["./scripts/before-run.sh"]
        }),
    );

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.env("DEVELOPER_DIR", &developer_dir);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "run",
        "--platform",
        "watchos",
        "--simulator",
        "--debug",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let trace = fs::read_to_string(&hook_trace).unwrap();
    let lines = trace.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "before_build:before_build:watchos");
    assert!(lines[1].starts_with("before_run:before_run:WatchApp:simulator:"));
    assert!(lines[1].contains(".app"));
}

fn set_manifest_hooks(path: &Path, hooks: JsonValue) {
    let mut manifest: JsonValue = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    manifest["hooks"] = hooks;
    fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}
