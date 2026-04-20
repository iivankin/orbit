use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

use base64::Engine as _;

#[path = "../../support/command_fixtures.rs"]
mod command_fixtures;

mod asc_mock;
mod crypto;
mod tool_mocks;
mod ui_helpers;
mod workspaces;

#[allow(unused_imports)]
pub use self::asc_mock::{AscMockServer, spawn_asc_mock};
pub use self::command_fixtures::{
    base_command, clear_log, create_home, latest_receipt_path, orbi_bin, orbi_cache_dir, read_log,
    run_and_capture, sourcekit_lsp_command, write_executable,
};
pub use self::crypto::create_api_key;
pub use self::tool_mocks::{
    create_brew_idb_companion_install_mock, create_build_xcrun_mock, create_codesign_mock,
    create_ditto_mock, create_hdiutil_mock, create_idb_mock, create_lldb_attach_mock,
    create_passthrough_mock, create_python3_fb_idb_install_mock, create_quality_swift_mock,
    create_security_mock, create_sw_vers_mock, create_testing_swift_mock, create_watch_xcrun_mock,
    create_xcodebuild_mock,
};
pub use self::ui_helpers::{
    create_fake_xcode_bundle, create_runtime_download_xcodebuild_mock,
    create_runtime_installing_xcrun_mock, format_failure_output, latest_ui_report_path,
    set_manifest_platforms, set_manifest_xcode,
};
pub use self::workspaces::{
    create_asset_resource_workspace, create_git_swift_package_workspace,
    create_macos_app_store_workspace, create_macos_developer_id_workspace,
    create_macos_universal_workspace, create_mixed_language_workspace, create_resource_workspace,
    create_semver_git_swift_package_workspace, create_signing_workspace,
    create_swift_package_workspace, create_testing_workspace, create_ui_testing_workspace,
    create_watch_workspace, create_xcframework_workspace,
};

pub fn seed_mock_asc_auth(home: &std::path::Path, team_id: &str, api_key_path: &std::path::Path) {
    let private_key_pem = fs::read_to_string(api_key_path).unwrap();
    let payload = base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_vec(&serde_json::json!({
            "issuerId": "00000000-0000-0000-0000-000000000000",
            "keyId": "KEY1234567",
            "privateKeyPem": private_key_pem,
        }))
        .unwrap(),
    );
    let auth_dir = home.join(".asc-sync/auth");
    fs::create_dir_all(&auth_dir).unwrap();
    fs::write(auth_dir.join(format!("{team_id}.json")), payload).unwrap();
}

pub fn prepare_embedded_asc_state(
    workspace: &std::path::Path,
    home: &std::path::Path,
    mock_bin: &std::path::Path,
    log_path: &std::path::Path,
    base_url: &str,
    api_key_path: &std::path::Path,
) {
    seed_mock_asc_auth(home, "TEAM123456", api_key_path);

    let mut apply = base_command(workspace, home, mock_bin, log_path);
    apply.env("ORBI_ASC_BASE_URL", base_url);
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

    let mut import = base_command(workspace, home, mock_bin, log_path);
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

    clear_log(log_path);
}

pub fn prepare_manual_developer_id_embedded_asc_state(
    workspace: &std::path::Path,
    home: &std::path::Path,
    mock_bin: &std::path::Path,
    log_path: &std::path::Path,
    security_db_path: &std::path::Path,
) {
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.join("orbi.json")).unwrap()).unwrap();
    let asc = manifest.get("asc").cloned().unwrap();
    let team_id = asc.get("team_id").and_then(|value| value.as_str()).unwrap();
    let (bundle_logical_name, bundle_spec) = asc
        .get("bundle_ids")
        .and_then(|value| value.as_object())
        .and_then(|bundle_ids| bundle_ids.iter().next())
        .unwrap();
    let bundle_id = bundle_spec
        .get("bundle_id")
        .and_then(|value| value.as_str())
        .unwrap();
    let (profile_logical_name, profile_spec) = asc
        .get("profiles")
        .and_then(|value| value.as_object())
        .and_then(|profiles| profiles.iter().next())
        .unwrap();
    let profile_name = profile_spec
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap();
    let certificate_logical_name = profile_spec
        .get("certs")
        .and_then(|value| value.as_array())
        .and_then(|certs| certs.first())
        .and_then(|value| value.as_str())
        .unwrap();
    let certificate_name = asc
        .get("certs")
        .and_then(|value| value.as_object())
        .and_then(|certs| certs.get(certificate_logical_name))
        .and_then(|certificate| certificate.get("name"))
        .and_then(|value| value.as_str())
        .unwrap();

    let orbi_asc_dir = workspace.join(".orbi/asc");
    fs::create_dir_all(&orbi_asc_dir).unwrap();
    let workspace_bundle = asc_sync::sync::Workspace::new(workspace);
    let passwords = BTreeMap::from([
        (
            asc_sync::scope::Scope::Developer,
            age::secrecy::SecretString::from("developer-test-password".to_owned()),
        ),
        (
            asc_sync::scope::Scope::Release,
            age::secrecy::SecretString::from("release-test-password".to_owned()),
        ),
    ]);
    asc_sync::bundle::initialize_bundle(&workspace_bundle.bundle_path, team_id, &passwords)
        .unwrap();
    let mut runtime = workspace_bundle.create_runtime().unwrap();

    let pkcs12_password = "developer-id-test-password".to_owned();
    let (pkcs12, serial_number) =
        create_pkcs12_fixture(&orbi_asc_dir, certificate_name, &pkcs12_password);
    runtime.set_cert(certificate_logical_name.to_owned(), pkcs12);
    runtime.set_cert_password(certificate_logical_name.to_owned(), pkcs12_password.clone());
    runtime.set_profile(
        profile_logical_name.to_owned(),
        developer_id_profile_plist(team_id, bundle_id, "UUID-MAC-DIRECT"),
    );

    let mut state = asc_sync::state::State::new(team_id);
    state.bundle_ids.insert(
        bundle_logical_name.to_owned(),
        asc_sync::state::ManagedBundleId {
            apple_id: "BUNDLE1".into(),
            bundle_id: bundle_id.to_owned(),
        },
    );
    state.certs.insert(
        certificate_logical_name.to_owned(),
        asc_sync::state::ManagedCertificate {
            apple_id: Some("CERT1".into()),
            kind: "DEVELOPER_ID_APPLICATION_G2".into(),
            name: certificate_name.to_owned(),
            serial_number: serial_number.clone(),
            p12_password: pkcs12_password,
        },
    );
    state.profiles.insert(
        profile_logical_name.to_owned(),
        asc_sync::state::ManagedProfile {
            apple_id: "PROFILE1".into(),
            name: profile_name.to_owned(),
            kind: "MAC_APP_DIRECT".into(),
            bundle_id: bundle_logical_name.to_owned(),
            certs: vec![certificate_logical_name.to_owned()],
            devices: Vec::new(),
            uuid: "UUID-MAC-DIRECT".into(),
        },
    );

    asc_sync::bundle::write_scope(
        &workspace_bundle.bundle_path,
        &runtime,
        asc_sync::scope::Scope::Release,
        &state,
        passwords.get(&asc_sync::scope::Scope::Release).unwrap(),
    )
    .unwrap();

    let mut import = base_command(workspace, home, mock_bin, log_path);
    import.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "asc",
        "signing",
        "import",
    ]);
    import.env(
        asc_sync::bundle::DEVELOPER_BUNDLE_PASSWORD_ENV,
        "developer-test-password",
    );
    import.env(
        asc_sync::bundle::RELEASE_BUNDLE_PASSWORD_ENV,
        "release-test-password",
    );
    let import_output = run_and_capture(&mut import);
    assert!(
        import_output.status.success(),
        "{}",
        String::from_utf8_lossy(&import_output.stderr)
    );
    assert_imported_identity_matches_serial(security_db_path, certificate_name, &serial_number);

    clear_log(log_path);
}

