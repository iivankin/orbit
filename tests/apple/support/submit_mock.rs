use std::collections::BTreeMap;
use std::io::Write;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct SubmitMockServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct SubmitMockState {
    build_get_count: usize,
    next_file_id: usize,
    files: BTreeMap<String, SubmitFile>,
}

struct SubmitFile {
    asset_type: String,
    file_name: String,
    file_size: u64,
    checksum: String,
    uti: String,
    uploaded: bool,
}

struct MockResponse {
    status: &'static str,
    body: String,
    headers: Vec<(&'static str, String)>,
}

impl SubmitMockServer {
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }
}

pub fn spawn_submit_mock(root: &std::path::Path, bundle_id: &str) -> SubmitMockServer {
    let _ = root;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new(Mutex::new(SubmitMockState::default()));
    let requests_clone = Arc::clone(&requests);
    let state_clone = Arc::clone(&state);
    let bundle_id = bundle_id.to_owned();
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

            let response = submit_response(
                &first_line,
                &request,
                &state_clone,
                &base_url_for_thread,
                &bundle_id,
            );
            let mut headers =
                String::from("Content-Type: application/json\r\nConnection: close\r\n");
            for (name, value) in response.headers {
                headers.push_str(name);
                headers.push_str(": ");
                headers.push_str(&value);
                headers.push_str("\r\n");
            }
            headers.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
            let http = format!(
                "HTTP/1.1 {}\r\n{}\r\n{}",
                response.status, headers, response.body
            );
            stream.write_all(http.as_bytes()).unwrap();
        }
    });

    SubmitMockServer {
        base_url,
        requests,
        handle: Some(handle),
    }
}

