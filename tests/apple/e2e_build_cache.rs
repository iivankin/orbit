use std::fs;
use std::time::Duration;

use crate::support::{
    base_command, clear_log, create_api_key, create_asset_resource_workspace,
    create_build_xcrun_mock, create_codesign_mock, create_ditto_mock, create_home,
    create_macos_universal_workspace, create_mixed_language_workspace, create_resource_workspace,
    create_security_mock, create_signing_workspace, create_sw_vers_mock, create_watch_workspace,
    create_watch_xcrun_mock, create_xcodebuild_mock, read_log, run_and_capture, spawn_asc_mock,
};

fn build_command(
    workspace: &std::path::Path,
    home: &std::path::Path,
    mock_bin: &std::path::Path,
    log_path: &std::path::Path,
) -> std::process::Command {
    let mut command = base_command(workspace, home, mock_bin, log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "ios",
        "--simulator",
    ]);
    command
}

fn distribution_build_command(
    workspace: &std::path::Path,
    home: &std::path::Path,
    mock_bin: &std::path::Path,
    log_path: &std::path::Path,
    base_url: &str,
    api_key_path: &std::path::Path,
) -> std::process::Command {
    let mut command = base_command(workspace, home, mock_bin, log_path);
    command.env("ORBIT_ASC_BASE_URL", base_url);
    command.env("ORBIT_ASC_API_KEY_PATH", api_key_path);
    command.env("ORBIT_ASC_KEY_ID", "KEY1234567");
    command.env(
        "ORBIT_ASC_ISSUER_ID",
        "00000000-0000-0000-0000-000000000000",
    );
    command.args([
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
    command
}

fn macos_build_command(
    workspace: &std::path::Path,
    home: &std::path::Path,
    mock_bin: &std::path::Path,
    log_path: &std::path::Path,
    base_url: &str,
    api_key_path: &std::path::Path,
) -> std::process::Command {
    let mut command = base_command(workspace, home, mock_bin, log_path);
    command.env("ORBIT_ASC_BASE_URL", base_url);
    command.env("ORBIT_ASC_API_KEY_PATH", api_key_path);
    command.env("ORBIT_ASC_KEY_ID", "KEY1234567");
    command.env(
        "ORBIT_ASC_ISSUER_ID",
        "00000000-0000-0000-0000-000000000000",
    );
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "build",
        "--platform",
        "macos",
    ]);
    command
}

#[test]
fn build_reuses_cached_target_outputs_between_runs() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_mixed_language_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);

    let mut first = build_command(&workspace, &home, &mock_bin, &log_path);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let first_log = read_log(&log_path);
    assert!(first_log.contains("xcrun --sdk iphonesimulator clang"));
    assert!(first_log.contains("xcrun --sdk iphonesimulator swiftc"));

    clear_log(&log_path);

    let mut second = build_command(&workspace, &home, &mock_bin, &log_path);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("xcrun --sdk iphonesimulator clang"));
    assert!(!second_log.contains("xcrun --sdk iphonesimulator swiftc"));
}

#[test]
fn build_cache_invalidates_when_header_changes() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_mixed_language_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);

    let mut first = build_command(&workspace, &home, &mock_bin, &log_path);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    clear_log(&log_path);
    fs::write(
        workspace.join("Sources/App/Bridge.h"),
        "int orbit_add(int a, int b);\nint orbit_subtract(int a, int b);\n",
    )
    .unwrap();

    let mut second = build_command(&workspace, &home, &mock_bin, &log_path);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_log = read_log(&log_path);
    assert!(second_log.contains("xcrun --sdk iphonesimulator clang"));
    assert!(second_log.contains("xcrun --sdk iphonesimulator swiftc"));
}

#[test]
fn swift_only_changes_reuse_cached_clang_objects() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_mixed_language_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);

    let mut first = build_command(&workspace, &home, &mock_bin, &log_path);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    clear_log(&log_path);
    fs::write(
        workspace.join("Sources/App/App.swift"),
        "import SwiftUI\n@main struct ExampleApp: App { var body: some Scene { WindowGroup { Text(\"Updated\") } } }\n",
    )
    .unwrap();

    let mut second = build_command(&workspace, &home, &mock_bin, &log_path);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("xcrun --sdk iphonesimulator clang"));
    assert!(second_log.contains("xcrun --sdk iphonesimulator swiftc"));
}

