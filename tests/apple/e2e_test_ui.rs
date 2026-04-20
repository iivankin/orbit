use std::fs;

use serde_json::json;

use crate::support::{
    base_command, create_brew_idb_companion_install_mock, create_build_xcrun_mock,
    create_fake_xcode_bundle, create_home, create_idb_mock, create_python3_fb_idb_install_mock,
    create_runtime_download_xcodebuild_mock, create_runtime_installing_xcrun_mock,
    create_ui_testing_workspace, format_failure_output, latest_ui_report_path, read_log,
    run_and_capture, set_manifest_platforms, set_manifest_xcode,
};
use tempfile::tempdir;

#[test]
fn orbi_test_runs_ui_flows_for_manifest_tests() {
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
    assert!(log.contains("xcrun simctl launch IOS-UDID dev.orbi.fixture.ui"));
    assert!(log.contains("idb launch -f dev.orbi.fixture.ui -onboardingComplete true -seedUser qa@example.com --udid IOS-UDID"));
    assert!(log.contains("idb clear-keychain --udid IOS-UDID"));
    assert!(log.contains("idb uninstall dev.orbi.fixture.ui --udid IOS-UDID"));
    assert!(log.contains("idb ui describe-all --udid IOS-UDID"));
    assert!(log.contains(
        "xcrun simctl spawn IOS-UDID log stream --style compact --color none --level debug --process ExampleApp"
    ));
    assert!(log.contains("idb video"));
    assert!(log.contains("idb ui swipe --duration 0.500 --delta 5 354 426 39 426 --udid IOS-UDID"));
    assert!(log.contains("idb ui tap 140 142 --udid IOS-UDID"));
    assert!(log.contains("idb ui button SIRI --udid IOS-UDID"));
    assert!(log.contains("idb ui text"));
    assert!(log.contains("hello orbi"));
    assert!(log.contains("idb ui key 42 --udid IOS-UDID"));
    assert!(log.contains("idb ui key 40 --udid IOS-UDID"));
    assert!(log.contains("idb ui key --duration 0.200 41 --udid IOS-UDID"));
    assert!(log.contains("idb ui key-sequence 4 5 6 --udid IOS-UDID"));
    assert!(log.contains("idb open https://example.com --udid IOS-UDID"));
    assert!(log.contains("idb set-location 55.7558 37.6173 --udid IOS-UDID"));
    assert!(log.contains("xcrun simctl privacy IOS-UDID grant location dev.orbi.fixture.ui"));
    assert!(log.contains("xcrun simctl privacy IOS-UDID revoke photos dev.orbi.fixture.ui"));
    assert!(log.contains("xcrun simctl privacy IOS-UDID grant microphone dev.orbi.fixture.ui"));
    assert!(log.contains("xcrun simctl privacy IOS-UDID reset reminders dev.orbi.fixture.ui"));
    assert!(log.contains(
        "xcrun simctl location IOS-UDID start --speed=42 55.7558,37.6173 55.7568,37.6183"
    ));
    assert!(log.contains("idb add-media"));
    assert!(log.contains("cat.jpg --udid IOS-UDID"));

    let report_path = latest_ui_report_path(workspace.join(".orbi/tests/ui").as_path());
    let report = fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("orbi-idb-ios-simulator"));
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
fn orbi_ui_init_writes_json_flow_template() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    let workspace = create_ui_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "ui",
        "init",
        "Tests/UI/generated-flow.json",
    ]);
    let output = run_and_capture(&mut command);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{}",
        format_failure_output(&stderr)
    );

    let contents = fs::read_to_string(workspace.join("Tests/UI/generated-flow.json")).unwrap();
    assert!(contents.contains("\"$schema\""));
    assert!(contents.contains("\"appId\": \"dev.orbi.fixture.ui\""));
    assert!(contents.contains("\"name\": \"generated-flow\""));
    assert!(contents.contains("\"steps\""));
    assert!(contents.contains("\"launchApp\""));
}

