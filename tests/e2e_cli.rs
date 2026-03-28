use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn orbit_bin() -> &'static str {
    env!("CARGO_BIN_EXE_orbit")
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn create_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    home
}

fn base_command(workspace: &Path, home: &Path, mock_bin: &Path, log_path: &Path) -> Command {
    let mut command = Command::new(orbit_bin());
    command.current_dir(workspace);
    command.env("HOME", home);
    command.env(
        "PATH",
        format!(
            "{}:{}",
            mock_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    command.env("MOCK_LOG", log_path);
    command
}

fn create_watch_workspace(root: &Path) -> PathBuf {
    let workspace = root.join("watch-workspace");
    fs::create_dir_all(workspace.join("Sources/App")).unwrap();
    fs::create_dir_all(workspace.join("Sources/WatchApp")).unwrap();
    fs::create_dir_all(workspace.join("Sources/WatchExtension")).unwrap();
    fs::write(
        workspace.join("Sources/App/App.swift"),
        "import SwiftUI\n@main struct ExampleIOSApp: App { var body: some Scene { WindowGroup { Text(\"Phone\") } } }\n",
    )
    .unwrap();
    fs::write(
        workspace.join("Sources/WatchApp/App.swift"),
        "import SwiftUI\n@main struct ExampleWatchApp: App { var body: some Scene { WindowGroup { Text(\"Watch\") } } }\n",
    )
    .unwrap();
    fs::write(
        workspace.join("Sources/WatchExtension/Extension.swift"),
        "import SwiftUI\n@main struct ExampleWatchExtension: App { var body: some Scene { WindowGroup { Text(\"Ext\") } } }\n",
    )
    .unwrap();
    fs::write(
        workspace.join("orbit.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": "https://orbit.dev/schemas/apple-app.v1.json",
            "name": "WatchFixture",
            "bundle_id": "dev.orbit.fixture.watch",
            "version": "0.1.0",
            "build": 1,
            "platforms": {
                "ios": "18.0",
                "watchos": "11.0"
            },
            "sources": [
                "Sources/App"
            ],
            "watch": {
                "sources": [
                    "Sources/WatchApp"
                ],
                "extension": {
                    "sources": [
                        "Sources/WatchExtension"
                    ],
                    "entry": {
                        "class": "WatchExtensionDelegate"
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    workspace
}

fn create_signing_workspace(root: &Path) -> PathBuf {
    let workspace = root.join("signing-workspace");
    fs::create_dir_all(workspace.join("Sources/App")).unwrap();
    fs::write(
        workspace.join("Sources/App/App.swift"),
        "import SwiftUI\n@main struct ExampleApp: App { var body: some Scene { WindowGroup { Text(\"App\") } } }\n",
    )
    .unwrap();
    fs::write(
        workspace.join("orbit.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": "https://orbit.dev/schemas/apple-app.v1.json",
            "name": "ExampleApp",
            "bundle_id": "dev.orbit.fixture",
            "version": "0.1.0",
            "build": 1,
            "team_id": "TEAM123456",
            "platforms": {
                "ios": "18.0"
            },
            "sources": [
                "Sources/App"
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    workspace
}

fn create_security_mock(mock_bin: &Path, db_path: &Path) {
    write_executable(
        &mock_bin.join("security"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "security $@" >> "$MOCK_LOG"
db="{db}"
cmd="$1"
shift
case "$cmd" in
  add-generic-password)
    account=""
    service=""
    password=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) account="$2"; shift 2 ;;
        -s) service="$2"; shift 2 ;;
        -w) password="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    mkdir -p "$(dirname "$db")"
    tmp="$db.tmp"
    touch "$db"
    grep -v "^$service|$account|" "$db" > "$tmp" || true
    printf '%s|%s|%s\n' "$service" "$account" "$password" >> "$tmp"
    mv "$tmp" "$db"
    ;;
  find-generic-password)
    account=""
    service=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) account="$2"; shift 2 ;;
        -s) service="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    value="$(awk -F'|' -v svc="$service" -v acct="$account" '$1 == svc && $2 == acct {{ print $3; exit }}' "$db" 2>/dev/null)"
    if [ -z "$value" ]; then
      exit 44
    fi
    printf '%s\n' "$value"
    ;;
  delete-generic-password)
    account=""
    service=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) account="$2"; shift 2 ;;
        -s) service="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    tmp="$db.tmp"
    touch "$db"
    grep -v "^$service|$account|" "$db" > "$tmp" || true
    mv "$tmp" "$db"
    ;;
  create-keychain|unlock-keychain|set-keychain-settings|import|set-key-partition-list)
    ;;
  find-identity)
    printf '  1) "Imported Identity"\n'
    ;;
  *)
    echo "unexpected security command: $cmd" >&2
    exit 1
    ;;
