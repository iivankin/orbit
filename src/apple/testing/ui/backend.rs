use std::path::{Path, PathBuf};
use std::process::Child;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::{
    UiCrashDeleteRequest, UiCrashQuery, UiHardwareButton, UiKeyModifier, UiKeyPress,
    UiPermissionConfig, UiSelector, UiSwipeDirection, UiTravel,
};

#[path = "backend/ios_simulator.rs"]
mod ios_simulator;
#[path = "backend/macos.rs"]
mod macos;

pub use self::ios_simulator::IosSimulatorBackend;
pub use self::macos::MacosBackend;
pub(crate) use self::macos::macos_doctor;

pub trait UiBackend {
    fn backend_name(&self) -> &'static str;
    fn target_name(&self) -> &str;
    fn target_id(&self) -> &str;
    fn auto_record_top_level_flows(&self) -> bool {
        true
    }
    fn video_extension(&self) -> &'static str {
        "mp4"
    }
    fn requires_running_target_for_recording(&self) -> bool {
        false
    }
    fn describe_all(&self) -> Result<JsonValue>;
    fn describe_point(&self, x: f64, y: f64) -> Result<JsonValue>;
    fn launch_app(
        &self,
        bundle_id: &str,
        stop_app: bool,
        arguments: &[(String, String)],
    ) -> Result<()>;
    fn stop_app(&self, bundle_id: &str) -> Result<()>;
    fn clear_app_state(&self, bundle_id: &str) -> Result<()>;
    fn focus(&self) -> Result<()>;
    fn pin_running_target_by_executable(
        &self,
        _executable_path: &Path,
        _ignored_pid: Option<u32>,
    ) -> Result<()> {
        Ok(())
    }
    fn tap_point(&self, x: f64, y: f64, duration_ms: Option<u32>) -> Result<()>;
    fn activate_selector(&self, _selector: &UiSelector) -> Result<bool> {
        Ok(false)
    }
    fn hover_point(&self, _x: f64, _y: f64) -> Result<()> {
        bail!(
            "`hoverOn` is not supported by the current {} backend",
            self.backend_name()
        )
    }
    fn right_click_point(&self, _x: f64, _y: f64) -> Result<()> {
        bail!(
            "`rightClickOn` is not supported by the current {} backend",
            self.backend_name()
        )
    }
    fn swipe_points(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        duration_ms: Option<u32>,
        delta: Option<u32>,
    ) -> Result<()>;
    fn drag_points(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        duration_ms: Option<u32>,
        delta: Option<u32>,
    ) -> Result<()> {
        self.swipe_points(start, end, duration_ms, delta)
    }
    fn input_text(&self, text: &str) -> Result<()>;
    fn press_button(&self, button: UiHardwareButton, duration_ms: Option<u32>) -> Result<()>;
    fn press_key(&self, key: &UiKeyPress) -> Result<()>;
    fn select_menu_item(&self, _path: &[String]) -> Result<()> {
        bail!(
            "`selectMenuItem` is not supported by the current {} backend",
            self.backend_name()
        )
    }
    fn press_key_code(
        &self,
        keycode: u32,
        duration_ms: Option<u32>,
        modifiers: &[UiKeyModifier],
    ) -> Result<()>;
    fn press_key_sequence(&self, keycodes: &[u32]) -> Result<()>;
    fn take_screenshot(&self, path: &Path) -> Result<()>;
    fn open_link(&self, url: &str) -> Result<()>;
    fn clear_keychain(&self) -> Result<()>;
    fn set_location(&self, latitude: f64, longitude: f64) -> Result<()>;
    fn set_permissions(&self, bundle_id: &str, config: &UiPermissionConfig) -> Result<()>;
    fn travel(&self, command: &UiTravel) -> Result<()>;
    fn add_media(&self, paths: &[PathBuf]) -> Result<()>;
    fn install_dylib(&self, path: &Path) -> Result<()>;
    fn run_instruments(&self, template: &str, arguments: &[String]) -> Result<()>;
    fn update_contacts(&self, path: &Path) -> Result<()>;
    fn list_crash_logs(&self, query: &UiCrashQuery) -> Result<()>;
    fn show_crash_log(&self, name: &str) -> Result<()>;
    fn delete_crash_logs(&self, request: &UiCrashDeleteRequest) -> Result<()>;
    fn stream_logs(&self, arguments: &[String]) -> Result<()>;
    fn scroll_in_direction(&self, direction: UiSwipeDirection) -> Result<()>;
    fn scroll_at_point(&self, direction: UiSwipeDirection, point: (f64, f64)) -> Result<()>;
    fn hide_keyboard(&self) -> Result<()>;
    fn start_video_recording(&mut self, path: &Path) -> Result<()>;
    fn stop_video_recording(&mut self) -> Result<()>;
}

struct ActiveVideoRecording {
    path: PathBuf,
    child: Child,
}

#[derive(Debug, Deserialize)]
struct MacosWindowInfo {
    #[serde(rename = "windowNumber")]
    _window_number: i64,
    frame: MacosWindowFrame,
}

#[derive(Debug, Deserialize)]
struct MacosWindowFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct MacosDoctorStatus {
    #[serde(rename = "accessibilityTrusted")]
    pub accessibility_trusted: bool,
    #[serde(rename = "screenCaptureAccess")]
    pub screen_capture_access: bool,
}

fn idb_requirement_message() -> &'static str {
    super::idb_requirement_message()
}
