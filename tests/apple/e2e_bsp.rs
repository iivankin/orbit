use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::Stdio;

use reqwest::Url;
use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use crate::support::create_mixed_language_workspace;
use crate::support::{
    base_command, create_build_xcrun_mock, create_home, create_signing_workspace,
    create_xcframework_workspace, orbit_bin, read_log, run_and_capture,
};

#[test]
fn ide_install_build_server_writes_standard_connection_file() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "ide",
        "install-build-server",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let standard_path = workspace.join(".bsp/orbit.json");
    let standard_bytes = fs::read(&standard_path).unwrap();
    assert!(!workspace.join("buildServer.json").exists());

    let manifest_path = workspace.join("orbit.json").canonicalize().unwrap();
    let details: Value = serde_json::from_slice(&standard_bytes).unwrap();
    assert_eq!(details["name"], "orbit");
    assert_eq!(details["bspVersion"], "2.2.0");
    assert_eq!(
        details["languages"],
        json!(["swift", "c", "objective-c", "cpp", "objective-cpp"])
    );
    assert_eq!(
        details["argv"],
        json!([orbit_bin(), "--manifest", manifest_path, "bsp"])
    );
}

#[cfg(target_os = "macos")]
#[test]
fn bsp_server_serves_targets_sources_and_sourcekit_options() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_mixed_language_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let manifest_path = workspace.join("orbit.json");
    let source_path = workspace
        .join("Sources/App/App.swift")
        .canonicalize()
        .unwrap();
    let source_uri = Url::from_file_path(&source_path).unwrap().to_string();
    let output_path = format!("{}.o", source_path.display());
    let objc_source_path = workspace
        .join("Sources/App/Bridge.m")
        .canonicalize()
        .unwrap();
    let objc_source_uri = Url::from_file_path(&objc_source_path).unwrap().to_string();
    let header_path = workspace
        .join("Sources/App/Bridge.h")
        .canonicalize()
        .unwrap();
    let header_uri = Url::from_file_path(&header_path).unwrap().to_string();

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "bsp",
    ]);

    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stdout);

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "build/initialize",
            "params": {
                "displayName": "orbit-test",
                "version": "0.0.0",
                "bspVersion": "2.2.0",
                "rootUri": Url::from_file_path(&workspace).unwrap().to_string(),
                "capabilities": {}
            }
        }),
    );
    let initialize = read_jsonrpc_message(&mut reader);
    assert_eq!(initialize["id"], 1);
    assert_eq!(initialize["result"]["dataKind"], "sourceKit");
    assert!(
        initialize["result"]["data"]["indexDatabasePath"]
            .as_str()
            .unwrap()
            .ends_with(".orbit/ide/index/db")
    );
    assert!(
        initialize["result"]["data"]["indexStorePath"]
            .as_str()
            .unwrap()
            .ends_with(".orbit/ide/index/store")
    );
    assert_eq!(
        initialize["result"]["data"]["sourceKitOptionsProvider"],
        Value::Bool(true)
    );
    assert_eq!(
        initialize["result"]["data"]["prepareProvider"],
        Value::Bool(true)
    );
    assert_eq!(
        initialize["result"]["data"]["outputPathsProvider"],
        Value::Bool(true)
    );
    assert_eq!(
        initialize["result"]["capabilities"]["canReload"],
        Value::Bool(true)
    );
    assert_eq!(
        initialize["result"]["capabilities"]["inverseSourcesProvider"],
        Value::Bool(true)
    );
    assert_eq!(
        initialize["result"]["data"]["watchers"][0]["globPattern"],
        "orbit.json"
    );
    let watchers = initialize["result"]["data"]["watchers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|watcher| watcher["globPattern"].as_str())
        .collect::<Vec<_>>();
    assert!(watchers.contains(&"**/*.swift"));
    assert!(watchers.contains(&"**/*.xcframework/**"));

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "build/initialized",
            "params": {}
        }),
    );

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/buildTargets",
            "params": {}
        }),
    );
    let targets = read_jsonrpc_message(&mut reader);
    let target = &targets["result"]["targets"][0];
    assert_eq!(target["displayName"], "ExampleApp");
    assert_eq!(target["dataKind"], "sourceKit");
    assert_eq!(target["languageIds"], json!(["objective-c", "swift"]));
    assert!(
        target["data"]["toolchain"]
            .as_str()
            .unwrap()
            .contains("OrbitDefault.xctoolchain")
    );
    let target_uri = target["id"]["uri"].as_str().unwrap().to_owned();

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "buildTarget/sources",
            "params": {
                "targets": [{ "uri": target_uri }]
            }
        }),
    );
    let sources = read_jsonrpc_message(&mut reader);
    assert_eq!(sources["id"], 3);
    let source_entries = sources["result"]["items"][0]["sources"].as_array().unwrap();
    assert!(source_entries.iter().any(|source| {
        source["uri"] == Value::String(source_uri.clone())
            && source["data"]["language"] == Value::String("swift".to_owned())
            && source["data"]["outputPath"] == Value::String(output_path.clone())
    }));
    assert!(source_entries.iter().any(|source| {
        source["uri"] == Value::String(objc_source_uri.clone())
            && source["data"]["language"] == Value::String("objective-c".to_owned())
            && source["data"]["outputPath"]
                .as_str()
                .is_some_and(|value| value.ends_with("Bridge.m.o"))
    }));
    assert!(source_entries.iter().any(|source| {
        source["uri"] == Value::String(header_uri.clone())
            && source["data"]["kind"] == Value::String("header".to_owned())
            && source["data"].get("outputPath").is_none()
    }));

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "buildTarget/prepare",
            "params": {
                "targets": [{ "uri": target_uri }]
            }
        }),
    );
    let (prepare_notifications, prepare) = read_jsonrpc_messages_until_response(&mut reader, 4);
    assert_eq!(prepare["id"], 4);
    assert!(prepare_notifications.iter().any(|message| {
        message["method"] == Value::String("build/logMessage".to_owned())
            && message["params"]["message"]
                .as_str()
                .is_some_and(|value| value.contains("Preparing"))
    }));
    assert!(prepare_notifications.iter().any(|message| {
        message["method"] == Value::String("build/taskStart".to_owned())
            && message["params"]["message"]
                .as_str()
                .is_some_and(|value| value.contains("Preparing"))
    }));
    assert!(prepare_notifications.iter().any(|message| {
        message["method"] == Value::String("build/taskProgress".to_owned())
            && message["params"]["message"]
                .as_str()
                .is_some_and(|value| value.contains("Prepared"))
            && message["params"]["unit"] == Value::String("targets".to_owned())
            && message["params"]["progress"]
                .as_u64()
                .zip(message["params"]["total"].as_u64())
                .is_some_and(|(progress, total)| progress == total)
    }));
    assert!(prepare_notifications.iter().any(|message| {
        message["method"] == Value::String("build/taskFinish".to_owned())
            && message["params"]["status"] == 1
    }));

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 41,
            "method": "buildTarget/outputPaths",
            "params": {
                "targets": [{ "uri": target_uri }]
            }
        }),
    );
    let output_paths = read_jsonrpc_message(&mut reader);
    let output_root = output_paths["result"]["items"][0]["outputPaths"][0]["uri"]
        .as_str()
        .unwrap();
    assert!(output_root.contains(".orbit/ide/build/ios/ide/simulator/ExampleApp"));

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "textDocument/sourceKitOptions",
            "params": {
                "target": { "uri": target_uri },
                "textDocument": { "uri": source_uri },
                "language": "swift"
            }
        }),
    );
    let options = read_jsonrpc_message(&mut reader);
    let compiler_arguments = options["result"]["compilerArguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(compiler_arguments.iter().any(|value| value == "swiftc"));
    assert!(compiler_arguments.iter().any(|value| value == "-sdk"));
    assert!(
        compiler_arguments
            .iter()
            .any(|value| value == "-index-store-path")
    );
    assert!(
        compiler_arguments
            .windows(2)
            .any(|pair| pair[0] == "-index-unit-output-path" && pair[1] == output_path)
    );
    assert!(
        compiler_arguments
            .iter()
            .any(|value| value.ends_with("Sources/App/App.swift"))
    );

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 51,
            "method": "textDocument/sourceKitOptions",
            "params": {
                "target": { "uri": target_uri },
                "textDocument": { "uri": objc_source_uri },
                "language": "objective-c"
            }
        }),
    );
    let objc_options = read_jsonrpc_message(&mut reader);
    let objc_compiler_arguments = objc_options["result"]["compilerArguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(objc_compiler_arguments.iter().any(|value| value == "clang"));
    assert!(
        objc_compiler_arguments
            .iter()
            .any(|value| value == "-index-store-path")
    );
    assert!(
        objc_compiler_arguments
            .windows(2)
            .any(|pair| pair[0] == "-o" && pair[1].ends_with("Bridge.m.o"))
    );
    assert!(
        objc_compiler_arguments
            .iter()
            .any(|value| value.ends_with("Sources/App/Bridge.m"))
    );
    assert!(
        !objc_compiler_arguments
            .iter()
            .any(|value| value == "-index-unit-output-path")
    );

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 511,
            "method": "buildTarget/inverseSources",
            "params": {
                "textDocument": { "uri": header_uri }
            }
        }),
    );
    let inverse_sources = read_jsonrpc_message(&mut reader);
    assert_eq!(
        inverse_sources["result"]["targets"],
        json!([{ "uri": target_uri.clone() }])
    );

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 512,
            "method": "textDocument/sourceKitOptions",
            "params": {
                "target": { "uri": target_uri },
                "textDocument": { "uri": header_uri },
                "language": "objective-c"
            }
        }),
    );
    let header_options = read_jsonrpc_message(&mut reader);
    let header_compiler_arguments = header_options["result"]["compilerArguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        header_compiler_arguments
            .iter()
            .any(|value| value == "clang")
    );
    assert!(
        header_compiler_arguments
            .iter()
            .any(|value| value == "-xobjective-c")
    );
    assert!(
        header_compiler_arguments
            .iter()
            .any(|value| value.ends_with("Sources/App/Bridge.h"))
    );
    assert!(
        !header_compiler_arguments
            .iter()
            .any(|value| value == "-index-unit-output-path")
    );

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 52,
            "method": "workspace/reload",
            "params": {}
        }),
    );
    let (reload_notifications, reload) = read_jsonrpc_messages_until_response(&mut reader, 52);
    assert_eq!(reload["id"], 52);
    assert!(reload_notifications.iter().any(|message| {
        message["method"] == Value::String("build/taskStart".to_owned())
            && message["params"]["message"]
                .as_str()
                .is_some_and(|value| value.contains("Reloading"))
    }));
    assert!(reload_notifications.iter().any(|message| {
        message["method"] == Value::String("build/taskFinish".to_owned())
            && message["params"]["status"] == 1
    }));
    let log_before_watch = read_log(&log_path);
    let sdk_path_requests_before_watch = count_occurrences(
        &log_before_watch,
        "xcrun --sdk iphonesimulator --show-sdk-path",
    );

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeWatchedFiles",
            "params": {
                "changes": [{
                    "uri": header_uri,
                    "type": 2
                }]
            }
        }),
    );
    let watched_file_notifications = read_jsonrpc_notifications(&mut reader, 2);
    assert!(watched_file_notifications.iter().any(|message| {
        message["method"] == Value::String("build/logMessage".to_owned())
            && message["params"]["message"]
                .as_str()
                .is_some_and(|value| value.contains("without rebuilding the semantic snapshot"))
    }));
    assert!(watched_file_notifications.iter().any(|message| {
        message["method"] == Value::String("buildTarget/didChange".to_owned())
            && message["params"]["changes"]
                == json!([{
                    "target": { "uri": target_uri.clone() },
                    "kind": 2
                }])
    }));

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "build/shutdown",
            "params": {}
        }),
    );
    let shutdown = read_jsonrpc_message(&mut reader);
    assert_eq!(shutdown["id"], 6);

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "build/exit",
            "params": {}
        }),
    );

    drop(stdin);
    let status = child.wait().unwrap();
    let mut stderr_output = String::new();
    stderr.read_to_string(&mut stderr_output).unwrap();
    assert!(status.success(), "{stderr_output}");

    let log = read_log(&log_path);
    assert_eq!(
        count_occurrences(&log, "xcrun --sdk iphonesimulator --show-sdk-path"),
        sdk_path_requests_before_watch
    );
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
    assert!(log.contains("xcrun --find swiftc"));
    assert!(log.contains("xcrun --sdk iphonesimulator swiftc"));
    assert!(log.contains("xcrun --sdk iphonesimulator clang"));
}