fn submit_response(
    first_line: &str,
    request: &str,
    state: &Arc<Mutex<SubmitMockState>>,
    base_url: &str,
    bundle_id: &str,
) -> MockResponse {
    if first_line.starts_with("POST /WebObjects/MZLabelService.woa/json/MZContentDeliveryService") {
        let body = request_body(request);
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        let method = json["method"].as_str().unwrap_or_default();
        let provider_name = json["params"]["ProviderName"].as_str();
        return match (method, provider_name) {
            ("authenticateUserWithArguments", _) => json_response(
                "200 OK",
                serde_json::json!({
                    "id": "orbit-auth-user",
                    "jsonrpc": "2.0",
                    "result": {
                        "Success": true,
                        "ProvidersByShortname": {
                            "TEAM123456": {
                                "ProviderName": "TEAM123456",
                                "ProviderPublicId": "provider-test",
                                "WWDRTeamID": "TEAM123456"
                            }
                        }
                    }
                }),
            ),
            ("authenticateForSession", None) => json_response(
                "200 OK",
                serde_json::json!({
                    "id": "orbit-auth-session",
                    "jsonrpc": "2.0",
                    "result": {
                        "Success": true,
                        "SessionId": "lookup-session",
                        "SharedSecret": "lookup-secret"
                    }
                }),
            ),
            ("authenticateForSession", Some("TEAM123456")) => json_response(
                "200 OK",
                serde_json::json!({
                    "id": "orbit-provider-session",
                    "jsonrpc": "2.0",
                    "result": {
                        "Success": true,
                        "SessionId": "upload-session",
                        "SharedSecret": "upload-secret"
                    }
                }),
            ),
            _ => json_response(
                "404 Not Found",
                serde_json::json!({
                    "errors": [{
                        "status": "404",
                        "detail": format!("unexpected content delivery auth request `{body}`")
                    }]
                }),
            ),
        };
    }

    if first_line.starts_with("POST /WebObjects/MZLabelService.woa/json/MZITunesProducerService") {
        let body = request_body(request);
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        let method = json["method"].as_str().unwrap_or_default();
        if method == "providersInfoWithArguments" {
            return json_response(
                "200 OK",
                serde_json::json!({
                    "id": "orbit-providers-info",
                    "jsonrpc": "2.0",
                    "result": {
                        "Success": true,
                        "ProvidersInfo": {
                            "TEAM123456": {
                                "ProviderName": "Example Team",
                                "ProviderShortname": "TEAM123456",
                                "PublicID": "provider-test",
                                "WWDRTeamID": "TEAM123456"
                            }
                        }
                    }
                }),
            );
        }
    }

    if first_line.starts_with("POST /WebObjects/MZLabelService.woa/json/MZITunesSoftwareService") {
        return json_response(
            "200 OK",
            serde_json::json!({
                "id": "orbit-lookup-software",
                "jsonrpc": "2.0",
                "result": {
                    "Success": true,
                    "Applications": {
                        bundle_id: "APP1"
                    },
                    "Attributes": [{
                        "Apple ID": "APP1"
                    }]
                }
            }),
        );
    }

    if first_line
        .starts_with("POST /MZContentDeliveryService/iris/provider/provider-test/v1/builds")
    {
        return MockResponse {
            status: "201 Created",
            body: serde_json::json!({
                "data": {
                    "id": "build-1",
                    "type": "builds",
                    "attributes": {
                        "uploadedDate": "2026-03-29T14:07:10.000Z",
                        "processingState": "PROCESSING",
                        "processingErrors": [],
                        "buildProcessingState": {
                            "state": "PROCESSING",
                            "errors": []
                        }
                    }
                }
            })
            .to_string(),
            headers: vec![("Set-Cookie", "dqsid=mock-dqsid; Path=/".to_owned())],
        };
    }

    if first_line.starts_with(
        "POST /MZContentDeliveryService/iris/provider/provider-test/v1/buildDeliveryFiles",
    ) {
        let body = request_body(request);
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        let attributes = &json["data"]["attributes"];
        let mut state = state.lock().unwrap();
        state.next_file_id += 1;
        let file_id = format!("file-{}", state.next_file_id);
        let file = SubmitFile {
            asset_type: attributes["assetType"].as_str().unwrap().to_owned(),
            file_name: attributes["fileName"].as_str().unwrap().to_owned(),
            file_size: attributes["fileSize"].as_u64().unwrap(),
            checksum: attributes["sourceFileChecksum"]
                .as_str()
                .unwrap()
                .to_owned(),
            uti: attributes["uti"].as_str().unwrap().to_owned(),
            uploaded: false,
        };
        state.files.insert(file_id.clone(), file);
        let upload_url = format!("{base_url}/upload/{file_id}");
        return json_response(
            "201 Created",
            serde_json::json!({
                "data": {
                    "id": file_id,
                    "type": "buildDeliveryFiles",
                    "attributes": {
                        "assetType": attributes["assetType"],
                        "assetDeliveryState": {
                            "state": "AWAITING_UPLOAD",
                            "errors": [],
                            "warnings": []
                        },
                        "uploadOperations": [{
                            "method": "PUT",
                            "url": upload_url,
                            "length": attributes["fileSize"],
                            "requestHeaders": [{
                                "name": "x-upload-token",
                                "value": "mock"
                            }]
                        }]
                    }
                }
            }),
        );
    }

    if first_line.starts_with("PUT /upload/") {
        return MockResponse {
            status: "200 OK",
            body: String::new(),
            headers: Vec::new(),
        };
    }

    if first_line.starts_with(
        "PATCH /MZContentDeliveryService/iris/provider/provider-test/v1/buildDeliveryFiles/",
    ) {
        let file_id = first_line
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.rsplit('/').next())
            .unwrap();
        let mut state = state.lock().unwrap();
        let file = state.files.get_mut(file_id).unwrap();
        file.uploaded = true;
        return json_response("200 OK", build_file_document(file_id, file));
    }

    if first_line.starts_with(
        "GET /MZContentDeliveryService/iris/provider/provider-test/v1/buildDeliveryFiles/",
    ) {
        let file_id = first_line
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.rsplit('/').next())
            .unwrap();
        let state = state.lock().unwrap();
        let file = state.files.get(file_id).unwrap();
        return json_response("200 OK", build_file_document(file_id, file));
    }

    if first_line
        .starts_with("GET /MZContentDeliveryService/iris/provider/provider-test/v1/builds/")
    {
        let mut state = state.lock().unwrap();
        state.build_get_count += 1;
        let processing_state = if state.build_get_count > 1 {
            "COMPLETE"
        } else {
            "PROCESSING"
        };
        return json_response(
            "200 OK",
            serde_json::json!({
                "data": {
                    "id": "build-1",
                    "type": "builds",
                    "attributes": {
                        "uploadedDate": "2026-03-29T14:07:10.000Z",
                        "processingState": processing_state,
                        "processingErrors": [],
                        "buildProcessingState": {
                            "state": processing_state,
                            "errors": []
                        }
                    }
                }
            }),
        );
    }

    if first_line.starts_with(
        "POST /MZContentDeliveryService/iris/provider/provider-test/v1/metricsAndLogging",
    ) {
        return json_response(
            "201 Created",
            serde_json::json!({
                "data": {
                    "id": "metric-1",
                    "type": "metricsAndLogging"
                }
            }),
        );
    }

    json_response(
        "404 Not Found",
        serde_json::json!({
            "errors": [{
                "status": "404",
                "detail": format!("unexpected submit mock request `{first_line}`")
            }]
        }),
    )
}

fn build_file_document(file_id: &str, file: &SubmitFile) -> serde_json::Value {
    let state = if file.uploaded {
        "COMPLETE"
    } else {
        "AWAITING_UPLOAD"
    };
    serde_json::json!({
        "data": {
            "id": file_id,
            "type": "buildDeliveryFiles",
            "attributes": {
                "assetType": file.asset_type,
                "fileName": file.file_name,
                "fileSize": file.file_size,
                "sourceFileChecksum": file.checksum,
                "uti": file.uti,
                "assetDeliveryState": {
                    "state": state,
                    "errors": [],
                    "warnings": []
                },
                "uploadOperations": serde_json::Value::Null
            }
        }
    })
}

fn json_response(status: &'static str, body: serde_json::Value) -> MockResponse {
    MockResponse {
        status,
        body: body.to_string(),
        headers: Vec::new(),
    }
}

fn request_body(request: &str) -> &str {
    request.split("\r\n\r\n").nth(1).unwrap_or_default()
}
