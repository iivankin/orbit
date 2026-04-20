use std::fs;

use crate::support::{
    base_command, clear_log, create_build_xcrun_mock, create_home, create_mixed_language_workspace,
    create_signing_workspace, create_watch_workspace, read_log, run_and_capture,
};

#[test]
fn ide_dump_args_emits_semantic_swiftc_invocations() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "ide",
        "dump-args",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"platform\": \"ios\""));
    assert!(stdout.contains("\"destination\": \"simulator\""));
    assert!(stdout.contains("\"sdk_name\": \"iphonesimulator\""));
    assert!(stdout.contains("\"target\": \"ExampleApp\""));
    assert!(stdout.contains("\"module_name\": \"ExampleApp\""));
    assert!(stdout.contains("\"target_triple\": \"arm64-apple-ios18.0-simulator\""));
    assert!(stdout.contains("\"swiftc\""));
    assert!(stdout.contains("\"-sdk\""));
    assert!(stdout.contains("Sources/App/App.swift"));

    let log = read_log(&log_path);
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
}

#[test]
fn ide_dump_args_filters_requested_file_and_platform() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_watch_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "ide",
        "dump-args",
        "--platform",
        "ios",
        "--file",
        "Sources/App/App.swift",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"requested_file\":"));
    assert!(stdout.contains("Sources/App/App.swift"));
    assert!(stdout.contains("\"platforms\": ["));
    assert!(stdout.contains("\"ios\""));
    assert!(!stdout.contains("\"watchos\""));
    assert!(!stdout.contains("Sources/WatchApp/App.swift"));
    assert!(!stdout.contains("WatchFixtureWatchApp"));

    let log = read_log(&log_path);
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
    assert!(!log.contains("xcrun --sdk watchsimulator --show-sdk-path"));
}

#[test]
fn ide_dump_args_includes_c_family_invocations_for_mixed_targets() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_mixed_language_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "ide",
        "dump-args",
        "--file",
        "Sources/App/Bridge.m",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"language\": \"objective-c\""));
    assert!(stdout.contains("\"clang\""));
    assert!(stdout.contains("\"output_path\":"));
    assert!(stdout.contains("Bridge.m.o"));
    assert!(stdout.contains("Sources/App/Bridge.m"));

    let log = read_log(&log_path);
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
    assert!(log.contains("xcrun --find swiftc"));
}

#[test]
fn ide_dump_args_reuses_cached_semantic_artifact_between_runs() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let mut first = base_command(&workspace, &home, &mock_bin, &log_path);
    first.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "ide",
        "dump-args",
    ]);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let first_stderr = String::from_utf8_lossy(&first_output.stderr);
    assert!(first_stderr.contains("Semantic analysis cache miss"));

    let first_log = read_log(&log_path);
    assert!(first_log.contains("xcrun --find swiftc"));

    clear_log(&log_path);

    let mut second = base_command(&workspace, &home, &mock_bin, &log_path);
    second.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "ide",
        "dump-args",
    ]);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_stderr = String::from_utf8_lossy(&second_output.stderr);
    assert!(second_stderr.contains("Semantic analysis cache hit"));

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("xcrun --find swiftc"));
}
