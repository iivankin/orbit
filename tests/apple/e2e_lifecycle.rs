use std::fs;

use crate::support::submit_mock::spawn_submit_mock;
use crate::support::{
    base_command, clear_log, create_api_key, create_build_xcrun_mock, create_codesign_mock,
    create_ditto_mock, create_home, create_security_mock, create_signing_workspace,
    create_submit_swinfo_mock, latest_receipt_path, read_log, run_and_capture, spawn_asc_mock,
};

#[test]
fn app_store_build_submit_and_clean_complete_full_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    let security_db = temp.path().join("security-db.txt");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_security_mock(&mock_bin, &security_db);
    create_codesign_mock(&mock_bin);
    create_ditto_mock(&mock_bin);
    create_submit_swinfo_mock(&mock_bin);

    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    create_api_key(&api_key_path);
    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbit.fixture",
        "ExampleApp",
        false,
    );
    let submit_server = spawn_submit_mock(temp.path(), "dev.orbit.fixture");

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
    let build_output = run_and_capture(&mut build);
    assert!(
        build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let build_log = read_log(&log_path);
    assert!(build_log.contains("xcrun --sdk iphoneos --show-sdk-path"));
    assert!(build_log.contains("xcrun --sdk iphoneos swiftc"));
    assert!(build_log.contains("security import"));
    assert!(build_log.contains("codesign --force --sign"));
    assert!(build_log.contains("ditto -c -k --keepParent"));
    let receipt_path = latest_receipt_path(&workspace);
    assert!(receipt_path.exists());

    clear_log(&log_path);

    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ORBIT_ASC_BASE_URL", &server.base_url);
    submit.env("ORBIT_CONTENT_DELIVERY_BASE_URL", &submit_server.base_url);
    submit.env("ORBIT_ASC_API_KEY_PATH", &api_key_path);
    submit.env("ORBIT_ASC_KEY_ID", "KEY1234567");
    submit.env(
        "ORBIT_ASC_ISSUER_ID",
        "00000000-0000-0000-0000-000000000000",
    );
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
        receipt_path.to_str().unwrap(),
    ]);
    let submit_output = run_and_capture(&mut submit);
    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );

    let submit_log = read_log(&log_path);
    assert!(submit_log.contains("swinfo -f"));
    assert!(!submit_log.contains("altool"));
    assert!(!submit_log.contains("swiftc"));

    let mut clean = base_command(&workspace, &home, &mock_bin, &log_path);
    clean.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "clean",
        "--local",
    ]);
    let clean_output = run_and_capture(&mut clean);
    assert!(
        clean_output.status.success(),
        "{}",
        String::from_utf8_lossy(&clean_output.stderr)
    );
    assert!(!workspace.join(".orbit").exists());

    let requests = server.requests();
    let submit_requests = submit_server.requests();
    server.shutdown();
    submit_server.shutdown();
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /v1/bundleIds"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /v1/certificates"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /v1/profiles"))
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