fn create_pkcs12_fixture(
    root: &std::path::Path,
    common_name: &str,
    password: &str,
) -> (Vec<u8>, String) {
    let key_path = root.join("developer-id-test.key");
    let cert_path = root.join("developer-id-test.pem");
    let p12_path = root.join("developer-id-test.p12");

    let subject = format!("/CN={common_name}");
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "365",
            "-keyout",
            key_path.to_str().unwrap(),
            "-out",
            cert_path.to_str().unwrap(),
            "-subj",
            &subject,
        ])
        .status()
        .unwrap();
    assert!(status.success(), "failed to generate test certificate");

    let status = Command::new("openssl")
        .args([
            "pkcs12",
            "-export",
            "-inkey",
            key_path.to_str().unwrap(),
            "-in",
            cert_path.to_str().unwrap(),
            "-out",
            p12_path.to_str().unwrap(),
            "-passout",
            &format!("pass:{password}"),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "failed to generate test PKCS#12");

    let serial_output = Command::new("openssl")
        .args([
            "x509",
            "-in",
            cert_path.to_str().unwrap(),
            "-noout",
            "-serial",
        ])
        .output()
        .unwrap();
    assert!(
        serial_output.status.success(),
        "failed to read test certificate serial"
    );
    let serial_number = String::from_utf8_lossy(&serial_output.stdout)
        .trim()
        .strip_prefix("serial=")
        .unwrap()
        .to_owned();

    (fs::read(&p12_path).unwrap(), serial_number)
}

fn developer_id_profile_plist(team_id: &str, bundle_id: &str, uuid: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>ApplicationIdentifierPrefix</key>
  <array>
    <string>{team_id}</string>
  </array>
  <key>Entitlements</key>
  <dict>
    <key>application-identifier</key>
    <string>{team_id}.{bundle_id}</string>
    <key>com.apple.developer.team-identifier</key>
    <string>{team_id}</string>
  </dict>
  <key>Name</key>
  <string>Developer ID Test Profile</string>
  <key>TeamIdentifier</key>
  <array>
    <string>{team_id}</string>
  </array>
  <key>UUID</key>
  <string>{uuid}</string>
</dict>
</plist>
"#
    )
    .into_bytes()
}

fn assert_imported_identity_matches_serial(
    security_db_path: &std::path::Path,
    certificate_name: &str,
    expected_serial: &str,
) {
    let entries = fs::read_to_string(security_db_path).unwrap_or_default();
    let matching_entry = entries.lines().find_map(|line| {
        let mut fields = line.split('|');
        let kind = fields.next()?;
        let _keychain = fields.next()?;
        let _hash = fields.next()?;
        let entry_name = fields.next()?;
        let cert_path = fields.next()?;
        (kind == "import" && entry_name == certificate_name).then(|| cert_path.to_owned())
    });
    let cert_path = matching_entry.unwrap_or_else(|| {
        panic!(
            "missing imported identity `{certificate_name}` in {}\n{}",
            security_db_path.display(),
            entries
        )
    });
    let output = Command::new("openssl")
        .args(["x509", "-in", &cert_path, "-noout", "-serial"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to inspect imported certificate serial"
    );
    let imported_serial = String::from_utf8_lossy(&output.stdout)
        .trim()
        .strip_prefix("serial=")
        .unwrap()
        .to_owned();
    assert_eq!(imported_serial, expected_serial);
}
