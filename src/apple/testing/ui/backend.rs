use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tempfile::{Builder as TempfileBuilder, TempDir, tempdir};

use super::{
    UiCrashDeleteRequest, UiCrashQuery, UiHardwareButton, UiKeyModifier, UiKeyPress,
    UiLocationPoint, UiPermissionConfig, UiPermissionState, UiPressKey, UiSwipeDirection, UiTravel,
};
use crate::apple::build::pipeline::macos_executable_path;
use crate::apple::logs::MacosInferiorLogRelay;
use crate::apple::simulator::{SimulatorDevice, select_simulator_device};
use crate::apple::xcode::{
    SelectedXcode, lldb_path as selected_xcode_lldb_path,
    log_redirect_dylib_path as selected_xcode_log_redirect_dylib_path, xcrun_command,
};
use crate::context::ProjectContext;
use crate::util::{
    command_output, command_output_allow_failure, ensure_dir, run_command, run_command_capture,
};

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
    fn tap_point(&self, x: f64, y: f64, duration_ms: Option<u32>) -> Result<()>;
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

pub struct IosSimulatorBackend {
    device: SimulatorDevice,
    bundle_path: PathBuf,
    bundle_id: String,
    active_video: Option<ActiveVideoRecording>,
    selected_xcode: Option<SelectedXcode>,
}

impl IosSimulatorBackend {
    pub fn attach(project: &ProjectContext) -> Result<Self> {
        let device = select_simulator_device(project, crate::manifest::ApplePlatform::Ios)?;
        if !device.is_booted() {
            let mut boot = xcrun_command(project.selected_xcode.as_ref());
            boot.args(["simctl", "boot", &device.udid]);
            run_command(&mut boot)?;
        }

        let mut bootstatus = xcrun_command(project.selected_xcode.as_ref());
        bootstatus.args(["simctl", "bootstatus", &device.udid, "-b"]);
        run_command(&mut bootstatus)?;

        Ok(Self {
            device,
            bundle_path: PathBuf::new(),
            bundle_id: String::new(),
            active_video: None,
            selected_xcode: project.selected_xcode.clone(),
        })
    }

    pub fn prepare(
        project: &ProjectContext,
        receipt: &crate::apple::build::receipt::BuildReceipt,
    ) -> Result<Self> {
        let mut backend = Self::attach(project)?;

        let mut install = xcrun_command(backend.selected_xcode.as_ref());
        install.args([
            "simctl",
            "install",
            &backend.device.udid,
            receipt
                .bundle_path
                .to_str()
                .context("bundle path contains invalid UTF-8")?,
        ]);
        run_command(&mut install)?;

        backend.bundle_path = receipt.bundle_path.clone();
        backend.bundle_id = receipt.bundle_id.clone();
        Ok(backend)
    }

    fn run_idb(&self, arguments: &[String]) -> Result<()> {
        let mut command = Command::new("idb");
        command.args(arguments);
        command.arg("--udid").arg(&self.device.udid);
        run_command(&mut command).with_context(idb_requirement_message)
    }

    fn idb_output(&self, arguments: &[String]) -> Result<String> {
        let mut command = Command::new("idb");
        command.args(arguments);
        command.arg("--udid").arg(&self.device.udid);
        command_output(&mut command).with_context(idb_requirement_message)
    }

    fn run_simctl_privacy(&self, action: &str, service: &str, bundle_id: &str) -> Result<()> {
        let mut command = xcrun_command(self.selected_xcode.as_ref());
        command.args([
            "simctl",
            "privacy",
            &self.device.udid,
            action,
            service,
            bundle_id,
        ]);
        run_command(&mut command)
    }

    fn run_idb_passthrough(&self, command_name: &str, arguments: &[String]) -> Result<()> {
        let mut command = Command::new("idb");
        command.arg(command_name);
        command.args(arguments);
        command.arg("--udid").arg(&self.device.udid);
        run_command(&mut command).with_context(idb_requirement_message)
    }
}