#[test]
fn orbi_test_ui_trace_fails_on_ios_simulator() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    let workspace = create_ui_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "test",
        "--ui",
        "--platform",
        "ios",
        "--trace",
        "memory",
    ]);
    let output = run_and_capture(&mut command);
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("simulator profiling is currently unavailable"));
    assert!(stderr.contains("xctrace/InstrumentsCLI simulator path is unstable"));

    let log = read_log(&log_path);
    assert!(!log.contains("xcrun simctl install"));
    assert!(!log.contains("idb launch"));
    assert!(!log.contains("xcrun xctrace record"));
}

#[test]
fn orbi_test_ui_trace_advances_past_macos_relaunch_planning() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    let workspace = create_ui_testing_workspace(temp.path());
    set_manifest_platforms(
        workspace.join("orbi.json").as_path(),
        json!({
            "macos": "15.0"
        }),
    );
    fs::write(
        workspace.join("Tests/UI/advanced.json"),
        "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    \"launchApp\",\n    {\n      \"assertVisible\": \"Continue\"\n    }\n  ]\n}\n",
    )
    .unwrap();
    fs::write(
        workspace.join("Tests/UI/login.json"),
        "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    \"launchApp\",\n    {\n      \"assertVisible\": \"Continue\"\n    }\n  ]\n}\n",
    )
    .unwrap();

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "test",
        "--ui",
        "--platform",
        "macos",
        "--trace",
        "cpu",
    ]);
    let output = run_and_capture(&mut command);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        !stderr.contains("only one `launchApp`"),
        "{}",
        format_failure_output(&stderr)
    );

    let report_path = latest_ui_report_path(workspace.join(".orbi/tests/ui").as_path());
    let report = fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("\"command\": \"launchApp\""));
    assert!(report.contains("\"error\": \"could not find `Continue`"));

    let profiles_dir = workspace.join(".orbi/artifacts/profiles");
    let trace_count = fs::read_dir(&profiles_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| extension == "trace")
        })
        .count();
    assert!(
        trace_count >= 2,
        "expected at least 2 trace bundles in {}",
        profiles_dir.display()
    );
}

#[test]
fn orbi_test_ui_discovers_user_site_idb_when_path_is_missing() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_idb_mock(&mock_bin);
    fs::remove_file(mock_bin.join("idb")).unwrap();

    let user_bin = home.join("Library").join("Python").join("3.12").join("bin");
    fs::create_dir_all(&user_bin).unwrap();
    create_idb_mock(&user_bin);

    let workspace = create_ui_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.env("PATH", format!("{}:/usr/bin:/bin", mock_bin.display()));
    command.args(["--non-interactive", "test", "--ui", "--platform", "ios"]);
    let output = run_and_capture(&mut command);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{}",
        format_failure_output(&stderr)
    );

    let log = read_log(&log_path);
    assert!(!log.contains("python3 -m pip install"));
    assert!(!log.contains("brew install idb-companion"));
    assert!(log.contains("idb launch -f dev.orbi.fixture.ui -onboardingComplete true -seedUser qa@example.com --udid IOS-UDID"));
}

#[test]
fn orbi_test_ui_auto_installs_missing_idb_tooling() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_python3_fb_idb_install_mock(&mock_bin);
    create_brew_idb_companion_install_mock(&mock_bin);
    let workspace = create_ui_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.env("PATH", format!("{}:/usr/bin:/bin", mock_bin.display()));
    command.args(["--non-interactive", "test", "--ui", "--platform", "ios"]);
    let output = run_and_capture(&mut command);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{}",
        format_failure_output(&stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains(
        "python3 -m pip install --user --disable-pip-version-check --no-input fb-idb==1.1.7"
    ));
    assert!(log.contains("brew tap facebook/fb"));
    assert!(log.contains("brew install idb-companion"));
    assert!(log.contains("idb launch -f dev.orbi.fixture.ui -onboardingComplete true -seedUser qa@example.com --udid IOS-UDID"));
}

