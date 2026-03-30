use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base64::Engine as _;

pub struct AscMockServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl AscMockServer {
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }
}

struct AscMockState {
    bundle_id_created: bool,
    app_created: bool,
    certificate_der: Option<String>,
    certificate_serial: Option<String>,
}

pub fn spawn_asc_mock(
    root: &Path,
    team_id: &str,
    bundle_identifier: &str,
    app_name: &str,
    preseed_bundle_id: bool,
) -> AscMockServer {
    let ca_root = root.join("asc-ca");
    fs::create_dir_all(&ca_root).unwrap();
    let (ca_key_path, ca_cert_path) = create_certificate_authority(&ca_root);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_clone = Arc::clone(&requests);
    let state = Arc::new(Mutex::new(AscMockState {
        bundle_id_created: preseed_bundle_id,
        app_created: false,
        certificate_der: None,
        certificate_serial: None,
    }));
    let state_clone = Arc::clone(&state);
    let team_id = team_id.to_owned();
    let bundle_identifier = bundle_identifier.to_owned();
    let app_name = app_name.to_owned();

    let handle = thread::spawn(move || {
        let mut idle_polls = 0_u32;
        loop {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => {
                    idle_polls = 0;
                    connection
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if idle_polls > 500 {
                        break;
                    }
                    idle_polls += 1;
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(_) => break,
            };
            stream.set_nonblocking(false).unwrap();

            let request = read_http_request(&mut stream).unwrap();
            let first_line = request.lines().next().unwrap_or_default().to_owned();
            requests_clone.lock().unwrap().push(first_line.clone());

            let body = asc_response_body(
                &first_line,
                &request,
                &state_clone,
                &ca_root,
                &ca_key_path,
                &ca_cert_path,
                &team_id,
                &bundle_identifier,
                &app_name,
            );
            let (status, body) = match body {
                Ok(body) => ("200 OK", body),
                Err(message) => (
                    "404 Not Found",
                    serde_json::json!({
                        "errors": [{
                            "status": "404",
                            "code": "NOT_FOUND",
                            "title": "Not Found",
                            "detail": message
                        }]
                    })
                    .to_string(),
                ),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });

    AscMockServer {
        base_url,
        requests,
        handle: Some(handle),
    }
}

fn create_certificate_authority(root: &Path) -> (PathBuf, PathBuf) {
    let key_path = root.join("ca-key.pem");
    let cert_path = root.join("ca-cert.pem");
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
                "/CN=Orbit Mock CA",
            ])
            .status()
            .unwrap()
            .success()
    );
    (key_path, cert_path)
}

