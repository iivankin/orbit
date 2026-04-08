use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::json;
use tempfile::tempdir;

use super::backend::UiBackend;
use super::flow::select_ui_flow_paths;
use super::{
    UiCommand, UiCrashDeleteRequest, UiCrashQuery, UiFlowRunner, UiHardwareButton, UiKeyModifier,
    UiKeyPress, UiPermissionConfig, UiSelector, UiSwipeDirection, UiTravel,
    find_element_by_selector, find_visible_element_by_selector, find_visible_scroll_container,
    infer_screen_frame, plan_macos_ui_trace, ui_testing_destination,
};
use crate::apple::build::toolchain::DestinationKind;
use crate::manifest::ApplePlatform;

type LaunchRecords = Arc<Mutex<Vec<Vec<(String, String)>>>>;

#[derive(Default)]
struct TestUiBackend {
    launches: LaunchRecords,
    selector_taps: Arc<Mutex<Vec<UiSelector>>>,
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
fn macos_trace_plan_allows_single_launch_with_prelaunch_commands() {
    let temp = tempdir().unwrap();
    let flow = write_flow(
        temp.path(),
        "flow.yaml",
        "---\n- clearState\n- launchApp\n- assertVisible: Ready\n",
    );

    let plan = plan_macos_ui_trace(&[flow]).unwrap();
    assert_eq!(plan.prelaunch_commands.len(), 1);
    assert!(matches!(
        plan.prelaunch_commands.first(),
        Some(UiCommand::ClearState(None))
    ));
    assert!(plan.launch.is_some());
}

#[test]
fn macos_trace_plan_rejects_multiple_launches() {
    let temp = tempdir().unwrap();
    let first = write_flow(temp.path(), "first.yaml", "---\n- launchApp\n");
    let second = write_flow(temp.path(), "second.yaml", "---\n- launchApp\n");

    let error = plan_macos_ui_trace(&[first, second]).unwrap_err();
    assert!(error.to_string().contains("only one `launchApp`"));
}

#[test]
fn ui_runner_skips_only_the_first_launch_when_trace_prelaunched_the_app() {
    let temp = tempdir().unwrap();
    let launches = Arc::new(Mutex::new(Vec::new()));
    let backend = TestUiBackend {
        launches: launches.clone(),
        selector_taps: Arc::new(Mutex::new(Vec::new())),
    };
    let mut runner = UiFlowRunner::new(
        Box::new(backend),
        temp.path().join("artifacts"),
        "dev.orbit.fixture".to_owned(),
        true,
    );

    runner
        .run_leaf_command(&UiCommand::LaunchApp(super::UiLaunchApp::default()))
        .unwrap();
    assert!(launches.lock().unwrap().is_empty());

    runner
        .run_leaf_command(&UiCommand::LaunchApp(super::UiLaunchApp {
            arguments: vec![("seedUser".to_owned(), "qa@example.com".to_owned())],
            ..super::UiLaunchApp::default()
        }))
        .unwrap();

    let recorded = launches.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0],
        vec![("seedUser".to_owned(), "qa@example.com".to_owned())]
    );
}

#[test]
fn ui_runner_prefers_backend_selector_activation_for_tap_on() {
    let temp = tempdir().unwrap();
    let selector_taps = Arc::new(Mutex::new(Vec::new()));
    let backend = TestUiBackend {
        launches: Arc::new(Mutex::new(Vec::new())),
        selector_taps: selector_taps.clone(),
    };
    let mut runner = UiFlowRunner::new(
        Box::new(backend),
        temp.path().join("artifacts"),
        "dev.orbit.fixture".to_owned(),
        false,
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
fn flow_selector_matches_configured_name_and_file_stem() {
    let temp = tempdir().unwrap();
    let named = write_flow(
        temp.path(),
        "onboarding-profile.yaml",
        "name: onboarding-provider-setup-profile\n---\n- launchApp\n",
    )
    .canonicalize()
    .unwrap();
    let plain = write_flow(temp.path(), "relaunch.yaml", "---\n- launchApp\n")
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
        "onboarding-profile.yaml",
        "name: onboarding-provider-setup-profile\n---\n- launchApp\n",
    )
    .canonicalize()
    .unwrap();

    let error =
        select_ui_flow_paths(&[named], &["missing-flow".to_owned()], temp.path()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("missing-flow"));
    assert!(message.contains("onboarding-profile.yaml"));
    assert!(message.contains("onboarding-provider-setup-profile"));
}
