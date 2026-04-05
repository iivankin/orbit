use std::fs;
use std::path::{Path, PathBuf};

use super::write_executable;

pub fn format_failure_output(stderr: &str) -> String {
    let Some(report_path) = stderr.split("see ").nth(1).map(str::trim) else {
        return stderr.to_owned();
    };
    match fs::read_to_string(report_path) {
        Ok(report) => format!("{stderr}\nreport:\n{report}"),
        Err(_) => stderr.to_owned(),
    }
}

pub fn latest_ui_report_path(root: &Path) -> PathBuf {
    let mut runs = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    runs.sort();
    runs.pop().unwrap().join("report.json")
}

pub fn set_manifest_platforms(path: &Path, platforms: serde_json::Value) {
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    manifest["platforms"] = platforms;
    fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}

pub fn create_runtime_installing_xcrun_mock(mock_bin: &Path, runtime_ready_flag: &Path) {
    write_executable(
        &mock_bin.join("xcrun"),
        &format!(
            r#"#!/bin/sh
set -eu
if [ -n "${{DEVELOPER_DIR:-}}" ]; then
  echo "DEVELOPER_DIR=$DEVELOPER_DIR" >> "$MOCK_LOG"
fi
echo "xcrun $@" >> "$MOCK_LOG"
if [ "$#" -ge 3 ] && [ "$1" = "simctl" ] && [ "$2" = "list" ] && [ "$3" = "devices" ]; then
  if [ -f "{runtime_ready_flag}" ]; then
    cat <<'JSON'
{{"devices":{{"com.apple.CoreSimulator.SimRuntime.iOS-18-0":[{{"udid":"IOS-UDID","name":"iPhone 16","state":"Shutdown"}}]}}}}
JSON
  else
    cat <<'JSON'
{{"devices":{{}}}}
JSON
  fi
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "runtime" ] && [ "$3" = "add" ]; then
  test -f "$4"
  touch "{runtime_ready_flag}"
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "boot" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "bootstatus" ]; then
  exit 0
fi
echo "unexpected xcrun command: $@" >&2
exit 1
"#,
            runtime_ready_flag = runtime_ready_flag.display(),
        ),
    );
}

pub fn create_runtime_download_xcodebuild_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("xcodebuild"),
        r#"#!/bin/sh
set -eu
if [ -n "${DEVELOPER_DIR:-}" ]; then
  echo "DEVELOPER_DIR=$DEVELOPER_DIR" >> "$MOCK_LOG"
fi
echo "xcodebuild $@" >> "$MOCK_LOG"
if [ "$1" = "-version" ]; then
  printf '%s\n' "Xcode 26.4"
  printf '%s\n' "Build version 17E192"
  exit 0
fi
if [ "$1" = "-downloadPlatform" ] && [ "$2" = "iOS" ] && [ "$3" = "-exportPath" ]; then
  mkdir -p "$4"
  printf 'dmg' > "$4/iOS_18.0_Simulator_Runtime.dmg"
  exit 0
fi
echo "unexpected xcodebuild command: $@" >&2
exit 1
"#,
    );
}

pub fn create_fake_xcode_bundle(root: &Path, name: &str, version: &str, build: &str) -> PathBuf {
    let app_root = root.join(name);
    let contents = app_root.join("Contents");
    let developer_dir = contents.join("Developer");
    fs::create_dir_all(&developer_dir).unwrap();
    fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>com.apple.dt.Xcode</string>
  <key>CFBundleShortVersionString</key>
  <string>{version}</string>
  <key>ProductBuildVersion</key>
  <string>{build}</string>
</dict>
</plist>
"#
        ),
    )
    .unwrap();
    app_root
}

pub fn set_manifest_xcode(path: &Path, version: &str) {
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    manifest["xcode"] = serde_json::Value::String(version.to_owned());
    fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}