impl UiBackend for IosSimulatorBackend {
    fn backend_name(&self) -> &'static str {
        "orbit-idb-ios-simulator"
    }

    fn target_name(&self) -> &str {
        &self.device.name
    }

    fn target_id(&self) -> &str {
        &self.device.udid
    }

    fn describe_all(&self) -> Result<JsonValue> {
        let output = self.idb_output(&["ui".to_owned(), "describe-all".to_owned()])?;
        serde_json::from_str(&output).context("failed to parse `idb ui describe-all` output")
    }

    fn describe_point(&self, x: f64, y: f64) -> Result<JsonValue> {
        let output = self.idb_output(&[
            "ui".to_owned(),
            "describe-point".to_owned(),
            format!("{}", x.round() as i64),
            format!("{}", y.round() as i64),
        ])?;
        serde_json::from_str(&output).context("failed to parse `idb ui describe-point` output")
    }

    fn launch_app(
        &self,
        bundle_id: &str,
        stop_app: bool,
        arguments: &[(String, String)],
    ) -> Result<()> {
        if stop_app {
            self.stop_app(bundle_id)?;
            let mut command = xcrun_command(self.selected_xcode.as_ref());
            command.args(["simctl", "launch", &self.device.udid, bundle_id]);
            for (key, value) in arguments {
                command.arg(format!("-{key}"));
                command.arg(value);
            }
            run_command_capture(&mut command).map(|_| ())
        } else {
            let mut command = Command::new("idb");
            command.args(["launch", "-f", bundle_id]);
            for (key, value) in arguments {
                command.arg(format!("-{key}"));
                command.arg(value);
            }
            command.arg("--udid").arg(&self.device.udid);
            run_command(&mut command).with_context(idb_requirement_message)
        }
    }

    fn stop_app(&self, bundle_id: &str) -> Result<()> {
        let mut command = xcrun_command(self.selected_xcode.as_ref());
        command.args(["simctl", "terminate", &self.device.udid, bundle_id]);
        let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
        if success {
            return Ok(());
        }
        let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
        if combined.contains("found nothing to terminate") || combined.contains("not running") {
            return Ok(());
        }
        bail!("failed to stop `{bundle_id}` on {}", self.device.name)
    }

    fn clear_app_state(&self, bundle_id: &str) -> Result<()> {
        if bundle_id != self.bundle_id {
            bail!(
                "clearState currently supports only Orbit's built app `{}` on iOS simulators",
                self.bundle_id
            );
        }

        self.stop_app(bundle_id)?;
        self.run_idb(&["uninstall".to_owned(), bundle_id.to_owned()])?;

        let mut install = xcrun_command(self.selected_xcode.as_ref());
        install.args([
            "simctl",
            "install",
            &self.device.udid,
            self.bundle_path
                .to_str()
                .context("bundle path contains invalid UTF-8")?,
        ]);
        run_command(&mut install)
    }

    fn focus(&self) -> Result<()> {
        self.run_idb_passthrough("focus", &[])
    }

    fn tap_point(&self, x: f64, y: f64, duration_ms: Option<u32>) -> Result<()> {
        let mut arguments = vec!["ui".to_owned(), "tap".to_owned()];
        if let Some(duration_ms) = duration_ms {
            arguments.push("--duration".to_owned());
            arguments.push(format!("{:.3}", duration_ms as f64 / 1000.0));
        }
        arguments.extend([
            format!("{}", x.round() as i64),
            format!("{}", y.round() as i64),
        ]);
        self.run_idb(&arguments)
    }

    fn swipe_points(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        duration_ms: Option<u32>,
        delta: Option<u32>,
    ) -> Result<()> {
        let mut arguments = vec!["ui".to_owned(), "swipe".to_owned()];
        if let Some(duration_ms) = duration_ms {
            arguments.push("--duration".to_owned());
            arguments.push(format!("{:.3}", duration_ms as f64 / 1000.0));
        }
        if let Some(delta) = delta {
            arguments.push("--delta".to_owned());
            arguments.push(delta.to_string());
        }
        arguments.extend([
            format!("{}", start.0.round() as i64),
            format!("{}", start.1.round() as i64),
            format!("{}", end.0.round() as i64),
            format!("{}", end.1.round() as i64),
        ]);
        self.run_idb(&arguments)
    }

    fn input_text(&self, text: &str) -> Result<()> {
        self.run_idb(&["ui".to_owned(), "text".to_owned(), text.to_owned()])
    }

    fn press_button(&self, button: UiHardwareButton, duration_ms: Option<u32>) -> Result<()> {
        let mut arguments = vec!["ui".to_owned(), "button".to_owned()];
        if let Some(duration_ms) = duration_ms {
            arguments.push("--duration".to_owned());
            arguments.push(format!("{:.3}", duration_ms as f64 / 1000.0));
        }
        arguments.push(button.summary().to_owned());
        self.run_idb(&arguments)
    }

    fn press_key(&self, key: &UiKeyPress) -> Result<()> {
        if !key.modifiers.is_empty() {
            bail!("keyboard modifiers are not supported by the current iOS simulator backend");
        }

        match key.key {
            UiPressKey::Home => self.press_button(UiHardwareButton::Home, None),
            UiPressKey::Lock | UiPressKey::Power => self.press_button(UiHardwareButton::Lock, None),
            UiPressKey::Enter => self.press_key_code(40, None, &[]),
            UiPressKey::Backspace => self.press_key_code(42, None, &[]),
            UiPressKey::Escape => self.press_key_code(41, None, &[]),
            UiPressKey::Space => self.press_key_code(44, None, &[]),
            UiPressKey::Tab => self.press_key_code(43, None, &[]),
            UiPressKey::LeftArrow => self.press_key_code(80, None, &[]),
            UiPressKey::RightArrow => self.press_key_code(79, None, &[]),
            UiPressKey::DownArrow => self.press_key_code(81, None, &[]),
            UiPressKey::UpArrow => self.press_key_code(82, None, &[]),
            UiPressKey::Character(character) => {
                let Some(keycode) = ios_hid_keycode_for_character(character) else {
                    bail!(
                        "`pressKey {}` is not supported by the current iOS simulator backend",
                        key.summary()
                    );
                };
                self.press_key_code(keycode, None, &[])
            }
            UiPressKey::VolumeUp | UiPressKey::VolumeDown | UiPressKey::Back => bail!(
                "`pressKey {}` is not supported by the current iOS simulator backend",
                key.summary()
            ),
        }
    }

    fn press_key_code(
        &self,
        keycode: u32,
        duration_ms: Option<u32>,
        modifiers: &[UiKeyModifier],
    ) -> Result<()> {
        if !modifiers.is_empty() {
            bail!("keyboard modifiers are not supported by the current iOS simulator backend");
        }
        let mut arguments = vec!["ui".to_owned(), "key".to_owned()];
        if let Some(duration_ms) = duration_ms {
            arguments.push("--duration".to_owned());
            arguments.push(format!("{:.3}", duration_ms as f64 / 1000.0));
        }
        arguments.push(keycode.to_string());
        self.run_idb(&arguments)
    }

    fn press_key_sequence(&self, keycodes: &[u32]) -> Result<()> {
        let mut arguments = vec!["ui".to_owned(), "key-sequence".to_owned()];
        arguments.extend(keycodes.iter().map(u32::to_string));
        self.run_idb(&arguments)
    }

    fn take_screenshot(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        let mut command = xcrun_command(self.selected_xcode.as_ref());
        command.args([
            "simctl",
            "io",
            &self.device.udid,
            "screenshot",
            path.to_str()
                .context("screenshot path contains invalid UTF-8")?,
        ]);
        run_command_capture(&mut command).map(|_| ())
    }

    fn open_link(&self, url: &str) -> Result<()> {
        self.run_idb(&["open".to_owned(), url.to_owned()])
    }

    fn clear_keychain(&self) -> Result<()> {
        self.run_idb(&["clear-keychain".to_owned()])
    }

    fn set_location(&self, latitude: f64, longitude: f64) -> Result<()> {
        self.run_idb(&[
            "set-location".to_owned(),
            latitude.to_string(),
            longitude.to_string(),
        ])
    }

    fn set_permissions(&self, bundle_id: &str, config: &UiPermissionConfig) -> Result<()> {
        for permission in &config.permissions {
            match permission.name.as_str() {
                "all" => match permission.state {
                    UiPermissionState::Allow => {
                        self.run_simctl_privacy("grant", "all", bundle_id)?;
                        self.run_idb(&[
                            "approve".to_owned(),
                            bundle_id.to_owned(),
                            "photos".to_owned(),
                            "camera".to_owned(),
                            "contacts".to_owned(),
                            "url".to_owned(),
                            "location".to_owned(),
                            "notification".to_owned(),
                        ])?;
                    }
                    UiPermissionState::Deny => {
                        self.run_simctl_privacy("revoke", "all", bundle_id)?;
                    }
                    UiPermissionState::Unset => {
                        let mut command = xcrun_command(self.selected_xcode.as_ref());
                        command.args(["simctl", "privacy", &self.device.udid, "reset", "all"]);
                        run_command(&mut command)?;
                    }
                },
                "camera" | "notification" | "url" => match permission.state {
                    UiPermissionState::Allow => {
                        self.run_idb(&[
                            "approve".to_owned(),
                            bundle_id.to_owned(),
                            permission.name.clone(),
                        ])?;
                    }
                    _ => bail!(
                        "permission `{}` only supports `allow` on the current iOS simulator backend",
                        permission.name
                    ),
                },
                "contacts" => {
                    apply_simulator_permission(self, bundle_id, permission, "contacts")?;
                }
                "photos" => {
                    apply_simulator_permission(self, bundle_id, permission, "photos")?;
                }
                "location" => {
                    apply_simulator_permission(self, bundle_id, permission, "location")?;
                }
                "microphone" => {
                    apply_simulator_permission(self, bundle_id, permission, "microphone")?;
                }
                "calendar" => {
                    apply_simulator_permission(self, bundle_id, permission, "calendar")?;
                }
                "reminders" => {
                    apply_simulator_permission(self, bundle_id, permission, "reminders")?;
                }
                "motion" => {
                    apply_simulator_permission(self, bundle_id, permission, "motion")?;
                }
                "media-library" | "mediaLibrary" => {
                    apply_simulator_permission(self, bundle_id, permission, "media-library")?;
                }
                "siri" => {
                    apply_simulator_permission(self, bundle_id, permission, "siri")?;
                }
                other => bail!("unsupported permission `{other}`"),
            }
        }
        Ok(())
    }

    fn travel(&self, command: &UiTravel) -> Result<()> {
        let mut simctl = xcrun_command(self.selected_xcode.as_ref());
        simctl.args(["simctl", "location", &self.device.udid, "start"]);
        if let Some(speed) = command.speed_meters_per_second {
            simctl.arg(format!("--speed={speed}"));
        }
        for UiLocationPoint {
            latitude,
            longitude,
        } in &command.points
        {
            simctl.arg(format!("{latitude},{longitude}"));
        }
        run_command(&mut simctl)
    }

    fn add_media(&self, paths: &[PathBuf]) -> Result<()> {
        let arguments = paths
            .iter()
            .map(|path| {
                path.to_str()
                    .context("media path contains invalid UTF-8")
                    .map(str::to_owned)
            })
            .collect::<Result<Vec<_>>>()?;
        self.run_idb_passthrough("add-media", &arguments)
    }

    fn install_dylib(&self, path: &Path) -> Result<()> {
        self.run_idb(&[
            "dylib".to_owned(),
            "install".to_owned(),
            path.to_str()
                .context("dylib path contains invalid UTF-8")?
                .to_owned(),
        ])
    }

    fn run_instruments(&self, template: &str, arguments: &[String]) -> Result<()> {
        let mut command = Command::new("idb");
        command.args(["instruments", "--template", template]);
        command.args(arguments);
        command.arg("--udid").arg(&self.device.udid);
        run_command(&mut command).with_context(idb_requirement_message)
    }

    fn update_contacts(&self, path: &Path) -> Result<()> {
        self.run_idb(&[
            "contacts".to_owned(),
            "update".to_owned(),
            path.to_str()
                .context("contacts path contains invalid UTF-8")?
                .to_owned(),
        ])
    }

    fn list_crash_logs(&self, query: &UiCrashQuery) -> Result<()> {
        let mut arguments = vec!["crash".to_owned(), "list".to_owned()];
        if let Some(before) = query.before.as_deref() {
            arguments.push("--before".to_owned());
            arguments.push(before.to_owned());
        }
        if let Some(since) = query.since.as_deref() {
            arguments.push("--since".to_owned());
            arguments.push(since.to_owned());
        }
        if let Some(bundle_id) = query.bundle_id.as_deref() {
            arguments.push("--bundle-id".to_owned());
            arguments.push(bundle_id.to_owned());
        }
        self.run_idb(&arguments)
    }

    fn show_crash_log(&self, name: &str) -> Result<()> {
        self.run_idb(&["crash".to_owned(), "show".to_owned(), name.to_owned()])
    }

    fn delete_crash_logs(&self, request: &UiCrashDeleteRequest) -> Result<()> {
        if request.name.is_none()
            && !request.delete_all
            && request.before.is_none()
            && request.since.is_none()
            && request.bundle_id.is_none()
        {
            bail!("crash delete requires a crash name, filters, or `--all`");
        }
        if !request.delete_all
            && request.name.is_none()
            && (request.before.is_some() || request.since.is_some() || request.bundle_id.is_some())
        {
            bail!("crash delete filters require `--all`");
        }

        let mut arguments = vec!["crash".to_owned(), "delete".to_owned()];
        if let Some(name) = request.name.as_deref() {
            arguments.push(name.to_owned());
        }
        if let Some(before) = request.before.as_deref() {
            arguments.push("--before".to_owned());
            arguments.push(before.to_owned());
        }
        if let Some(since) = request.since.as_deref() {
            arguments.push("--since".to_owned());
            arguments.push(since.to_owned());
        }
        if let Some(bundle_id) = request.bundle_id.as_deref() {
            arguments.push("--bundle-id".to_owned());
            arguments.push(bundle_id.to_owned());
        }
        if request.delete_all {
            arguments.push("--all".to_owned());
        }
        self.run_idb(&arguments)
    }

    fn stream_logs(&self, arguments: &[String]) -> Result<()> {
        let mut command = Command::new("idb");
        command.arg("log");
        command.arg("--udid").arg(&self.device.udid);
        if !arguments.is_empty() {
            command.arg("--");
            command.args(arguments);
        }
        run_command(&mut command).with_context(idb_requirement_message)
    }

    fn scroll_in_direction(&self, direction: UiSwipeDirection) -> Result<()> {
        let tree = self.describe_all()?;
        let screen = super::infer_screen_frame(&tree)
            .context("could not infer simulator screen bounds from the accessibility tree")?;
        let (start, end) = match direction {
            UiSwipeDirection::Left => ((10.0, 50.0), (90.0, 50.0)),
            UiSwipeDirection::Right => ((90.0, 50.0), (10.0, 50.0)),
            UiSwipeDirection::Up => ((50.0, 20.0), (50.0, 90.0)),
            UiSwipeDirection::Down => ((50.0, 50.0), (50.0, 10.0)),
        };
        let start = (
            screen.x + (screen.width * start.0 / 100.0),
            screen.y + (screen.height * start.1 / 100.0),
        );
        let end = (
            screen.x + (screen.width * end.0 / 100.0),
            screen.y + (screen.height * end.1 / 100.0),
        );
        self.swipe_points(start, end, Some(500), Some(5))
    }

    fn scroll_at_point(&self, direction: UiSwipeDirection, point: (f64, f64)) -> Result<()> {
        let tree = self.describe_all()?;
        let screen = super::infer_screen_frame(&tree)
            .context("could not infer simulator screen bounds from the accessibility tree")?;
        let horizontal_span = (screen.width * 0.28).clamp(40.0, 180.0);
        let vertical_span = (screen.height * 0.28).clamp(60.0, 220.0);
        let clamp_x = |value: f64| value.clamp(screen.x + 16.0, screen.x + screen.width - 16.0);
        let clamp_y = |value: f64| value.clamp(screen.y + 16.0, screen.y + screen.height - 16.0);

        let (start, end) = match direction {
            UiSwipeDirection::Left => (
                (clamp_x(point.0 - horizontal_span), point.1),
                (clamp_x(point.0 + horizontal_span), point.1),
            ),
            UiSwipeDirection::Right => (
                (clamp_x(point.0 + horizontal_span), point.1),
                (clamp_x(point.0 - horizontal_span), point.1),
            ),
            UiSwipeDirection::Up => (
                (point.0, clamp_y(point.1 - vertical_span)),
                (point.0, clamp_y(point.1 + vertical_span)),
            ),
            UiSwipeDirection::Down => (
                (point.0, clamp_y(point.1 + vertical_span)),
                (point.0, clamp_y(point.1 - vertical_span)),
            ),
        };
        self.swipe_points(start, end, Some(500), Some(5))
    }

    fn hide_keyboard(&self) -> Result<()> {
        let tree = self.describe_all()?;
        let screen = super::infer_screen_frame(&tree)
            .context("could not infer simulator screen bounds from the accessibility tree")?;
        let start = (
            screen.x + (screen.width * 0.50),
            screen.y + (screen.height * 0.68),
        );
        let end = (
            screen.x + (screen.width * 0.50),
            screen.y + (screen.height * 0.54),
        );
        self.swipe_points(start, end, Some(120), Some(3))
            .context("failed to dismiss the software keyboard")
    }

    fn start_video_recording(&mut self, path: &Path) -> Result<()> {
        if self.active_video.is_some() {
            bail!("video recording is already active for {}", self.device.name);
        }
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }

        let mut command = Command::new("idb");
        command.args([
            "video",
            path.to_str().context("video path contains invalid UTF-8")?,
            "--udid",
            &self.device.udid,
        ]);
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        let child = command
            .spawn()
            .with_context(|| format!("failed to start video recording for {}", self.device.name))
            .with_context(idb_requirement_message)?;
        self.active_video = Some(ActiveVideoRecording {
            path: path.to_path_buf(),
            child,
        });
        Ok(())
    }

    fn stop_video_recording(&mut self) -> Result<()> {
        let Some(mut recording) = self.active_video.take() else {
            return Ok(());
        };

        let graceful_wait_started = Instant::now();
        while graceful_wait_started.elapsed() < Duration::from_millis(250) {
            if let Some(status) = recording.child.try_wait()? {
                if !status.success() && !recording.path.exists() {
                    bail!(
                        "`idb video` exited with {status} before writing {}",
                        recording.path.display()
                    );
                }
                return Ok(());
            }
            thread::sleep(Duration::from_millis(25));
        }

        if recording.child.try_wait()?.is_none() {
            let mut interrupt = Command::new("kill");
            interrupt.args(["-INT", &recording.child.id().to_string()]);
            run_command(&mut interrupt).with_context(|| {
                format!("failed to stop video recording for {}", self.device.name)
            })?;
        }

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Some(status) = recording.child.try_wait()? {
                if !status.success() && !recording.path.exists() {
                    bail!(
                        "`idb video` exited with {status} before writing {}",
                        recording.path.display()
                    );
                }
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = recording.child.kill();
        let _ = recording.child.wait();
        if recording.path.exists() {
            return Ok(());
        }

        bail!(
            "timed out waiting for video recording to finish writing {}",
            recording.path.display()
        )
    }
}

