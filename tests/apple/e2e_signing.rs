use std::fs;

use crate::support::{
    base_command, create_home, create_p12, create_security_mock, create_signing_workspace,
    orbit_data_dir, run_and_capture,
};

#[test]
fn signing_import_export_and_clean_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let security_db = temp.path().join("security-db.txt");
    fs::create_dir_all(&mock_bin).unwrap();

    create_security_mock(&mock_bin, &security_db);

    let p12_path = create_p12(&temp.path().join("identity"), "secret");

    let mut import = base_command(&workspace, &home, &mock_bin, &log_path);
    import.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "apple",
        "signing",
        "import",
        "--platform",
        "ios",
        "--distribution",
        "development",
        "--p12",
        p12_path.to_str().unwrap(),
        "--password",
        "secret",
    ]);
    let import_output = run_and_capture(&mut import);
    assert!(
        import_output.status.success(),
        "{}",
        String::from_utf8_lossy(&import_output.stderr)
    );

    let state_path = orbit_data_dir(&home).join("teams/TEAM123456/signing.json");
    let mut signing_state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let certificate_id = signing_state["certificates"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let profile_path =
        orbit_data_dir(&home).join("teams/TEAM123456/profiles/fixture.mobileprovision");
    fs::create_dir_all(profile_path.parent().unwrap()).unwrap();
    fs::write(&profile_path, b"fixture-profile").unwrap();
    signing_state["profiles"] = serde_json::json!([{
        "id": "PROFILE-1",
        "profile_type": "limited",
        "bundle_id": "dev.orbit.fixture",
        "path": profile_path,
        "uuid": "UUID-1",
        "certificate_ids": [certificate_id],
        "device_ids": []
    }]);
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&signing_state).unwrap(),
    )
    .unwrap();

    let export_dir = temp.path().join("exported-signing");
    let mut export = base_command(&workspace, &home, &mock_bin, &log_path);
    export.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "apple",
        "signing",
        "export",
        "--platform",
        "ios",
        "--distribution",
        "development",
        "--output-dir",
        export_dir.to_str().unwrap(),
    ]);
    let export_output = run_and_capture(&mut export);
    assert!(
        export_output.status.success(),
        "{}",
        String::from_utf8_lossy(&export_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&export_output.stdout);
    assert!(stdout.contains("p12_password: secret"));
    assert!(
        export_dir
            .join("ExampleApp-ios-development-debug.p12")
            .exists()
    );
    assert!(
        export_dir
            .join("ExampleApp-ios-development-debug.mobileprovision")
            .exists()
    );

    fs::create_dir_all(workspace.join(".orbit/build")).unwrap();
    fs::write(workspace.join(".orbit/build/marker"), b"build").unwrap();

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

    let mut second_export = base_command(&workspace, &home, &mock_bin, &log_path);
    second_export.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "apple",
        "signing",
        "export",
        "--platform",
        "ios",
        "--distribution",
        "development",
        "--output-dir",
        export_dir.to_str().unwrap(),
    ]);
    let second_export_output = run_and_capture(&mut second_export);
    assert!(!second_export_output.status.success());
}
