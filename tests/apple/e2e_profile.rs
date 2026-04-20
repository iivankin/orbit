use std::fs;

use crate::support::{
    base_command, create_build_xcrun_mock, create_home, read_log, run_and_capture,
};
use tempfile::tempdir;

#[test]
fn orbi_inspect_trace_prints_time_profile_diagnosis() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let workspace = temp.path().join("workspace");
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    let trace_path = workspace.join("sample.trace");

    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&mock_bin).unwrap();
    fs::create_dir_all(&trace_path).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args(["inspect-trace", "sample.trace"]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Process: Orbi"));
    assert!(stdout.contains("Template: Time Profiler"));
    assert!(stdout.contains("SELF TIME"));
    assert!(stdout.contains("heavyWork()"));
    assert!(stdout.contains("CALL STACKS"));

    let log = read_log(&log_path);
    assert!(log.contains("xcrun xctrace export --input"));
    assert!(log.contains("--toc"));
    assert!(log.contains("--xpath"));
}

#[test]
fn orbi_inspect_trace_retries_until_trace_is_exportable() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let workspace = temp.path().join("workspace");
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    let trace_path = workspace.join("sample.trace");
    let fail_count_path = temp.path().join("export-fail-count.txt");

    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&mock_bin).unwrap();
    fs::create_dir_all(&trace_path).unwrap();
    fs::write(&fail_count_path, "2\n").unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.env("MOCK_XCTRACE_EXPORT_FAIL_COUNT_FILE", &fail_count_path);
    command.args(["inspect-trace", "sample.trace"]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Process: Orbi"));
    assert_eq!(fs::read_to_string(&fail_count_path).unwrap(), "0\n");

    let log = read_log(&log_path);
    assert!(log.matches("xcrun xctrace export --input").count() >= 4);
}

#[test]
fn orbi_inspect_trace_prints_allocations_diagnosis() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let workspace = temp.path().join("workspace");
    let mock_bin = temp.path().join("mock-bin");
    let sdk_root = temp.path().join("sdk-root");
    let log_path = temp.path().join("mock.log");
    let trace_path = workspace.join("allocations.trace");

    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&mock_bin).unwrap();
    fs::create_dir_all(&trace_path).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args(["inspect-trace", "allocations.trace"]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Template: Allocations"));
    assert!(stdout.contains("Live bytes: 32.2 MiB"));
    assert!(stdout.contains("LIVE BY CATEGORY"));
    assert!(stdout.contains("Malloc 256.0 KiB"));
    assert!(stdout.contains("LIVE BY RESPONSIBLE CALLER"));
    assert!(stdout.contains("allocateChunk()"));

    let log = read_log(&log_path);
    assert!(log.contains("--toc"));
    assert!(log.contains("Allocations"));
    assert!(log.contains("Allocations List"));
}