pub struct MacosBackend {
    helper_path: PathBuf,
    bundle_id: String,
    executable_path: PathBuf,
    selected_xcode: Option<SelectedXcode>,
    verbose: bool,
    launched_session: Mutex<Option<MacosLaunchedSession>>,
    last_tap_point: Mutex<Option<(f64, f64)>>,
    active_video: Option<ActiveVideoRecording>,
}

struct MacosLaunchedSession {
    child: Child,
    _launch_dir: TempDir,
    _log_pipe_anchor: fs::File,
    _log_relay: MacosInferiorLogRelay,
}

impl MacosBackend {
    pub fn prepare(
        project: &ProjectContext,
        receipt: &crate::apple::build::receipt::BuildReceipt,
    ) -> Result<Self> {
        Ok(Self {
            helper_path: ensure_macos_driver_binary(project)?,
            bundle_id: receipt.bundle_id.clone(),
            executable_path: macos_executable_path(receipt)?,
            selected_xcode: project.selected_xcode.clone(),
            verbose: project.app.verbose,
            launched_session: Mutex::new(None),
            last_tap_point: Mutex::new(None),
            active_video: None,
        })
    }

    fn run_helper(&self, arguments: &[String]) -> Result<()> {
        let mut command = Command::new(&self.helper_path);
        command.args(arguments);
        run_command(&mut command).with_context(macos_requirement_message)
    }

