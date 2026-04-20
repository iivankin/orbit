use std::fs;

use crate::support::{
    base_command, create_home, create_lldb_attach_mock, create_passthrough_mock,
    create_watch_workspace, create_watch_xcrun_mock, create_xcodebuild_mock, read_log,
    run_and_capture,
};

#[test]
fn watchos_run_debug_uses_simctl_and_lldb() {
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
    create_lldb_attach_mock(&developer_dir);
    create_passthrough_mock(&mock_bin, "open");

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.env("DEVELOPER_DIR", &developer_dir);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
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

    let log = read_log(&log_path);
    assert!(log.contains("xcrun simctl install WATCH-UDID"));
    assert!(log.contains(
        "xcrun simctl launch --wait-for-debugger --terminate-running-process WATCH-UDID dev.orbi.fixture.watch.watchkitapp"
    ));
    assert!(log.lines().any(|line| line.starts_with("lldb ")));
    assert!(log.contains("process attach -i -w -n WatchApp"));
    assert!(log.contains("process continue"));
}

#[test]
fn run_rejects_trace_with_debug() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_watch_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_watch_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_passthrough_mock(&mock_bin, "open");

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "run",
        "--platform",
        "watchos",
        "--simulator",
        "--debug",
        "--trace",
    ]);
    let output = run_and_capture(&mut command);
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("`orbi run --trace` does not support `--debug`"));
}

#[test]
fn run_rejects_trace_on_simulator() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_watch_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_watch_xcrun_mock(&mock_bin, &sdk_root);
    create_xcodebuild_mock(&mock_bin);
    create_passthrough_mock(&mock_bin, "open");

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "run",
        "--platform",
        "watchos",
        "--simulator",
        "--trace",
    ]);
    let output = run_and_capture(&mut command);
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("simulator profiling is currently unavailable"));
    assert!(stderr.contains("unstable"));

    let log = read_log(&log_path);
    assert!(!log.contains("xcrun simctl install"));
    assert!(!log.contains("xcrun xctrace record"));
}