#[allow(clippy::too_many_arguments)]
fn asc_response_body(
    first_line: &str,
    request: &str,
    state: &Arc<Mutex<AscMockState>>,
    ca_root: &Path,
    ca_key_path: &Path,
    ca_cert_path: &Path,
    team_id: &str,
    bundle_identifier: &str,
    app_name: &str,
) -> Result<String, String> {
    if first_line.starts_with("GET /v1/bundleIds") {
        let state = state.lock().unwrap();
        let data = if state.bundle_id_created {
            vec![serde_json::json!({
                "id": "BUNDLE1",
                "type": "bundleIds",
                "attributes": {
                    "name": app_name,
                    "identifier": bundle_identifier,
                    "platform": "IOS"
                },
                "relationships": {}
            })]
        } else {
            Vec::new()
        };
        return Ok(serde_json::json!({ "data": data, "included": [] }).to_string());
    }

    if first_line.starts_with("POST /v1/bundleIds") {
        state.lock().unwrap().bundle_id_created = true;
        return Ok(serde_json::json!({
            "data": {
                "id": "BUNDLE1",
                "type": "bundleIds",
                "attributes": {
                    "name": app_name,
                    "identifier": bundle_identifier,
                    "platform": "IOS"
                },
                "relationships": {}
            }
        })
        .to_string());
    }

    if first_line.starts_with("GET /v1/certificates") {
        let state = state.lock().unwrap();
        let data = if let Some(certificate_der) = &state.certificate_der {
            vec![serde_json::json!({
                "id": "CERT1",
                "type": "certificates",
                "attributes": {
                    "certificateType": "IOS_DISTRIBUTION",
                    "displayName": "Orbit Mock Distribution",
                    "serialNumber": state.certificate_serial,
                    "certificateContent": certificate_der
                },
                "relationships": {}
            })]
        } else {
            Vec::new()
        };
        return Ok(serde_json::json!({ "data": data, "included": [] }).to_string());
    }

    if first_line.starts_with("POST /v1/certificates") {
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .ok_or_else(|| "missing request body".to_owned())?;
        let json: serde_json::Value =
            serde_json::from_str(body).map_err(|error| error.to_string())?;
        let csr_content = json["data"]["attributes"]["csrContent"]
            .as_str()
            .ok_or_else(|| "missing csrContent".to_owned())?;
        let certificate_der = sign_csr(ca_root, ca_key_path, ca_cert_path, csr_content)?;
        let certificate_serial = read_der_serial(&certificate_der)?;
        let certificate_der = base64::engine::general_purpose::STANDARD.encode(certificate_der);
        let mut state = state.lock().unwrap();
        state.certificate_der = Some(certificate_der.clone());
        state.certificate_serial = Some(certificate_serial.clone());
        return Ok(serde_json::json!({
            "data": {
                "id": "CERT1",
                "type": "certificates",
                "attributes": {
                    "certificateType": "IOS_DISTRIBUTION",
                    "displayName": "Orbit Mock Distribution",
                    "serialNumber": certificate_serial,
                    "certificateContent": certificate_der
                },
                "relationships": {}
            }
        })
        .to_string());
    }

    if first_line.starts_with("GET /v1/profiles") {
        return Ok(serde_json::json!({ "data": [], "included": [] }).to_string());
    }

    if first_line.starts_with("POST /v1/profiles") {
        let profile_content = base64::engine::general_purpose::STANDARD
            .encode(provisioning_profile_xml(team_id, bundle_identifier).as_bytes());
        return Ok(serde_json::json!({
            "data": {
                "id": "PROFILE1",
                "type": "profiles",
                "attributes": {
                    "name": "Orbit Mock Profile",
                    "profileType": "IOS_APP_STORE",
                    "profileState": "ACTIVE",
                    "profileContent": profile_content,
                    "uuid": "UUID-PROFILE-1"
                },
                "relationships": {}
            }
        })
        .to_string());
    }

    if first_line.starts_with("GET /v1/apps") {
        let state = state.lock().unwrap();
        let data = if state.app_created {
            vec![serde_json::json!({
                "id": "APP1",
                "type": "apps",
                "attributes": {
                    "name": app_name,
                    "sku": "DEV-ORBIT-FIXTURE",
                    "primaryLocale": "en-US"
                },
                "relationships": {}
            })]
        } else {
            Vec::new()
        };
        return Ok(serde_json::json!({ "data": data, "included": [] }).to_string());
    }

    if first_line.starts_with("POST /v1/apps") {
        state.lock().unwrap().app_created = true;
        return Ok(serde_json::json!({
            "data": {
                "id": "APP1",
                "type": "apps",
                "attributes": {
                    "name": app_name,
                    "sku": "DEV-ORBIT-FIXTURE",
                    "primaryLocale": "en-US"
                },
                "relationships": {}
            }
        })
        .to_string());
    }

    Err(format!("unexpected request `{first_line}`"))
}

fn sign_csr(
    root: &Path,
    ca_key_path: &Path,
    ca_cert_path: &Path,
    csr_content: &str,
) -> Result<Vec<u8>, String> {
    let csr_path = root.join("request.csr.pem");
    let certificate_path = root.join("signed.cer");
    fs::write(&csr_path, csr_content).map_err(|error| error.to_string())?;

    let mut command = Command::new("openssl");
    command.args([
        "x509",
        "-req",
        "-in",
        csr_path.to_str().unwrap(),
        "-CA",
        ca_cert_path.to_str().unwrap(),
        "-CAkey",
        ca_key_path.to_str().unwrap(),
        "-CAcreateserial",
        "-out",
        certificate_path.to_str().unwrap(),
        "-outform",
        "DER",
        "-days",
        "365",
    ]);
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    fs::read(&certificate_path).map_err(|error| error.to_string())
}

fn read_der_serial(certificate_der: &[u8]) -> Result<String, String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let certificate_path = temp.path().join("certificate.der");
    fs::write(&certificate_path, certificate_der).map_err(|error| error.to_string())?;
    let output = Command::new("openssl")
        .args([
            "x509",
            "-inform",
            "DER",
            "-in",
            certificate_path.to_str().unwrap(),
            "-serial",
            "-noout",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let line = String::from_utf8_lossy(&output.stdout);
    Ok(line
        .trim()
        .strip_prefix("serial=")
        .unwrap_or(line.trim())
        .to_owned())
}

fn provisioning_profile_xml(team_id: &str, bundle_identifier: &str) -> String {
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
    <string>{team_id}.{bundle_identifier}</string>
    <key>com.apple.developer.team-identifier</key>
    <string>{team_id}</string>
    <key>get-task-allow</key>
    <false/>
    <key>keychain-access-groups</key>
    <array>
      <string>{team_id}.{bundle_identifier}</string>
    </array>
  </dict>
</dict>
</plist>
"#
    )
}

pub(crate) fn read_http_request(stream: &mut impl Read) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];

    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(headers_end) = headers_end(&buffer) {
            let body_length = content_length(&buffer[..headers_end]);
            while buffer.len() < headers_end + body_length {
                let read = stream.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            break;
        }
    }

    Ok(String::from_utf8_lossy(&buffer).to_string())
}

fn headers_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                return value.trim().parse::<usize>().ok();
            }
            None
        })
        .unwrap_or(0)
}