    fn helper_output(&self, arguments: &[String]) -> Result<String> {
        let mut command = Command::new(&self.helper_path);
        command.args(arguments);
        command_output(&mut command).with_context(macos_requirement_message)
    }

    fn stop_launched_process(&self) -> Result<()> {
        let mut session = self
            .launched_session
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend process state"))?;
        let Some(mut session) = session.take() else {
            return Ok(());
        };
        if session.child.try_wait()?.is_some() {
            return Ok(());
        }

        let mut terminate = Command::new("kill");
        terminate.args(["-TERM", &session.child.id().to_string()]);
        let _ = command_output_allow_failure(&mut terminate)?;

        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if session.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }

        let _ = session.child.kill();
        let _ = session.child.wait();
        Ok(())
    }

    fn start_attached_log_session(
        &self,
        arguments: &[(String, String)],
    ) -> Result<MacosLaunchedSession> {
        let launch_dir = tempdir().context("failed to create macOS UI launch tempdir")?;
        let log_pipe = launch_dir.path().join("inferior-stdio.pipe");
        let lldb_script = launch_dir.path().join("run.expect");
        let coordinator_script = launch_dir.path().join("coordinator.expect");
        let wrapper_script = launch_dir.path().join("launch.zsh");

        let mut mkfifo = Command::new("mkfifo");
        mkfifo.arg(&log_pipe);
        run_command(&mut mkfifo)?;

        let log_pipe_anchor = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&log_pipe)
            .with_context(|| {
                format!("failed to open macOS UI log pipe `{}`", log_pipe.display())
            })?;
        let log_relay = MacosInferiorLogRelay::start(&log_pipe, &self.bundle_id, self.verbose);

        fs::write(
            &lldb_script,
            macos_lldb_run_script(self.selected_xcode.as_ref(), arguments)?.as_bytes(),
        )
        .with_context(|| format!("failed to write {}", lldb_script.display()))?;

        fs::write(
            &wrapper_script,
            macos_attached_launch_wrapper(
                self.bundle_id.as_str(),
                self.executable_path.as_path(),
                lldb_script.as_path(),
                log_pipe.as_path(),
            )?
            .as_bytes(),
        )
        .with_context(|| format!("failed to write {}", wrapper_script.display()))?;

        fs::write(
            &coordinator_script,
            macos_expect_wrapper_coordinator().as_bytes(),
        )
        .with_context(|| format!("failed to write {}", coordinator_script.display()))?;

        let mut chmod = Command::new("chmod");
        chmod.args(["+x"]);
        chmod.arg(&wrapper_script);
        run_command(&mut chmod)?;

        let mut child = Command::new("expect");
        child.args(["-f"]);
        child.arg(&coordinator_script);
        child.arg(&wrapper_script);
        child.stdin(Stdio::inherit());
        child.stdout(Stdio::inherit());
        child.stderr(Stdio::inherit());
        let child = child
            .spawn()
            .with_context(|| format!("failed to start `{}` under LLDB", self.bundle_id))?;

        Ok(MacosLaunchedSession {
            child,
            _launch_dir: launch_dir,
            _log_pipe_anchor: log_pipe_anchor,
            _log_relay: log_relay,
        })
    }

    fn window_capture_info(&self) -> Result<MacosWindowInfo> {
        let started = Instant::now();
        let mut last_error = None;
        while started.elapsed() < Duration::from_secs(10) {
            let mut command = Command::new(&self.helper_path);
            command.args(["window-info", "--bundle-id", self.bundle_id.as_str()]);
            let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
            if success {
                return serde_json::from_str(&stdout).context("failed to parse macOS window info");
            }
            let detail = stderr.trim();
            if !detail.is_empty() {
                last_error = Some(detail.to_owned());
            }
            thread::sleep(Duration::from_millis(100));
        }

        match last_error {
            Some(detail) => bail!("{detail}"),
            None => bail!(
                "timed out waiting for a visible macOS window for `{}`",
                self.bundle_id
            ),
        }
    }

    fn wait_for_focusable_app(&self) -> Result<()> {
        let started = Instant::now();
        let mut last_error = None;
        while started.elapsed() < Duration::from_secs(10) {
            let mut command = Command::new(&self.helper_path);
            command.args(["focus", "--bundle-id", self.bundle_id.as_str()]);
            let (success, _stdout, stderr) = command_output_allow_failure(&mut command)?;
            if success {
                return Ok(());
            }
            let detail = stderr.trim();
            if !detail.is_empty() {
                last_error = Some(detail.to_owned());
            }
            thread::sleep(Duration::from_millis(100));
        }

        match last_error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => bail!("timed out waiting for macOS app focus"),
        }
    }

    fn reopen_window(&self) -> Result<()> {
        let started = Instant::now();
        let mut last_error = None;
        while started.elapsed() < Duration::from_secs(10) {
            let mut script = Command::new("osascript");
            script.args([
                "-e",
                &format!("tell application id \"{}\" to activate", self.bundle_id),
                "-e",
                &format!("tell application id \"{}\" to reopen", self.bundle_id),
            ]);
            let (success, _stdout, stderr) = command_output_allow_failure(&mut script)?;
            if success {
                return Ok(());
            }
            let detail = stderr.trim();
            if !detail.is_empty() {
                last_error = Some(detail.to_owned());
            }
            thread::sleep(Duration::from_millis(100));
        }

        match last_error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => bail!("timed out waiting to send macOS reopen AppleEvent"),
        }
    }
}