esac
"#,
            db = db_path.display()
        ),
    );
}

fn create_watch_xcrun_mock(mock_bin: &Path, sdk_root: &Path) {
    write_executable(
        &mock_bin.join("xcrun"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "xcrun $@" >> "$MOCK_LOG"
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-path" ]; then
  mkdir -p "{sdk}"
  printf '%s\n' "{sdk}"
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "swiftc" ]; then
  out=""
  module=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
      out="$arg"
    fi
    if [ "$prev" = "-emit-module-path" ]; then
      module="$arg"
    fi
    prev="$arg"
  done
  mkdir -p "$(dirname "$out")"
  : > "$out"
  if [ -n "$module" ]; then
    mkdir -p "$(dirname "$module")"
    : > "$module"
  fi
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "list" ] && [ "$3" = "devices" ]; then
  cat <<'JSON'
{{"devices":{{"com.apple.CoreSimulator.SimRuntime.watchOS-11-0":[{{"udid":"WATCH-UDID","name":"Apple Watch Series 9","state":"Shutdown"}}]}}}}
JSON
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "boot" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "bootstatus" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "install" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "launch" ]; then
  exit 0
fi
echo "unexpected xcrun command: $@" >&2
exit 1
"#,
            sdk = sdk_root.display()
        ),
    );
}

fn create_altool_xcrun_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("xcrun"),
        r#"#!/bin/sh
set -eu
echo "xcrun $@" >> "$MOCK_LOG"
if [ "$1" = "altool" ]; then
  exit 0
fi
echo "unexpected xcrun command: $@" >&2
exit 1
"#,
    );
}

fn create_passthrough_mock(mock_bin: &Path, name: &str) {
    write_executable(
        &mock_bin.join(name),
        &format!(
            r#"#!/bin/sh
set -eu
echo "{name} $@" >> "$MOCK_LOG"
"#,
        ),
    );
}

fn create_p12(identity_dir: &Path, password: &str) -> PathBuf {
    fs::create_dir_all(identity_dir).unwrap();
    let key_path = identity_dir.join("key.pem");
    let cert_path = identity_dir.join("cert.pem");
    let p12_path = identity_dir.join("signing.p12");
    assert!(
        Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                key_path.to_str().unwrap(),
                "-out",
                cert_path.to_str().unwrap(),
                "-subj",
                "/CN=Orbit Test",
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("openssl")
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
            .unwrap()
            .success()
    );
    p12_path
}

