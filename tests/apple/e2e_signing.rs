use std::fs;
use std::path::Path;

use crate::support::{
    base_command, create_api_key, create_build_xcrun_mock, create_codesign_mock,
    create_hdiutil_mock, create_home, create_macos_app_store_workspace,
    create_macos_developer_id_workspace, create_security_mock, create_signing_workspace,
    create_sw_vers_mock, create_xcodebuild_mock, latest_receipt_path, prepare_embedded_asc_state,
    prepare_manual_developer_id_embedded_asc_state, read_log, run_and_capture, seed_mock_asc_auth,
    spawn_asc_mock, write_executable,
};

fn imported_identity_hash(security_db: &Path, name: &str) -> String {
    let entries = fs::read_to_string(security_db).unwrap_or_default();
    entries
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('|');
            let kind = fields.next()?;
            let _keychain = fields.next()?;
            let hash = fields.next()?;
            let entry_name = fields.next()?;
            (kind == "import" && entry_name == name).then(|| hash.to_owned())
        })
        .next()
        .unwrap_or_else(|| {
            panic!(
                "missing imported identity `{name}` in {}",
                security_db.display()
            )
        })
}

#[test]
fn asc_signing_apply_import_print_build_settings_and_clean_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let security_db = temp.path().join("security-db.txt");
    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    fs::create_dir_all(&mock_bin).unwrap();

    create_security_mock(&mock_bin, &security_db);
    create_api_key(&api_key_path);

    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbi.fixture",
        "ExampleApp",
        false,
        false,
    );
    let api_base_url = format!("{}/v1", server.base_url);
    seed_mock_asc_auth(&home, "TEAM123456", &api_key_path);

    let mut apply = base_command(&workspace, &home, &mock_bin, &log_path);
    apply.env("ORBI_ASC_BASE_URL", &api_base_url);
    apply.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "asc",
        "apply",
    ]);
    let apply_output = run_and_capture(&mut apply);
    assert!(
        apply_output.status.success(),
        "{}",
        String::from_utf8_lossy(&apply_output.stderr)
    );
    assert!(workspace.join("signing.ascbundle").exists());

    let mut import = base_command(&workspace, &home, &mock_bin, &log_path);
    import.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "asc",
        "signing",
        "import",
    ]);
    let import_output = run_and_capture(&mut import);
    assert!(
        import_output.status.success(),
        "{}",
        String::from_utf8_lossy(&import_output.stderr)
    );

    let import_log = read_log(&log_path);
    assert!(import_log.contains("security import"));

    let mut build_settings = base_command(&workspace, &home, &mock_bin, &log_path);
    build_settings.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "asc",
        "signing",
        "print-build-settings",
    ]);
    let build_settings_output = run_and_capture(&mut build_settings);
    assert!(
        build_settings_output.status.success(),
        "{}",
        String::from_utf8_lossy(&build_settings_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&build_settings_output.stdout);
    assert!(stdout.contains("CODE_SIGN_STYLE=Manual"));
    assert!(stdout.contains("DEVELOPMENT_TEAM=TEAM123456"));
    assert!(stdout.contains("PROVISIONING_PROFILE_SPECIFIER=ios-app-store"));

    let requests = server.requests();
    server.shutdown();
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

    let mut second_build_settings = base_command(&workspace, &home, &mock_bin, &log_path);
    second_build_settings.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "asc",
        "signing",
        "print-build-settings",
    ]);
    let second_build_settings_output = run_and_capture(&mut second_build_settings);
    assert!(!second_build_settings_output.status.success());
}

#[test]
fn developer_id_build_exports_signed_dmg() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_macos_developer_id_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    let security_db = temp.path().join("security-db.txt");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_security_mock(&mock_bin, &security_db);
    create_codesign_mock(&mock_bin);
    create_hdiutil_mock(&mock_bin);
    create_xcodebuild_mock(&mock_bin);
    create_sw_vers_mock(&mock_bin);
    write_executable(
        &mock_bin.join("spctl"),
        r#"#!/bin/sh
set -eu
echo "spctl $@" >> "$MOCK_LOG"
printf '%s: rejected\n' "$5" >&2
printf 'source=Unnotarized Developer ID\n' >&2
exit 1
"#,
    );
    prepare_manual_developer_id_embedded_asc_state(
        &workspace,
        &home,
        &mock_bin,
        &log_path,
        &security_db,
    );

    let mut build = base_command(&workspace, &home, &mock_bin, &log_path);
    build.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "build",
        "--platform",
        "macos",
        "--distribution",
        "developer-id",
        "--release",
    ]);
    let build_output = run_and_capture(&mut build);

    assert!(
        build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let receipt_path = latest_receipt_path(&workspace);
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    let artifact_path = std::path::PathBuf::from(receipt["artifact_path"].as_str().unwrap());
    assert!(
        artifact_path.exists(),
        "missing dmg at {}",
        artifact_path.display()
    );
    assert_eq!(
        artifact_path.extension().and_then(|value| value.to_str()),
        Some("dmg")
    );

    let build_log = read_log(&log_path);
    let developer_id_hash =
        imported_identity_hash(&security_db, "Developer ID Application: Example Team");
    assert!(build_log.contains("hdiutil create"));
    assert!(build_log.contains("spctl -a -vvv --type open"));
    assert!(build_log.contains("codesign -dv --verbose=4"));
    assert!(build_log.contains(&format!("--sign {developer_id_hash}")));
    assert!(!build_log.contains("productbuild --component"));
    assert!(!build_log.contains("pkgutil --check-signature"));
}

#[test]
fn mac_app_store_build_exports_signed_app_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_macos_app_store_workspace(temp.path());
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
        "dev.orbi.fixture.macos-store",
        "ExampleMacApp",
        false,
        false,
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
        "macos",
        "--distribution",
        "mac-app-store",
        "--release",
    ]);
    let build_output = run_and_capture(&mut build);

    let requests = server.requests();
    server.shutdown();

    assert!(
        build_output.status.success(),
        "{}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let receipt_path = latest_receipt_path(&workspace);
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    let artifact_path = std::path::PathBuf::from(receipt["artifact_path"].as_str().unwrap());
    assert!(
        artifact_path.exists(),
        "missing app bundle at {}",
        artifact_path.display()
    );
    assert!(artifact_path.is_dir());
    assert_eq!(
        artifact_path.extension().and_then(|value| value.to_str()),
        Some("app")
    );

    let build_log = read_log(&log_path);
    let distribution_hash =
        imported_identity_hash(&security_db, "Apple Distribution: Example Team");
    assert!(build_log.contains(&format!("--sign {distribution_hash}")));
    assert!(!build_log.contains("productbuild --component"));
    assert!(!build_log.contains("hdiutil create"));
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
}
