use std::fs;

use crate::support::{
    base_command, create_build_xcrun_mock, create_home, create_testing_swift_mock,
    create_testing_workspace, read_log, run_and_capture,
};
use tempfile::tempdir;

#[test]
fn orbit_test_runs_swift_testing_for_manifest_unit_tests() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_testing_swift_mock(&mock_bin);
    let workspace = create_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args(["test", "--trace"]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("xcrun xctrace record --template Time Profiler"));
    assert!(log.contains("--launch -- swift test --disable-keychain --package-path"));
    assert!(log.contains("swift test --disable-keychain --package-path"));
    assert!(log.contains("--enable-swift-testing"));
    assert!(log.contains("--disable-xctest"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("trace: "));

    let profiles_dir = workspace.join(".orbit").join("artifacts").join("profiles");
    let entries = fs::read_dir(&profiles_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].extension().and_then(|value| value.to_str()),
        Some("trace")
    );

    let package_root = workspace.join(".orbit/tests/swift-testing/package");
    let package_manifest = fs::read_to_string(package_root.join("Package.swift")).unwrap();
    assert!(package_manifest.contains(".executableTarget("));
    assert!(package_manifest.contains("name: \"ExampleApp\""));
    assert!(package_manifest.contains(".testTarget("));
    assert!(package_manifest.contains("name: \"ExampleAppUnitTests\""));
    assert!(
        package_root
            .join("Targets/ExampleApp/Sources/input-0")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        package_root
            .join("Targets/ExampleAppUnitTests/Sources/input-0")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn orbit_test_trace_fails_when_trace_finalization_fails() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_testing_swift_mock(&mock_bin);
    let workspace = create_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.env("MOCK_XCTRACE_EXPORT_FAIL", "1");
    command.args(["test", "--trace"]);
    let output = run_and_capture(&mut command);
    assert!(!output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("trace: "));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed to finalize CPU trace"));
}