#[test]
fn bsp_server_reloads_for_xcframework_dependency_changes() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_xcframework_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let manifest_path = workspace.join("orbit.json");
    let info_plist_path = workspace
        .join("Vendor/VendorSDK.xcframework/Info.plist")
        .canonicalize()
        .unwrap();
    let info_plist_uri = Url::from_file_path(&info_plist_path).unwrap().to_string();

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "bsp",
    ]);

    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stdout);

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "build/initialize",
            "params": {
                "displayName": "orbit-test",
                "version": "0.0.0",
                "bspVersion": "2.2.0",
                "rootUri": Url::from_file_path(&workspace).unwrap().to_string(),
                "capabilities": {}
            }
        }),
    );
    let initialize = read_jsonrpc_message(&mut reader);
    assert_eq!(initialize["id"], 1);

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "build/initialized",
            "params": {}
        }),
    );

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/buildTargets",
            "params": {}
        }),
    );
    let targets = read_jsonrpc_message(&mut reader);
    let target_uri = targets["result"]["targets"][0]["id"]["uri"]
        .as_str()
        .unwrap()
        .to_owned();

    let log_before_watch = read_log(&log_path);
    let sdk_path_requests_before_watch = count_occurrences(
        &log_before_watch,
        "xcrun --sdk iphonesimulator --show-sdk-path",
    );

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeWatchedFiles",
            "params": {
                "changes": [{
                    "uri": info_plist_uri,
                    "type": 2
                }]
            }
        }),
    );
    let watched_file_notifications = read_jsonrpc_notifications(&mut reader, 2);
    assert!(watched_file_notifications.iter().any(|message| {
        message["method"] == Value::String("build/logMessage".to_owned())
            && message["params"]["message"].as_str().is_some_and(|value| {
                value.contains("reloaded build settings after watched file changes")
            })
    }));
    assert!(watched_file_notifications.iter().any(|message| {
        message["method"] == Value::String("buildTarget/didChange".to_owned())
            && message["params"]["changes"]
                == json!([{
                    "target": { "uri": target_uri.clone() },
                    "kind": 2
                }])
    }));

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "build/shutdown",
            "params": {}
        }),
    );
    let shutdown = read_jsonrpc_message(&mut reader);
    assert_eq!(shutdown["id"], 3);

    write_jsonrpc_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "build/exit",
            "params": {}
        }),
    );

    drop(stdin);
    let status = child.wait().unwrap();
    let mut stderr_output = String::new();
    stderr.read_to_string(&mut stderr_output).unwrap();
    assert!(status.success(), "{stderr_output}");

    let log = read_log(&log_path);
    assert!(
        count_occurrences(&log, "xcrun --sdk iphonesimulator --show-sdk-path")
            > sdk_path_requests_before_watch
    );
}

fn write_jsonrpc_message<W: Write>(writer: &mut W, message: &Value) {
    let body = serde_json::to_vec(message).unwrap();
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    writer.write_all(&body).unwrap();
    writer.flush().unwrap();
}

fn read_jsonrpc_message<R: BufRead>(reader: &mut R) -> Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).unwrap();
        assert!(read > 0, "unexpected EOF while reading JSON-RPC headers");
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("Content-Length:")
        {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
    }

    let mut body = vec![0_u8; content_length.expect("missing Content-Length header")];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[cfg(target_os = "macos")]
fn read_jsonrpc_messages_until_response<R: BufRead>(
    reader: &mut R,
    response_id: i64,
) -> (Vec<Value>, Value) {
    let mut notifications = Vec::new();
    loop {
        let message = read_jsonrpc_message(reader);
        if message["id"] == response_id {
            return (notifications, message);
        }
        notifications.push(message);
    }
}

fn read_jsonrpc_notifications<R: BufRead>(reader: &mut R, count: usize) -> Vec<Value> {
    (0..count).map(|_| read_jsonrpc_message(reader)).collect()
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}
