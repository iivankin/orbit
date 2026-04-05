use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct NotaryMockServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct NotaryMockState {
    status_get_count: usize,
}

impl NotaryMockServer {
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }
}

pub fn spawn_notary_mock() -> NotaryMockServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new(Mutex::new(NotaryMockState::default()));
    let requests_clone = Arc::clone(&requests);
    let state_clone = Arc::clone(&state);
    let base_url_for_thread = base_url.clone();

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

            let request = super::read_http_request(&mut stream).unwrap();
            let first_line = request.lines().next().unwrap_or_default().to_owned();
            requests_clone.lock().unwrap().push(first_line.clone());

            let response = notary_response(&first_line, &base_url_for_thread, &state_clone);
            let headers = format!(
                "Content-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                response.len()
            );
            let http = format!("HTTP/1.1 200 OK\r\n{}{response}", headers);
            stream.write_all(http.as_bytes()).unwrap();
        }
    });

    NotaryMockServer {
        base_url,
        requests,
        handle: Some(handle),
    }
}

pub fn write_xcode_notary_auth_fixture(path: &Path, team_id: &str) -> PathBuf {
    let fixture_path = path.join("xcode-notary-auth.json");
    fs::write(
        &fixture_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "team_id": team_id,
            "gs_token": "xcode-notary-token",
            "identity_id": "000336-08-fixture",
            "device_id": "DEVICE-ID-FIXTURE",
            "locale": "en_US",
            "time_zone": "GMT+3",
            "md_lu": "fixture-md-lu",
            "md": "fixture-md",
            "md_m": "fixture-md-m",
            "md_rinfo": "33883392",
            "authkit_client_info": "<Mac16,8> <macOS;26.4;25E246> <com.apple.AuthKit/1 (com.apple.dt.Xcode/24909)>",
            "notary_client_info": "<Mac16,8> <macOS;26.4;25E246> <com.apple.AuthKit/1 (com.apple.dt.Xcode.ITunesSoftwareService/24856)>",
            "authkit_user_agent": "Xcode/24909 CFNetwork/3860.500.112 Darwin/25.4.0",
            "xcode_version_header": "26.4 (17E192)"
        }))
        .unwrap(),
    )
    .unwrap();
    fixture_path
}

fn notary_response(
    first_line: &str,
    base_url: &str,
    state: &Arc<Mutex<NotaryMockState>>,
) -> String {
    if first_line.starts_with("POST /ci/auth/auth/authkit") {
        return serde_json::json!({
            "jwt": "fixture-jwt",
            "expires_at": "2099-01-01T00:00:00Z"
        })
        .to_string();
    }

    if first_line.starts_with("POST /notary/v2/submissions") {
        return serde_json::json!({
            "data": {
                "type": "newSubmissions",
                "id": "submission-1",
                "attributes": {
                    "awsAccessKeyId": "ASIAXCODEFIXTURE",
                    "awsSecretAccessKey": "secret",
                    "awsSessionToken": "session",
                    "bucket": "ignored-bucket",
                    "object": "uploads/submission-1.zip"
                }
            },
            "meta": {}
        })
        .to_string();
    }

    if first_line.starts_with("PUT /uploads/submission-1.zip") {
        return String::new();
    }

    if first_line.starts_with("GET /notary/v2/submissions/submission-1/logs") {
        return serde_json::json!({
            "data": {
                "id": "submission-1",
                "type": "submissionsLog",
                "attributes": {
                    "developerLogUrl": format!("{base_url}/logs/submission-1.json")
                }
            },
            "meta": {}
        })
        .to_string();
    }

    if first_line.starts_with("GET /logs/submission-1.json") {
        return serde_json::json!({
            "issues": [],
            "status": "Accepted"
        })
        .to_string();
    }

    if first_line.starts_with("GET /notary/v2/submissions/submission-1") {
        let status = {
            let mut state = state.lock().unwrap();
            state.status_get_count += 1;
            if state.status_get_count > 1 {
                "Accepted"
            } else {
                "In Progress"
            }
        };
        return serde_json::json!({
            "data": {
                "id": "submission-1",
                "type": "submissions",
                "attributes": {
                    "status": status,
                    "name": "ExampleApp-DeveloperId.pkg.zip",
                    "createdDate": "2026-03-29T20:02:16.337Z"
                }
            },
            "meta": {}
        })
        .to_string();
    }

    serde_json::json!({
        "error": {
            "detail": format!("unexpected notary mock request `{first_line}`")
        }
    })
    .to_string()
}
