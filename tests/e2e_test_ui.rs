mod support;

use std::fs;
use std::path::{Path, PathBuf};

use support::{
    base_command, create_build_xcrun_mock, create_home, create_idb_mock,
    create_ui_testing_workspace, read_log, run_and_capture,
};
use tempfile::tempdir;

#[test]
fn orbit_test_runs_ui_flows_for_maestro_manifest_tests() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_idb_mock(&mock_bin);
    let workspace = create_ui_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args(["--non-interactive", "test", "--ui", "--platform", "ios"]);
    let output = run_and_capture(&mut command);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{}",
        format_failure_output(&stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mock log line"));

    let log = read_log(&log_path);
    assert!(log.contains("xcrun simctl list devices available --json"));
    assert!(log.contains("xcrun simctl install IOS-UDID"));
    assert!(log.contains("xcrun simctl launch IOS-UDID dev.orbit.fixture.ui"));
    assert!(log.contains("idb launch -f dev.orbit.fixture.ui -onboardingComplete true -seedUser qa@example.com --udid IOS-UDID"));
    assert!(log.contains("idb clear-keychain --udid IOS-UDID"));
    assert!(log.contains("idb uninstall dev.orbit.fixture.ui --udid IOS-UDID"));
    assert!(log.contains("idb ui describe-all --udid IOS-UDID"));
    assert!(log.contains("idb log --udid IOS-UDID -- --process ExampleApp"));
    assert!(log.contains("idb video"));
    assert!(log.contains("idb ui swipe --duration 0.500 --delta 5 354 426 39 426 --udid IOS-UDID"));
    assert!(log.contains("idb ui tap 140 142 --udid IOS-UDID"));
    assert!(log.contains("idb ui button SIRI --udid IOS-UDID"));
    assert!(log.contains("idb ui text"));
    assert!(log.contains("hello orbit"));
    assert!(log.contains("idb ui key 42 --udid IOS-UDID"));
    assert!(log.contains("idb ui key 40 --udid IOS-UDID"));
    assert!(log.contains("idb ui key --duration 0.200 41 --udid IOS-UDID"));
    assert!(log.contains("idb ui key-sequence 4 5 6 --udid IOS-UDID"));
    assert!(log.contains("idb open https://example.com --udid IOS-UDID"));
    assert!(log.contains("idb set-location 55.7558 37.6173 --udid IOS-UDID"));
    assert!(log.contains("xcrun simctl privacy IOS-UDID grant location dev.orbit.fixture.ui"));
    assert!(log.contains("xcrun simctl privacy IOS-UDID revoke photos dev.orbit.fixture.ui"));
    assert!(log.contains("xcrun simctl privacy IOS-UDID grant microphone dev.orbit.fixture.ui"));
    assert!(log.contains("xcrun simctl privacy IOS-UDID reset reminders dev.orbit.fixture.ui"));
    assert!(log.contains(
        "xcrun simctl location IOS-UDID start --speed=42 55.7558,37.6173 55.7568,37.6183"
    ));
    assert!(log.contains("idb add-media"));
    assert!(log.contains("cat.jpg --udid IOS-UDID"));

    let report_path = latest_ui_report_path(workspace.join(".orbit/tests/ui").as_path());
    let report = fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("orbit-idb-ios-simulator"));
    assert!(report.contains("\"status\": \"passed\""));
    assert!(report.contains("\"video\""));

    let screenshot_path = report_path
        .parent()
        .unwrap()
        .join("artifacts")
        .join("after-login.png");
    assert!(
        screenshot_path.exists(),
        "missing {}",
        screenshot_path.display()
    );
    let video_path = report_path
        .parent()
        .unwrap()
        .join("artifacts")
        .join("login.mp4");
    assert!(video_path.exists(), "missing {}", video_path.display());
    let manual_video_path = report_path
        .parent()
        .unwrap()
        .join("artifacts")
        .join("advanced-clip.mp4");
    assert!(
        manual_video_path.exists(),
        "missing {}",
        manual_video_path.display()
    );
}

#[test]
fn orbit_test_ui_fails_early_with_install_hint_when_idb_is_missing() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    let workspace = create_ui_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.env("PATH", mock_bin.display().to_string());
    command.args(["--non-interactive", "test", "--ui", "--platform", "ios"]);
    let output = run_and_capture(&mut command);
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires `idb` and `idb_companion` on PATH"));
    assert!(stderr.contains("brew install idb-companion"));
    assert!(stderr.contains("python3 -m pip install fb-idb"));

    let log = read_log(&log_path);
    assert!(!log.contains("xcrun "));
}

fn format_failure_output(stderr: &str) -> String {
    let Some(report_path) = stderr.split("see ").nth(1).map(str::trim) else {
        return stderr.to_owned();
    };
    match fs::read_to_string(report_path) {
        Ok(report) => format!("{stderr}\nreport:\n{report}"),
        Err(_) => stderr.to_owned(),
    }
}

fn latest_ui_report_path(root: &Path) -> PathBuf {
    let mut runs = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    runs.sort();
    runs.pop().unwrap().join("report.json")
}

