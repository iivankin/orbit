use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;
use tempfile::tempdir;

use super::backend::UiBackend;
use super::flow::select_ui_flow_paths;
use super::{
    UiCommand, UiCrashDeleteRequest, UiCrashQuery, UiFlowRunner, UiHardwareButton, UiKeyModifier,
    UiKeyPress, UiPermissionConfig, UiSelector, UiSwipeDirection, UiTravel,
    cleanup_macos_trace_temp_dir, find_element_by_selector, find_visible_element_by_selector,
    find_visible_scroll_container, infer_screen_frame, ui_testing_destination,
};
use crate::apple::build::toolchain::DestinationKind;
use crate::manifest::ApplePlatform;

type LaunchRecords = Arc<Mutex<Vec<Vec<(String, String)>>>>;

#[derive(Default)]
struct TestUiBackend {
    launches: LaunchRecords,
    selector_taps: Arc<Mutex<Vec<UiSelector>>>,
    focus_calls: Arc<Mutex<u32>>,
}

impl UiBackend for TestUiBackend {
    fn backend_name(&self) -> &'static str {
        "test"
    }

    fn target_name(&self) -> &str {
        "test-target"
    }

    fn target_id(&self) -> &str {
        "test-target"
    }

    fn describe_all(&self) -> Result<serde_json::Value> {
        Ok(json!([]))
    }

    fn describe_point(&self, _x: f64, _y: f64) -> Result<serde_json::Value> {
        Ok(json!({}))
    }

    fn launch_app(
        &self,
        _bundle_id: &str,
        _stop_app: bool,
        arguments: &[(String, String)],
    ) -> Result<()> {
        self.launches.lock().unwrap().push(arguments.to_vec());
        Ok(())
    }

    fn stop_app(&self, _bundle_id: &str) -> Result<()> {
        Ok(())
    }

    fn clear_app_state(&self, _bundle_id: &str) -> Result<()> {
        Ok(())
    }

    fn focus(&self) -> Result<()> {
        *self.focus_calls.lock().unwrap() += 1;
        Ok(())
    }

    fn abort_pending_trace_launch(&self) -> Result<()> {
        Ok(())
    }

    fn tap_point(&self, _x: f64, _y: f64, _duration_ms: Option<u32>) -> Result<()> {
        Ok(())
    }

    fn activate_selector(&self, selector: &UiSelector) -> Result<bool> {
        self.selector_taps.lock().unwrap().push(selector.clone());
        Ok(true)
    }

    fn swipe_points(
        &self,
        _start: (f64, f64),
        _end: (f64, f64),
        _duration_ms: Option<u32>,
        _delta: Option<u32>,
    ) -> Result<()> {
        Ok(())
    }

    fn input_text(&self, _text: &str) -> Result<()> {
        Ok(())
    }

    fn press_button(&self, _button: UiHardwareButton, _duration_ms: Option<u32>) -> Result<()> {
        Ok(())
    }

    fn press_key(&self, _key: &UiKeyPress) -> Result<()> {
        Ok(())
    }

    fn press_key_code(
        &self,
        _keycode: u32,
        _duration_ms: Option<u32>,
        _modifiers: &[UiKeyModifier],
    ) -> Result<()> {
        Ok(())
    }

    fn press_key_sequence(&self, _keycodes: &[u32]) -> Result<()> {
        Ok(())
    }

    fn take_screenshot(&self, path: &Path) -> Result<()> {
        fs::write(path, b"png")?;
        Ok(())
    }

    fn open_link(&self, _url: &str) -> Result<()> {
        Ok(())
    }

    fn clear_keychain(&self) -> Result<()> {
        Ok(())
    }

    fn set_location(&self, _latitude: f64, _longitude: f64) -> Result<()> {
        Ok(())
    }

    fn set_permissions(&self, _bundle_id: &str, _config: &UiPermissionConfig) -> Result<()> {
        Ok(())
    }

    fn travel(&self, _command: &UiTravel) -> Result<()> {
        Ok(())
    }

    fn add_media(&self, _paths: &[PathBuf]) -> Result<()> {
        Ok(())
    }

    fn install_dylib(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    fn run_instruments(&self, _template: &str, _arguments: &[String]) -> Result<()> {
        Ok(())
    }

    fn update_contacts(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    fn list_crash_logs(&self, _query: &UiCrashQuery) -> Result<()> {
        Ok(())
    }

    fn show_crash_log(&self, _name: &str) -> Result<()> {
        Ok(())
    }

    fn delete_crash_logs(&self, _request: &UiCrashDeleteRequest) -> Result<()> {
        Ok(())
    }

    fn stream_logs(&self, _arguments: &[String]) -> Result<()> {
        Ok(())
    }

    fn scroll_in_direction(&self, _direction: UiSwipeDirection) -> Result<()> {
        Ok(())
    }

    fn scroll_at_point(&self, _direction: UiSwipeDirection, _point: (f64, f64)) -> Result<()> {
        Ok(())
    }

    fn hide_keyboard(&self) -> Result<()> {
        Ok(())
    }

    fn start_video_recording(&mut self, path: &Path) -> Result<()> {
        fs::write(path, b"video")?;
        Ok(())
    }

    fn stop_video_recording(&mut self) -> Result<()> {
        Ok(())
    }
}

fn write_flow(root: &Path, name: &str, contents: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn finds_best_matching_element_by_accessibility_text() {
    let tree = json!([
        {
            "AXLabel": "Continue Later",
            "frame": { "x": 10, "y": 10, "width": 100, "height": 20 }
        },
        {
            "AXLabel": "Continue",
            "frame": { "x": 20, "y": 40, "width": 100, "height": 20 }
        }
    ]);

    let matched = find_element_by_selector(
        &tree,
        &UiSelector {
            text: Some("Continue".to_owned()),
            id: None,
        },
    )
    .unwrap();
    assert_eq!(matched.label, "Continue");
    assert!(matched.frame.is_some());
}

#[test]
fn command_summary_redacts_long_input_text() {
    let summary =
        UiCommand::InputText("this text is definitely longer than the preview limit".to_owned())
            .summary();
    assert!(summary.contains("inputText"));
    assert!(summary.contains("..."));
}

#[test]
fn infer_screen_frame_picks_largest_frame() {
    let tree = json!([
        { "frame": { "x": 10, "y": 10, "width": 50, "height": 10 } },
        { "frame": { "x": 0, "y": 0, "width": 393, "height": 852 } }
    ]);

    let frame = infer_screen_frame(&tree).unwrap();
    assert_eq!(frame.width, 393.0);
    assert_eq!(frame.height, 852.0);
}

#[test]
fn visible_match_ignores_offscreen_elements() {
    let tree = json!([
        { "frame": { "x": 0, "y": 0, "width": 100, "height": 100 } },
        {
            "AXLabel": "Footer",
            "frame": { "x": 10, "y": 140, "width": 50, "height": 20 }
        }
    ]);

    let selector = UiSelector {
        text: Some("Footer".to_owned()),
        id: None,
    };
    assert!(find_element_by_selector(&tree, &selector).is_some());
    assert!(find_visible_element_by_selector(&tree, &selector).is_none());
}

#[test]
fn selector_can_match_identifier_and_copy_label_text() {
    let tree = json!([
        {
            "AXIdentifier": "email-value",
            "AXLabel": "qa@example.com",
            "frame": { "x": 20, "y": 40, "width": 100, "height": 20 }
        }
    ]);

    let matched = find_element_by_selector(
        &tree,
        &UiSelector {
            text: None,
            id: Some("email-value".to_owned()),
        },
    )
    .unwrap();
    assert_eq!(matched.copied_text.as_deref(), Some("qa@example.com"));
}

#[test]
fn macos_ui_uses_device_destination() {
    assert_eq!(
        ui_testing_destination(ApplePlatform::Macos),
        DestinationKind::Device
    );
}

#[test]
fn ios_ui_keeps_simulator_destination() {
    assert_eq!(
        ui_testing_destination(ApplePlatform::Ios),
        DestinationKind::Simulator
    );
}

#[test]
fn visible_scroll_container_prefers_largest_visible_scroll_role() {
    let tree = json!([
        { "frame": { "x": 0, "y": 0, "width": 500, "height": 500 } },
        {
            "AXRole": "AXScrollArea",
            "frame": { "x": 20, "y": 20, "width": 260, "height": 180 }
        },
        {
            "AXRole": "AXTable",
            "frame": { "x": 30, "y": 30, "width": 120, "height": 80 }
        },
        {
            "AXRole": "AXScrollArea",
            "frame": { "x": 20, "y": 540, "width": 300, "height": 200 }
        }
    ]);

    let frame = find_visible_scroll_container(&tree).unwrap();
    assert_eq!(frame.x, 20.0);
    assert_eq!(frame.y, 20.0);
    assert_eq!(frame.width, 260.0);
    assert_eq!(frame.height, 180.0);
}

#[test]
fn ui_runner_prefers_backend_selector_activation_for_tap_on() {
    let temp = tempdir().unwrap();
    let selector_taps = Arc::new(Mutex::new(Vec::new()));
    let backend = TestUiBackend {
        launches: Arc::new(Mutex::new(Vec::new())),
        selector_taps: selector_taps.clone(),
        focus_calls: Arc::new(Mutex::new(0)),
    };
    let mut runner = UiFlowRunner::new(
        Box::new(backend),
        temp.path().join("artifacts"),
        "dev.orbit.fixture".to_owned(),
        false,
        None,
    );

    runner
        .run_leaf_command(&UiCommand::TapOn(UiSelector {
            text: None,
            id: Some("continue-button".to_owned()),
        }))
        .unwrap();

    let recorded = selector_taps.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].text.is_none());
    assert_eq!(recorded[0].id.as_deref(), Some("continue-button"));
}