fn create_api_key(path: &Path) {
    assert!(
        Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-out",
                path.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
}

fn run_and_capture(command: &mut Command) -> Output {
    command.output().unwrap()
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn spawn_asc_mock() -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_clone = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        let mut idle_polls = 0_u32;
        loop {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => {
                    idle_polls = 0;
                    connection
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if idle_polls > 50 {
                        break;
                    }
                    idle_polls += 1;
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(_) => break,
            };
            let mut buffer = [0_u8; 16384];
            let bytes = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..bytes]).to_string();
            let first_line = request.lines().next().unwrap_or_default().to_owned();
            requests_clone.lock().unwrap().push(first_line.clone());
            let body = if first_line.starts_with("GET /v1/bundleIds") {
                r#"{"data":[{"id":"BUNDLE1","type":"bundleIds","attributes":{"name":"ExampleApp","identifier":"dev.orbit.fixture","platform":"IOS"},"relationships":{}}],"included":[]}"#
            } else if first_line.starts_with("GET /v1/apps") {
                r#"{"data":[]}"#
            } else if first_line.starts_with("POST /v1/apps") {
                r#"{"data":{"id":"APP1","type":"apps","attributes":{"name":"ExampleApp","sku":"DEV-ORBIT-FIXTURE","primaryLocale":"en-US"},"relationships":{}}}"#
            } else {
                r#"{"errors":[{"status":"404","code":"NOT_FOUND","title":"Not Found","detail":"unexpected request"}]}"#
            };
            let status = if body.contains("\"errors\"") {
                "404 Not Found"
            } else {
                "200 OK"
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (address, requests, handle)
}

#[test]
fn watchos_run_debug_uses_simctl_and_lldb() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_watch_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_watch_xcrun_mock(&mock_bin, &sdk_root);
    create_passthrough_mock(&mock_bin, "lldb");
    create_passthrough_mock(&mock_bin, "open");

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
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
        "xcrun simctl launch --wait-for-debugger --terminate-running-process WATCH-UDID dev.orbit.fixture.watch.watchkitapp"
    ));
    assert!(log.contains("lldb --file"));
    assert!(log.contains("process attach -i -w -n WatchApp"));
    assert!(log.contains("process continue"));
}

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

    let state_path = home.join("Library/Application Support/orbit/teams/TEAM123456/signing.json");
    let mut signing_state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let certificate_id = signing_state["certificates"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let profile_path = home.join(
        "Library/Application Support/orbit/teams/TEAM123456/profiles/fixture.mobileprovision",
    );
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

#[test]
fn push_auth_key_export_copies_team_scoped_p8() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    let team_dir = home.join("Library/Application Support/orbit/teams/TEAM123456");
    let push_keys_dir = team_dir.join("push-keys");
    fs::create_dir_all(&push_keys_dir).unwrap();
    let push_key_path = push_keys_dir.join("PUSHKEY123.p8");
    fs::write(&push_key_path, b"push-auth-key").unwrap();
    fs::write(
        team_dir.join("signing.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "certificates": [],
            "profiles": [],
            "push_keys": [{
                "id": "PUSHKEY123",
                "name": "@orbit/apns",
                "path": push_key_path
            }],
            "push_certificates": []
        }))
        .unwrap(),
    )
    .unwrap();

    let export_path = temp.path().join("AuthKey_PUSHKEY123.p8");
    let mut export = base_command(&workspace, &home, &mock_bin, &log_path);
    export.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "apple",
        "signing",
        "export-push",
        "--output",
        export_path.to_str().unwrap(),
    ]);
    let output = run_and_capture(&mut export);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("team_id: TEAM123456"));
    assert!(stdout.contains("key_id: PUSHKEY123"));
    assert_eq!(fs::read(&export_path).unwrap(), b"push-auth-key");
}

#[test]
fn submit_uses_existing_receipt_without_rebuilding() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_altool_xcrun_mock(&mock_bin);

    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    create_api_key(&api_key_path);
    let artifact_path = workspace.join("ExampleApp.ipa");
    fs::write(&artifact_path, b"ipa").unwrap();
    let receipt_dir = workspace.join(".orbit/receipts");
    fs::create_dir_all(&receipt_dir).unwrap();
    let receipt_path = receipt_dir.join("receipt.json");
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": "receipt-1",
            "target": "ExampleApp",
            "platform": "ios",
            "configuration": "release",
            "distribution": "app-store",
            "destination": "device",
            "bundle_id": "dev.orbit.fixture",
            "bundle_path": workspace.join("ExampleApp.app"),
            "artifact_path": artifact_path,
            "created_at_unix": 1,
            "submit_eligible": true
        }))
        .unwrap(),
    )
    .unwrap();

    let (base_url, requests, handle) = spawn_asc_mock();
    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ORBIT_ASC_BASE_URL", &base_url);
    submit.env("ORBIT_ASC_API_KEY_PATH", &api_key_path);
    submit.env("ORBIT_ASC_KEY_ID", "KEY1234567");
    submit.env(
        "ORBIT_ASC_ISSUER_ID",
        "00000000-0000-0000-0000-000000000000",
    );
    submit.env("ORBIT_APPLE_TEAM_ID", "TEAM123456");
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "submit",
        "--receipt",
        receipt_path.to_str().unwrap(),
    ]);
    let submit_output = run_and_capture(&mut submit);
    handle.join().unwrap();

    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );
    let log = read_log(&log_path);
    assert!(log.contains("xcrun altool --validate-app"));
    assert!(log.contains("xcrun altool --upload-package"));
    assert!(!log.contains("swiftc"));

    let requests = requests.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /v1/bundleIds"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /v1/apps"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /v1/apps"))
    );
}