#[test]
fn resource_only_changes_reuse_code_cache_and_refresh_bundle_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_resource_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);

    let mut first = build_command(&workspace, &home, &mock_bin, &log_path);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    clear_log(&log_path);
    fs::write(
        workspace.join("Resources/config.json"),
        "{\n  \"value\": 2\n}\n",
    )
    .unwrap();

    let mut second = build_command(&workspace, &home, &mock_bin, &log_path);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("xcrun --sdk iphonesimulator clang"));
    assert!(!second_log.contains("xcrun --sdk iphonesimulator swiftc"));

    let built_resource = workspace
        .join(".orbit/build/ios/development-debug/simulator/ExampleApp/ExampleApp.app/config.json");
    assert_eq!(
        fs::read_to_string(built_resource).unwrap(),
        "{\n  \"value\": 2\n}\n"
    );
}

#[test]
fn copy_only_resource_changes_reuse_cached_actool_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_asset_resource_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);

    let mut first = build_command(&workspace, &home, &mock_bin, &log_path);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let first_log = read_log(&log_path);
    assert!(first_log.contains("xcrun actool"));

    clear_log(&log_path);
    fs::write(
        workspace.join("Resources/config.json"),
        "{\n  \"value\": 2\n}\n",
    )
    .unwrap();

    let mut second = build_command(&workspace, &home, &mock_bin, &log_path);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("xcrun actool"));
    assert_eq!(
        fs::read_to_string(workspace.join(
            ".orbit/build/ios/development-debug/simulator/ExampleApp/ExampleApp.app/config.json"
        ),)
        .unwrap(),
        "{\n  \"value\": 2\n}\n"
    );
    assert!(
        workspace
            .join(
                ".orbit/build/ios/development-debug/simulator/ExampleApp/ExampleApp.app/Assets.car"
            )
            .exists()
    );
}

#[test]
fn watch_dependency_copy_outputs_are_reused_between_no_op_builds() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_watch_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_watch_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);

    let mut first = build_command(&workspace, &home, &mock_bin, &log_path);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let embedded_watch_binary = workspace.join(
        ".orbit/build/ios/development-debug/simulator/WatchFixture/WatchFixture.app/Watch/WatchApp.app/WatchApp",
    );
    let first_modified = fs::metadata(&embedded_watch_binary)
        .unwrap()
        .modified()
        .unwrap();

    std::thread::sleep(Duration::from_millis(1100));
    clear_log(&log_path);

    let mut second = build_command(&workspace, &home, &mock_bin, &log_path);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_modified = fs::metadata(&embedded_watch_binary)
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(first_modified, second_modified);
}

#[test]
fn distribution_build_reuses_signing_and_export_outputs_between_runs() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    let security_db = temp.path().join("security-db.txt");
    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_security_mock(&mock_bin, &security_db);
    create_codesign_mock(&mock_bin);
    create_ditto_mock(&mock_bin);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);
    create_api_key(&api_key_path);

    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbit.fixture",
        "ExampleApp",
        false,
    );

    let mut first = distribution_build_command(
        &workspace,
        &home,
        &mock_bin,
        &log_path,
        &server.base_url,
        &api_key_path,
    );
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let first_log = read_log(&log_path);
    assert!(first_log.contains("xcrun --sdk iphoneos swiftc"));
    assert!(first_log.contains("codesign --force --sign"));
    assert!(first_log.contains("ditto -c -k --keepParent"));

    clear_log(&log_path);

    let mut second = distribution_build_command(
        &workspace,
        &home,
        &mock_bin,
        &log_path,
        &server.base_url,
        &api_key_path,
    );
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    server.shutdown();

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("xcrun --sdk iphoneos swiftc"));
    assert!(!second_log.contains("codesign --force --sign"));
    assert!(!second_log.contains("ditto -c -k --keepParent"));
}

#[test]
fn universal_macos_build_reuses_cached_lipo_merge_between_runs() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_macos_universal_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    let security_db = temp.path().join("security-db.txt");
    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_security_mock(&mock_bin, &security_db);
    create_codesign_mock(&mock_bin);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);
    create_api_key(&api_key_path);

    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbit.fixture.macos-universal",
        "ExampleMacApp",
        false,
    );

    let mut first = macos_build_command(
        &workspace,
        &home,
        &mock_bin,
        &log_path,
        &server.base_url,
        &api_key_path,
    );
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let first_log = read_log(&log_path);
    assert!(first_log.contains("xcrun lipo -create -output"));

    clear_log(&log_path);

    let mut second = macos_build_command(
        &workspace,
        &home,
        &mock_bin,
        &log_path,
        &server.base_url,
        &api_key_path,
    );
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    server.shutdown();

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("xcrun lipo -create -output"));
}