#[test]
fn ui_runner_best_effort_focuses_after_launch_when_requested() {
    let temp = tempdir().unwrap();
    let focus_calls = Arc::new(Mutex::new(0));
    let backend = TestUiBackend {
        launches: Arc::new(Mutex::new(Vec::new())),
        selector_taps: Arc::new(Mutex::new(Vec::new())),
        focus_calls: focus_calls.clone(),
    };
    let mut runner = UiFlowRunner::new(
        Box::new(backend),
        temp.path().join("artifacts"),
        "dev.orbit.fixture".to_owned(),
        true,
        None,
    );

    runner
        .run_leaf_command(&UiCommand::LaunchApp(Default::default()))
        .unwrap();

    assert_eq!(*focus_calls.lock().unwrap(), 1);
}

#[test]
fn trace_temp_cleanup_removes_only_stale_ktrace_files_by_default() {
    let temp = tempdir().unwrap();
    let stale = temp.path().join("instruments-old.ktrace");
    let recent = temp.path().join("instruments-new.ktrace");
    let ignored = temp.path().join("notes.txt");
    fs::write(&stale, vec![0_u8; 8]).unwrap();
    thread::sleep(Duration::from_secs(2));
    fs::write(&recent, vec![1_u8; 4]).unwrap();
    fs::write(&ignored, b"keep").unwrap();

    let summary = cleanup_macos_trace_temp_dir(temp.path(), false, Duration::from_secs(1)).unwrap();
    assert_eq!(summary.scanned_files, 2);
    assert_eq!(summary.removed_files, 1);
    assert_eq!(summary.skipped_recent_files, 1);
    assert_eq!(summary.freed_bytes, 8);
    assert!(!stale.exists());
    assert!(recent.exists());
    assert!(ignored.exists());
}

