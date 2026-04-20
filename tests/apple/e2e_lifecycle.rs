use std::fs;

use crate::support::{
    base_command, clear_log, create_api_key, create_build_xcrun_mock, create_codesign_mock,
    create_ditto_mock, create_home, create_security_mock, create_signing_workspace,
    latest_receipt_path, prepare_embedded_asc_state, read_log, run_and_capture, spawn_asc_mock,
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
    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    create_api_key(&api_key_path);
    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbi.fixture",
        "ExampleApp",
        false,
        true,
    );
    let api_base_url = format!("{}/v1", server.base_url);
    prepare_embedded_asc_state(
        &workspace,
        &home,
        &mock_bin,
        &log_path,
        &api_base_url,
        &api_key_path,
    );

    let mut build = base_command(&workspace, &home, &mock_bin, &log_path);
    build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
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
    assert!(build_log.contains("codesign --force --sign"));
    assert!(build_log.contains("ditto -c -k --keepParent"));
    let receipt_path = latest_receipt_path(&workspace);
    assert!(receipt_path.exists());

    clear_log(&log_path);

    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ORBI_ASC_BASE_URL", &api_base_url);
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
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
    assert!(submit_log.contains("xcrun altool --upload-app -f"));
    assert!(!submit_log.contains("swiftc"));

    let mut clean = base_command(&workspace, &home, &mock_bin, &log_path);
    clean.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "clean",
        "--local",
    ]);
    let clean_output = run_and_capture(&mut clean);
    assert!(
        clean_output.status.success(),
        "{}",
        String::from_utf8_lossy(&clean_output.stderr)
    );
    assert!(!workspace.join(".orbi").exists());

    let requests = server.requests();
    server.shutdown();
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
            .any(|request| request.starts_with("GET /v1/apps"))
    );
}