impl Drop for MacosBackend {
    fn drop(&mut self) {
        let _ = self.stop_video_recording();
        let _ = self.stop_launched_process();
    }
}

impl UiBackend for MacosBackend {
    fn backend_name(&self) -> &'static str {
        "orbit-ax-macos"
    }

    fn target_name(&self) -> &str {
        "Mac"
    }

    fn target_id(&self) -> &str {
        "mac"
    }

    fn auto_record_top_level_flows(&self) -> bool {
        false
    }

    fn video_extension(&self) -> &'static str {
        "mov"
    }

    fn requires_running_target_for_recording(&self) -> bool {
        true
    }

    fn describe_all(&self) -> Result<JsonValue> {
        let output = self.helper_output(&[
            "describe-all".to_owned(),
            "--bundle-id".to_owned(),
            self.bundle_id.clone(),
        ])?;
        serde_json::from_str(&output).context("failed to parse macOS accessibility tree")
    }

    fn describe_point(&self, x: f64, y: f64) -> Result<JsonValue> {
        let output = self.helper_output(&[
            "describe-point".to_owned(),
            "--x".to_owned(),
            x.to_string(),
            "--y".to_owned(),
            y.to_string(),
        ])?;
        serde_json::from_str(&output).context("failed to parse macOS point accessibility data")
    }

    fn launch_app(
        &self,
        bundle_id: &str,
        stop_app: bool,
        arguments: &[(String, String)],
    ) -> Result<()> {
        if bundle_id != self.bundle_id {
            bail!(
                "launchApp currently supports only Orbit's built app `{}` on macOS",
                self.bundle_id
            );
        }
        if stop_app {
            self.stop_app(bundle_id)?;
        }
        if !stop_app
            && self
                .launched_session
                .lock()
                .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend process state"))?
                .is_some()
        {
            self.reopen_window()?;
            self.window_capture_info()?;
            return self.wait_for_focusable_app();
        }

        let session = self.start_attached_log_session(arguments)?;
        *self
            .launched_session
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend process state"))? =
            Some(session);
        self.window_capture_info()?;
        self.wait_for_focusable_app()
    }

    fn stop_app(&self, bundle_id: &str) -> Result<()> {
        if bundle_id != self.bundle_id {
            bail!(
                "stopApp currently supports only Orbit's built app `{}` on macOS",
                self.bundle_id
            );
        }
        self.stop_launched_process()?;

        let mut script = Command::new("osascript");
        script.args([
            "-e",
            &format!("tell application id \"{bundle_id}\" to quit"),
        ]);
        let _ = command_output_allow_failure(&mut script)?;

        let process_name = self
            .executable_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(bundle_id);
        let mut killall = Command::new("killall");
        killall.args(["-TERM", process_name]);
        let _ = command_output_allow_failure(&mut killall)?;
        Ok(())
    }

    fn clear_app_state(&self, bundle_id: &str) -> Result<()> {
        if bundle_id != self.bundle_id {
            bail!(
                "clearState currently supports only Orbit's built app `{}` on macOS",
                self.bundle_id
            );
        }

        self.stop_app(bundle_id)?;

        // Keep the cleanup scoped to bundle-specific storage roots so Orbit does not
        // touch shared containers or unrelated app data on the host Mac.
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let home = PathBuf::from(home);
        let candidate_paths = [
            home.join("Library")
                .join("Preferences")
                .join(format!("{bundle_id}.plist")),
            home.join("Library")
                .join("Application Support")
                .join(bundle_id),
            home.join("Library").join("Caches").join(bundle_id),
            home.join("Library").join("HTTPStorages").join(bundle_id),
            home.join("Library").join("WebKit").join(bundle_id),
            home.join("Library")
                .join("Saved Application State")
                .join(format!("{bundle_id}.savedState")),
            home.join("Library").join("Containers").join(bundle_id),
        ];

        for path in candidate_paths {
            remove_path_if_exists(&path)?;
        }

        let mut defaults = Command::new("defaults");
        defaults.args(["delete", bundle_id]);
        let _ = command_output_allow_failure(&mut defaults)?;

        if let Ok(user) = std::env::var("USER") {
            let mut cfprefsd = Command::new("killall");
            cfprefsd.args(["-u", user.as_str(), "cfprefsd"]);
            let _ = command_output_allow_failure(&mut cfprefsd)?;
        }

        Ok(())
    }

    fn focus(&self) -> Result<()> {
        self.run_helper(&[
            "focus".to_owned(),
            "--bundle-id".to_owned(),
            self.bundle_id.clone(),
        ])
    }

    fn tap_point(&self, x: f64, y: f64, duration_ms: Option<u32>) -> Result<()> {
        self.focus()?;
        thread::sleep(Duration::from_millis(80));
        let mut arguments = vec![
            "tap".to_owned(),
            "--x".to_owned(),
            x.to_string(),
            "--y".to_owned(),
            y.to_string(),
        ];
        if let Some(duration_ms) = duration_ms {
            arguments.push("--duration-ms".to_owned());
            arguments.push(duration_ms.to_string());
        }
        self.run_helper(&arguments)?;
        let mut last_tap = self
            .last_tap_point
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend tap state"))?;
        *last_tap = Some((x, y));
        Ok(())
    }

    fn hover_point(&self, x: f64, y: f64) -> Result<()> {
        self.run_helper(&[
            "move".to_owned(),
            "--x".to_owned(),
            x.to_string(),
            "--y".to_owned(),
            y.to_string(),
        ])
    }

    fn right_click_point(&self, x: f64, y: f64) -> Result<()> {
        self.run_helper(&[
            "right-click".to_owned(),
            "--x".to_owned(),
            x.to_string(),
            "--y".to_owned(),
            y.to_string(),
        ])
    }

    fn swipe_points(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        duration_ms: Option<u32>,
        delta: Option<u32>,
    ) -> Result<()> {
        let mut arguments = vec![
            "swipe".to_owned(),
            "--start-x".to_owned(),
            start.0.to_string(),
            "--start-y".to_owned(),
            start.1.to_string(),
            "--end-x".to_owned(),
            end.0.to_string(),
            "--end-y".to_owned(),
            end.1.to_string(),
            "--duration-ms".to_owned(),
            duration_ms.unwrap_or(500).to_string(),
        ];
        if let Some(delta) = delta {
            arguments.push("--delta".to_owned());
            arguments.push(delta.to_string());
        }
        self.run_helper(&arguments)
    }

    fn select_menu_item(&self, path: &[String]) -> Result<()> {
        if path.is_empty() {
            bail!("`selectMenuItem` requires at least one menu label");
        }

        let mut arguments = vec![
            "menu-item".to_owned(),
            "--bundle-id".to_owned(),
            self.bundle_id.clone(),
        ];
        for item in path {
            arguments.push("--item".to_owned());
            arguments.push(item.clone());
        }
        self.run_helper(&arguments)
    }

    fn drag_points(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        duration_ms: Option<u32>,
        delta: Option<u32>,
    ) -> Result<()> {
        let mut arguments = vec![
            "drag".to_owned(),
            "--start-x".to_owned(),
            start.0.to_string(),
            "--start-y".to_owned(),
            start.1.to_string(),
            "--end-x".to_owned(),
            end.0.to_string(),
            "--end-y".to_owned(),
            end.1.to_string(),
            "--duration-ms".to_owned(),
            duration_ms.unwrap_or(650).to_string(),
        ];
        if let Some(delta) = delta {
            arguments.push("--delta".to_owned());
            arguments.push(delta.to_string());
        }
        self.run_helper(&arguments)
    }

    fn input_text(&self, text: &str) -> Result<()> {
        let last_tap = self
            .last_tap_point
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend tap state"))?
            .to_owned();
        if let Some((x, y)) = last_tap {
            let result = self.run_helper(&[
                "set-value-at-point".to_owned(),
                "--x".to_owned(),
                x.to_string(),
                "--y".to_owned(),
                y.to_string(),
                "--text".to_owned(),
                text.to_owned(),
            ]);
            if result.is_ok() {
                return Ok(());
            }
        }
        self.run_helper(&["text".to_owned(), "--text".to_owned(), text.to_owned()])
    }

    fn press_button(&self, button: UiHardwareButton, _duration_ms: Option<u32>) -> Result<()> {
        bail!(
            "`pressButton {}` is not supported by the current macOS UI backend",
            button.summary()
        )
    }

    fn press_key(&self, key: &UiKeyPress) -> Result<()> {
        let (keycode, character) = match key.key {
            UiPressKey::Enter => (36, None),
            UiPressKey::Backspace => (51, None),
            UiPressKey::Escape | UiPressKey::Back => (53, None),
            UiPressKey::Space => (49, None),
            UiPressKey::Tab => (48, None),
            UiPressKey::Home => (115, None),
            UiPressKey::LeftArrow => (123, None),
            UiPressKey::RightArrow => (124, None),
            UiPressKey::DownArrow => (125, None),
            UiPressKey::UpArrow => (126, None),
            UiPressKey::Character(character) => (
                macos_keycode_for_character(character).with_context(|| {
                    format!(
                        "`pressKey {}` is not supported by the current macOS UI backend",
                        key.summary()
                    )
                })?,
                Some(character),
            ),
            UiPressKey::Lock
            | UiPressKey::Power
            | UiPressKey::VolumeUp
            | UiPressKey::VolumeDown => {
                bail!(
                    "`pressKey {}` is not supported by the current macOS UI backend",
                    key.summary()
                )
            }
        };
        self.focus()?;
        thread::sleep(Duration::from_millis(120));
        let mut arguments = vec![
            "key".to_owned(),
            "--bundle-id".to_owned(),
            self.bundle_id.clone(),
            "--keycode".to_owned(),
            keycode.to_string(),
        ];
        if let Some(character) = character {
            arguments.push("--character".to_owned());
            arguments.push(character.to_string());
        }
        if !key.modifiers.is_empty() {
            arguments.push("--modifiers".to_owned());
            arguments.push(
                key.modifiers
                    .iter()
                    .map(|modifier| macos_modifier_flag_name(*modifier))
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        self.run_helper(&arguments)
    }

    fn press_key_code(
        &self,
        keycode: u32,
        duration_ms: Option<u32>,
        modifiers: &[UiKeyModifier],
    ) -> Result<()> {
        self.focus()?;
        thread::sleep(Duration::from_millis(120));
        let mut arguments = vec![
            "key".to_owned(),
            "--bundle-id".to_owned(),
            self.bundle_id.clone(),
            "--keycode".to_owned(),
            keycode.to_string(),
        ];
        if let Some(duration_ms) = duration_ms {
            arguments.push("--duration-ms".to_owned());
            arguments.push(duration_ms.to_string());
        }
        if !modifiers.is_empty() {
            arguments.push("--modifiers".to_owned());
            arguments.push(
                modifiers
                    .iter()
                    .map(|modifier| macos_modifier_flag_name(*modifier))
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        self.run_helper(&arguments)
    }

    fn press_key_sequence(&self, keycodes: &[u32]) -> Result<()> {
        for keycode in keycodes {
            self.press_key_code(*keycode, None, &[])?;
        }
        Ok(())
    }

    fn take_screenshot(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        let window_info = self.window_capture_info()?;
        let temporary_capture = TempfileBuilder::new()
            .prefix("orbit-window-")
            .suffix(".png")
            .tempfile()
            .context("failed to allocate a temporary macOS screenshot path")?;
        let temp_path = temporary_capture.path().to_path_buf();
        drop(temporary_capture);

        let mut capture = Command::new("screencapture");
        capture.args([
            "-x",
            temp_path
                .to_str()
                .context("temporary screenshot path contains invalid UTF-8")?,
        ]);
        capture.stdout(Stdio::null());
        capture.stderr(Stdio::null());
        run_command(&mut capture)?;

        let mut crop = Command::new("sips");
        crop.args([
            "-c",
            &(window_info.frame.height.round() as i64).to_string(),
            &(window_info.frame.width.round() as i64).to_string(),
            "--cropOffset",
            &(window_info.frame.y.round() as i64).to_string(),
            &(window_info.frame.x.round() as i64).to_string(),
            temp_path
                .to_str()
                .context("temporary screenshot path contains invalid UTF-8")?,
            "--out",
            path.to_str()
                .context("screenshot path contains invalid UTF-8")?,
        ]);
        crop.stdout(Stdio::null());
        crop.stderr(Stdio::null());
        let result = run_command(&mut crop);
        let _ = fs::remove_file(&temp_path);
        result
    }

    fn open_link(&self, url: &str) -> Result<()> {
        let mut command = Command::new("open");
        command.arg(url);
        run_command(&mut command)
    }

    fn clear_keychain(&self) -> Result<()> {
        bail!("clearKeychain is not supported by the current macOS UI backend")
    }

    fn set_location(&self, _latitude: f64, _longitude: f64) -> Result<()> {
        bail!("setLocation is not supported by the current macOS UI backend")
    }

    fn set_permissions(&self, _bundle_id: &str, _config: &UiPermissionConfig) -> Result<()> {
        bail!("setPermissions is not supported by the current macOS UI backend")
    }

    fn travel(&self, _command: &UiTravel) -> Result<()> {
        bail!("travel is not supported by the current macOS UI backend")
    }

    fn add_media(&self, _paths: &[PathBuf]) -> Result<()> {
        bail!("addMedia is not supported by the current macOS UI backend")
    }

    fn install_dylib(&self, _path: &Path) -> Result<()> {
        bail!("install-dylib is not supported by the current macOS UI backend")
    }

    fn run_instruments(&self, _template: &str, _arguments: &[String]) -> Result<()> {
        bail!("instruments is not supported by the current macOS UI backend")
    }

    fn update_contacts(&self, _path: &Path) -> Result<()> {
        bail!("update-contacts is not supported by the current macOS UI backend")
    }

    fn list_crash_logs(&self, _query: &UiCrashQuery) -> Result<()> {
        bail!("crash log commands are not supported by the current macOS UI backend")
    }

    fn show_crash_log(&self, _name: &str) -> Result<()> {
        bail!("crash log commands are not supported by the current macOS UI backend")
    }

    fn delete_crash_logs(&self, _request: &UiCrashDeleteRequest) -> Result<()> {
        bail!("crash log commands are not supported by the current macOS UI backend")
    }

    fn stream_logs(&self, arguments: &[String]) -> Result<()> {
        let process_name = self
            .executable_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(self.bundle_id.as_str());
        let mut command = Command::new("log");
        command.arg("stream");
        if arguments.is_empty() {
            command.args(["--style", "compact", "--process", process_name]);
        } else {
            command.args(arguments);
        }
        run_command(&mut command)
    }

    fn scroll_in_direction(&self, direction: UiSwipeDirection) -> Result<()> {
        self.run_helper(&[
            "scroll".to_owned(),
            "--direction".to_owned(),
            match direction {
                UiSwipeDirection::Left => "left",
                UiSwipeDirection::Right => "right",
                UiSwipeDirection::Up => "up",
                UiSwipeDirection::Down => "down",
            }
            .to_owned(),
        ])
    }

    fn scroll_at_point(&self, direction: UiSwipeDirection, point: (f64, f64)) -> Result<()> {
        self.run_helper(&[
            "scroll-at-point".to_owned(),
            "--x".to_owned(),
            point.0.to_string(),
            "--y".to_owned(),
            point.1.to_string(),
            "--direction".to_owned(),
            match direction {
                UiSwipeDirection::Left => "left",
                UiSwipeDirection::Right => "right",
                UiSwipeDirection::Up => "up",
                UiSwipeDirection::Down => "down",
            }
            .to_owned(),
        ])
    }

    fn hide_keyboard(&self) -> Result<()> {
        Ok(())
    }

    fn start_video_recording(&mut self, path: &Path) -> Result<()> {
        if self.active_video.is_some() {
            bail!("video recording is already active for macOS");
        }
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        let window_info = self.window_capture_info()?;
        let rect = format!(
            "{},{},{},{}",
            window_info.frame.x.round() as i64,
            window_info.frame.y.round() as i64,
            window_info.frame.width.round() as i64,
            window_info.frame.height.round() as i64
        );

        let mut command = Command::new("screencapture");
        command.args([
            "-x",
            "-v",
            &format!("-R{rect}"),
            path.to_str().context("video path contains invalid UTF-8")?,
        ]);
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        let child = command.spawn().with_context(|| {
            format!(
                "failed to start macOS video recording to {}",
                path.display()
            )
        })?;
        self.active_video = Some(ActiveVideoRecording {
            path: path.to_path_buf(),
            child,
        });
        Ok(())
    }

    fn stop_video_recording(&mut self) -> Result<()> {
        let Some(mut recording) = self.active_video.take() else {
            return Ok(());
        };

        if recording.child.try_wait()?.is_none() {
            let mut interrupt = Command::new("kill");
            interrupt.args(["-INT", &recording.child.id().to_string()]);
            let _ = command_output_allow_failure(&mut interrupt)?;
        }

        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if let Some(status) = recording.child.try_wait()? {
                if !status.success() && !recording.path.exists() {
                    bail!(
                        "`screencapture -v` exited with {status} before writing {}",
                        recording.path.display()
                    );
                }
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = recording.child.kill();
        let _ = recording.child.wait();
        if recording.path.exists() {
            return Ok(());
        }

        bail!(
            "timed out waiting for macOS video recording to finish writing {}",
            recording.path.display()
        )
    }
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn macos_quit_applescript(bundle_id: &str) -> String {
    format!("tell application id \"{}\" to quit", bundle_id)
}

fn macos_xcode_log_redirect_env(selected_xcode: Option<&SelectedXcode>) -> Result<String> {
    let log_redirect_dylib = selected_xcode_log_redirect_dylib_path(selected_xcode)?;
    Ok([
        "NSUnbufferedIO=YES".to_owned(),
        "OS_LOG_TRANSLATE_PRINT_MODE=0x80".to_owned(),
        "IDE_DISABLED_OS_ACTIVITY_DT_MODE=1".to_owned(),
        "OS_LOG_DT_HOOK_MODE=0x07".to_owned(),
        "CFLOG_FORCE_DISABLE_STDERR=1".to_owned(),
        format!("DYLD_INSERT_LIBRARIES={}", log_redirect_dylib.display()),
    ]
    .join(" "))
}

fn macos_lldb_launch_command(arguments: &[(String, String)]) -> String {
    let mut command = "process launch -s -o $log_pipe -e $log_pipe".to_owned();
    let launch_arguments = arguments
        .iter()
        .flat_map(|(key, value)| [format!("-{key}"), value.clone()])
        .collect::<Vec<_>>();
    if !launch_arguments.is_empty() {
        command.push_str(" --");
        for argument in launch_arguments {
            command.push(' ');
            command.push_str(&lldb_quote_arg(&argument));
        }
    }
    command
}

fn macos_lldb_run_script(
    selected_xcode: Option<&SelectedXcode>,
    arguments: &[(String, String)],
) -> Result<String> {
    let lldb_path = selected_xcode_lldb_path(selected_xcode)?;
    let launch_command = macos_lldb_launch_command(arguments);
    Ok(format!(
        r#"set timeout -1
log_user 0

proc wait_for_prompt {{}} {{
    expect {{
        -re {{\(lldb\)}} {{ return }}
        timeout {{ send_user "timed out waiting for LLDB prompt\n"; exit 1 }}
        eof {{ send_user "LLDB exited before it became interactive\n"; exit 1 }}
    }}
}}

proc wait_for_message {{pattern message}} {{
    expect {{
        -re $pattern {{ return }}
        timeout {{ send_user "$message\n"; exit 1 }}
        eof {{ send_user "LLDB exited unexpectedly\n"; exit 1 }}
    }}
}}

set exe [lindex $argv 0]
set log_pipe [lindex $argv 1]
set lldb_path "{lldb_path}"

spawn $lldb_path $exe
wait_for_prompt
send -- "settings set target.env-vars {env_vars}\r"
wait_for_prompt
send -- "{launch_command}\r"
wait_for_message {{Process [0-9]+ launched}} "timed out waiting for LLDB to launch the macOS app"
wait_for_prompt
send -- "continue\r"
wait_for_message {{Process [0-9]+ resuming}} "timed out waiting for LLDB to continue the macOS app"
expect {{
    -re {{Process [0-9]+ exited}} {{}}
    -re {{Process [0-9]+ stopped}} {{}}
    eof {{ exit 0 }}
}}
"#,
        lldb_path = tcl_quote_arg(
            lldb_path
                .to_str()
                .context("macOS LLDB path contains invalid UTF-8")?,
        ),
        env_vars = tcl_quote_arg(&macos_xcode_log_redirect_env(selected_xcode)?),
        launch_command = launch_command,
    ))
}

fn macos_attached_launch_wrapper(
    bundle_id: &str,
    executable_path: &Path,
    lldb_script_path: &Path,
    log_pipe_path: &Path,
) -> Result<String> {
    Ok(format!(
        r#"#!/bin/zsh
set -uo pipefail

cleanup() {{
  local exit_code="${{1:-0}}"
  trap - INT TERM HUP EXIT

  if [[ -n "${{launcher_pid:-}}" ]]; then
    /usr/bin/osascript -e {quit_script} >/dev/null 2>&1 || true
    for _ in {{1..20}}; do
      if ! /usr/bin/pgrep -f {executable} >/dev/null 2>&1; then
        break
      fi
      sleep 0.1
    done
    /usr/bin/pkill -f {executable} >/dev/null 2>&1 || true
    kill -TERM "${{launcher_pid}}" >/dev/null 2>&1 || true
    wait "${{launcher_pid}}" 2>/dev/null || true
  fi

  exit "${{exit_code}}"
}}

trap 'cleanup 130' INT
trap 'cleanup 143' TERM HUP
trap 'cleanup $?' EXIT

/usr/bin/expect -f {lldb_script} {executable} {log_pipe} &
launcher_pid=$!
wait "${{launcher_pid}}"
launcher_status=$?
cleanup "${{launcher_status}}"
"#,
        quit_script = shell_quote_arg(&macos_quit_applescript(bundle_id)),
        executable = shell_quote_arg(
            executable_path
                .to_str()
                .context("macOS executable path contains invalid UTF-8")?,
        ),
        lldb_script = shell_quote_arg(
            lldb_script_path
                .to_str()
                .context("macOS LLDB script path contains invalid UTF-8")?,
        ),
        log_pipe = shell_quote_arg(
            log_pipe_path
                .to_str()
                .context("macOS log pipe path contains invalid UTF-8")?,
        ),
    ))
}

fn macos_expect_wrapper_coordinator() -> String {
    r#"set timeout -1
set wrapper [lindex $argv 0]

spawn -noecho /bin/zsh $wrapper
expect eof
"#
    .to_owned()
}

fn tcl_quote_arg(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn shell_quote_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
}

fn lldb_quote_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn ios_hid_keycode_for_character(character: char) -> Option<u32> {
    match character.to_ascii_uppercase() {
        'A'..='Z' => Some((character.to_ascii_uppercase() as u32) - ('A' as u32) + 4),
        '1'..='9' => Some((character as u32) - ('1' as u32) + 30),
        '0' => Some(39),
        _ => None,
    }
}

fn macos_keycode_for_character(character: char) -> Result<u32> {
    let uppercase = character.to_ascii_uppercase();
    let keycode = match uppercase {
        'A' => 0,
        'S' => 1,
        'D' => 2,
        'F' => 3,
        'H' => 4,
        'G' => 5,
        'Z' => 6,
        'X' => 7,
        'C' => 8,
        'V' => 9,
        'B' => 11,
        'Q' => 12,
        'W' => 13,
        'E' => 14,
        'R' => 15,
        'Y' => 16,
        'T' => 17,
        '1' => 18,
        '2' => 19,
        '3' => 20,
        '4' => 21,
        '6' => 22,
        '5' => 23,
        '=' => 24,
        '9' => 25,
        '7' => 26,
        '-' => 27,
        '8' => 28,
        '0' => 29,
        ']' => 30,
        'O' => 31,
        'U' => 32,
        '[' => 33,
        'I' => 34,
        'P' => 35,
        'L' => 37,
        'J' => 38,
        '\'' => 39,
        'K' => 40,
        ';' => 41,
        '\\' => 42,
        ',' => 43,
        '/' => 44,
        'N' => 45,
        'M' => 46,
        '.' => 47,
        '`' => 50,
        _ => bail!("unsupported character `{character}` for macOS keyboard input"),
    };
    Ok(keycode)
}

fn macos_modifier_flag_name(modifier: UiKeyModifier) -> &'static str {
    match modifier {
        UiKeyModifier::Command => "command",
        UiKeyModifier::Shift => "shift",
        UiKeyModifier::Option => "option",
        UiKeyModifier::Control => "control",
        UiKeyModifier::Function => "function",
    }
}

fn apply_simulator_permission(
    backend: &IosSimulatorBackend,
    bundle_id: &str,
    permission: &super::UiPermissionSetting,
    service: &str,
) -> Result<()> {
    match permission.state {
        UiPermissionState::Allow => backend.run_simctl_privacy("grant", service, bundle_id),
        UiPermissionState::Deny => backend.run_simctl_privacy("revoke", service, bundle_id),
        UiPermissionState::Unset => {
            let mut command = xcrun_command(backend.selected_xcode.as_ref());
            command.args([
                "simctl",
                "privacy",
                &backend.device.udid,
                "reset",
                service,
                bundle_id,
            ]);
            run_command(&mut command)
        }
    }
}

fn idb_requirement_message() -> &'static str {
    super::idb_requirement_message()
}

fn macos_requirement_message() -> &'static str {
    "Orbit macOS UI automation requires Accessibility access and the built-in Swift toolchain on this Mac"
}

pub(crate) fn macos_doctor(project: &ProjectContext) -> Result<MacosDoctorStatus> {
    let helper_path = ensure_macos_driver_binary(project)?;
    let mut command = Command::new(helper_path);
    command.arg("doctor");
    let output = command_output(&mut command).with_context(macos_requirement_message)?;
    serde_json::from_str(&output).context("failed to parse macOS UI doctor output")
}

fn ensure_macos_driver_binary(project: &ProjectContext) -> Result<PathBuf> {
    let tools_dir = project.project_paths.orbit_dir.join("tools");
    ensure_dir(&tools_dir)?;
    let binary_path = tools_dir.join("orbit-macos-ui-driver");
    let source_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("apple")
        .join("testing")
        .join("ui")
        .join("macos_driver.swift");

    let should_build = if !binary_path.exists() {
        true
    } else {
        let source_modified = fs::metadata(&source_path)
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("failed to read {}", source_path.display()))?;
        let binary_modified = fs::metadata(&binary_path)
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("failed to read {}", binary_path.display()))?;
        source_modified > binary_modified
    };
    if !should_build {
        return Ok(binary_path);
    }

    let mut command = xcrun_command(project.selected_xcode.as_ref());
    command.args(["--sdk", "macosx", "swiftc", "-O"]);
    command.arg(&source_path);
    command.arg("-o");
    command.arg(&binary_path);
    let (stdout, stderr) = run_command_capture(&mut command).with_context(|| {
        format!(
            "failed to compile macOS UI helper from {}",
            source_path.display()
        )
    })?;
    if !stdout.trim().is_empty() {
        eprintln!("{stdout}");
    }
    if !stderr.trim().is_empty() {
        eprintln!("{stderr}");
    }
    Ok(binary_path)
}