#[test]
fn trace_temp_cleanup_can_remove_recent_ktrace_files_with_all_flag() {
    let temp = tempdir().unwrap();
    let recent = temp.path().join("instruments-live.ktrace");
    fs::write(&recent, vec![7_u8; 16]).unwrap();

    let summary =
        cleanup_macos_trace_temp_dir(temp.path(), true, Duration::from_secs(3600)).unwrap();
    assert_eq!(summary.scanned_files, 1);
    assert_eq!(summary.removed_files, 1);
    assert_eq!(summary.skipped_recent_files, 0);
    assert_eq!(summary.freed_bytes, 16);
    assert!(!recent.exists());
}

#[test]
fn flow_selector_matches_configured_name_and_file_stem() {
    let temp = tempdir().unwrap();
    let named = write_flow(
        temp.path(),
        "onboarding-profile.json",
        "{\n  \"$schema\": \"/tmp/.orbit/schemas/orbit-ui-test.v1.json\",\n  \"name\": \"onboarding-provider-setup-profile\",\n  \"steps\": [\"launchApp\"]\n}\n",
    )
    .canonicalize()
    .unwrap();
    let plain = write_flow(
        temp.path(),
        "relaunch.json",
        "{\n  \"$schema\": \"/tmp/.orbit/schemas/orbit-ui-test.v1.json\",\n  \"steps\": [\"launchApp\"]\n}\n",
    )
        .canonicalize()
        .unwrap();

    let selected = select_ui_flow_paths(
        &[named.clone(), plain.clone()],
        &[
            "onboarding-provider-setup-profile".to_owned(),
            "relaunch".to_owned(),
        ],
        temp.path(),
    )
    .unwrap();

    assert_eq!(selected, vec![named, plain]);
}

#[test]
fn flow_selector_reports_available_flows_when_no_match_exists() {
    let temp = tempdir().unwrap();
    let named = write_flow(
        temp.path(),
        "onboarding-profile.json",
        "{\n  \"$schema\": \"/tmp/.orbit/schemas/orbit-ui-test.v1.json\",\n  \"name\": \"onboarding-provider-setup-profile\",\n  \"steps\": [\"launchApp\"]\n}\n",
    )
    .canonicalize()
    .unwrap();

    let error =
        select_ui_flow_paths(&[named], &["missing-flow".to_owned()], temp.path()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("missing-flow"));
    assert!(message.contains("onboarding-profile.json"));
    assert!(message.contains("onboarding-provider-setup-profile"));
}