#[test]
fn orbi_test_ui_fails_early_with_install_hint_when_idb_is_missing() {
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
    assert!(stderr.contains("requires `idb` and `idb_companion`"));
    assert!(stderr.contains("Orbi first looks in PATH"));
    assert!(stderr.contains("brew install idb-companion"));
    assert!(stderr.contains("python3 -m pip install --user fb-idb==1.1.7"));

    let log = read_log(&log_path);
    assert!(!log.contains("xcrun "));
}

#[test]
fn orbi_ui_focus_uses_manifest_selected_xcode_and_installs_missing_runtime() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_idb_mock(&mock_bin);

    let runtime_ready_flag = temp.path().join("runtime-ready");
    create_runtime_installing_xcrun_mock(&mock_bin, &runtime_ready_flag);
    create_runtime_download_xcodebuild_mock(&mock_bin);

    let xcode_root = temp.path().join("Xcodes");
    let xcode_app = create_fake_xcode_bundle(&xcode_root, "Xcode-26.4.app", "26.4", "17E192");
    let workspace = create_ui_testing_workspace(temp.path());
    set_manifest_xcode(workspace.join("orbi.json").as_path(), "26.4");

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.env("ORBI_XCODE_SEARCH_ROOTS", &xcode_root);
    command.args(["--non-interactive", "ui", "focus", "--platform", "ios"]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let developer_dir = xcode_app.join("Contents/Developer");
    let log = read_log(&log_path);
    assert!(log.contains(&format!("DEVELOPER_DIR={}", developer_dir.display())));
    assert!(log.contains("xcodebuild -downloadPlatform iOS -exportPath"));
    assert!(log.contains("xcrun simctl runtime add"));
    assert!(log.contains("xcrun simctl boot IOS-UDID"));
    assert!(log.contains("idb focus --udid IOS-UDID"));
}

#[test]
fn orbi_ui_action_commands_forward_to_runner_and_idb() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_idb_mock(&mock_bin);
    let workspace = create_ui_testing_workspace(temp.path());

    let mut launch = base_command(&workspace, &home, &mock_bin, &log_path);
    launch.args([
        "--non-interactive",
        "ui",
        "launch-app",
        "--platform",
        "ios",
        "--focus",
    ]);
    let output = run_and_capture(&mut launch);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut tap = base_command(&workspace, &home, &mock_bin, &log_path);
    tap.args([
        "--non-interactive",
        "ui",
        "tap",
        "--platform",
        "ios",
        "--text",
        "Continue",
    ]);
    let output = run_and_capture(&mut tap);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut swipe = base_command(&workspace, &home, &mock_bin, &log_path);
    swipe.args([
        "--non-interactive",
        "ui",
        "swipe",
        "--platform",
        "ios",
        "--direction",
        "left",
    ]);
    let output = run_and_capture(&mut swipe);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("xcrun simctl list devices available --json"));
    assert!(log.contains("xcrun simctl install IOS-UDID"));
    assert!(log.contains("xcrun simctl launch IOS-UDID dev.orbi.fixture.ui"));
    assert!(log.contains("idb focus --udid IOS-UDID"));
    assert!(log.contains("idb ui describe-all --udid IOS-UDID"));
    assert!(log.contains("idb ui tap 140 142 --udid IOS-UDID"));
    assert!(log.contains("idb ui swipe --duration 0.500 --delta 5 354 426 39 426 --udid IOS-UDID"));
}

#[test]
fn orbi_ui_aux_commands_forward_to_idb() {
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
        "dev.orbi.fixture.ui",
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
    assert!(log.contains("idb crash list --bundle-id dev.orbi.fixture.ui --udid IOS-UDID"));
    assert!(log.contains("idb crash show mock-crash-1.ips --udid IOS-UDID"));
    assert!(log.contains("idb crash delete --since 1710000000 --all --udid IOS-UDID"));
}

#[test]
fn orbi_ui_dump_tree_prints_accessibility_json() {
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
fn orbi_ui_describe_point_prints_point_json() {
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