#[test]
fn orbit_ui_aux_commands_forward_to_idb() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_idb_mock(&mock_bin);
    let workspace = create_ui_testing_workspace(temp.path());

    let mut focus = base_command(&workspace, &home, &mock_bin, &log_path);
    focus.args(["--non-interactive", "ui", "focus", "--platform", "ios"]);
    let output = run_and_capture(&mut focus);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut logs = base_command(&workspace, &home, &mock_bin, &log_path);
    logs.args([
        "--non-interactive",
        "ui",
        "logs",
        "--platform",
        "ios",
        "--",
        "--timeout",
        "1s",
    ]);
    let output = run_and_capture(&mut logs);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("mock log line"));

    let mut add_media = base_command(&workspace, &home, &mock_bin, &log_path);
    add_media.args([
        "--non-interactive",
        "ui",
        "add-media",
        "--platform",
        "ios",
        "Tests/Fixtures/cat.jpg",
    ]);
    let output = run_and_capture(&mut add_media);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut open = base_command(&workspace, &home, &mock_bin, &log_path);
    open.args([
        "--non-interactive",
        "ui",
        "open",
        "--platform",
        "ios",
        "https://example.com",
    ]);
    let output = run_and_capture(&mut open);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut dylib = base_command(&workspace, &home, &mock_bin, &log_path);
    dylib.args([
        "--non-interactive",
        "ui",
        "install-dylib",
        "--platform",
        "ios",
        "Tests/Fixtures/TestAgent.dylib",
    ]);
    let output = run_and_capture(&mut dylib);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut contacts = base_command(&workspace, &home, &mock_bin, &log_path);
    contacts.args([
        "--non-interactive",
        "ui",
        "update-contacts",
        "--platform",
        "ios",
        "Tests/Fixtures/contacts.sqlite",
    ]);
    let output = run_and_capture(&mut contacts);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut instruments = base_command(&workspace, &home, &mock_bin, &log_path);
    instruments.args([
        "--non-interactive",
        "ui",
        "instruments",
        "--platform",
        "ios",
        "--template",
        "Time Profiler",
        "--",
        "--operation-duration",
        "5",
        "--output",
        "trace",
    ]);
    let output = run_and_capture(&mut instruments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut crash_list = base_command(&workspace, &home, &mock_bin, &log_path);
    crash_list.args([
        "--non-interactive",
        "ui",
        "crash",
        "--platform",
        "ios",
        "list",
        "--bundle-id",
        "dev.orbit.fixture.ui",
    ]);
    let output = run_and_capture(&mut crash_list);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("mock-crash-1.ips"));

    let mut crash_show = base_command(&workspace, &home, &mock_bin, &log_path);
    crash_show.args([
        "--non-interactive",
        "ui",
        "crash",
        "--platform",
        "ios",
        "show",
        "mock-crash-1.ips",
    ]);
    let output = run_and_capture(&mut crash_show);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("mock crash payload"));

    let mut crash_delete = base_command(&workspace, &home, &mock_bin, &log_path);
    crash_delete.args([
        "--non-interactive",
        "ui",
        "crash",
        "--platform",
        "ios",
        "delete",
        "--all",
        "--since",
        "1710000000",
    ]);
    let output = run_and_capture(&mut crash_delete);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut reset = base_command(&workspace, &home, &mock_bin, &log_path);
    reset.args(["ui", "reset-idb"]);
    let output = run_and_capture(&mut reset);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("idb focus --udid IOS-UDID"));
    assert!(log.contains("idb log --udid IOS-UDID -- --timeout 1s"));
    assert!(log.contains("idb add-media"));
    assert!(log.contains("cat.jpg --udid IOS-UDID"));
    assert!(log.contains("idb open https://example.com --udid IOS-UDID"));
    assert!(log.contains("idb dylib install"));
    assert!(log.contains("TestAgent.dylib --udid IOS-UDID"));
    assert!(log.contains("idb contacts update"));
    assert!(log.contains("contacts.sqlite --udid IOS-UDID"));
    assert!(log.contains(
        "idb instruments --template Time Profiler --operation-duration 5 --output trace --udid IOS-UDID"
    ));
    assert!(log.contains("idb crash list --bundle-id dev.orbit.fixture.ui --udid IOS-UDID"));
    assert!(log.contains("idb crash show mock-crash-1.ips --udid IOS-UDID"));
    assert!(log.contains("idb crash delete --since 1710000000 --all --udid IOS-UDID"));
    assert!(log.contains("idb kill"));
}

#[test]
fn orbit_ui_dump_tree_prints_accessibility_json() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_idb_mock(&mock_bin);
    let workspace = create_ui_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args(["--non-interactive", "ui", "dump-tree", "--platform", "ios"]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"AXLabel\": \"Continue\""));

    let log = read_log(&log_path);
    assert!(log.contains("idb ui describe-all --udid IOS-UDID"));
}

#[test]
fn orbit_ui_describe_point_prints_point_json() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_idb_mock(&mock_bin);
    let workspace = create_ui_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "ui",
        "describe-point",
        "--platform",
        "ios",
        "--x",
        "140",
        "--y",
        "142",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"AXLabel\": \"Continue\""));

    let log = read_log(&log_path);
    assert!(log.contains("idb ui describe-point 140 142 --udid IOS-UDID"));
}
