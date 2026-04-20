use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;

use super::super::{
    UiCrashDeleteRequest, UiCrashQuery, UiHardwareButton, UiKeyModifier, UiKeyPress,
    UiLocationPoint, UiPermissionConfig, UiPermissionSetting, UiPermissionState, UiPressKey,
    UiSwipeDirection, UiTravel,
};
use super::{ActiveVideoRecording, UiBackend};
use crate::apple::simulator::{SimulatorDevice, select_simulator_device};
use crate::apple::xcode::{SelectedXcode, xcrun_command};
use crate::context::ProjectContext;
use crate::util::{
    command_output, command_output_allow_failure, ensure_dir, run_command, run_command_capture,
};

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
        run_command(&mut command).with_context(super::idb_requirement_message)
    }

    fn idb_output(&self, arguments: &[String]) -> Result<String> {
        let mut command = Command::new("idb");
        command.args(arguments);
        command.arg("--udid").arg(&self.device.udid);
        command_output(&mut command).with_context(super::idb_requirement_message)
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
        run_command(&mut command).with_context(super::idb_requirement_message)
    }
}

impl UiBackend for IosSimulatorBackend {
    fn backend_name(&self) -> &'static str {
        "orbi-idb-ios-simulator"
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
            run_command(&mut command).with_context(super::idb_requirement_message)
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
                "clearState currently supports only Orbi's built app `{}` on iOS simulators",
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
        run_command(&mut command).with_context(super::idb_requirement_message)
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
        run_command(&mut command).with_context(super::idb_requirement_message)
    }

    fn scroll_in_direction(&self, direction: UiSwipeDirection) -> Result<()> {
        let tree = self.describe_all()?;
        let screen = super::super::matching::infer_screen_frame(&tree)
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
        let screen = super::super::matching::infer_screen_frame(&tree)
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
        let screen = super::super::matching::infer_screen_frame(&tree)
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
            .with_context(super::idb_requirement_message)?;
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

        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
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

fn ios_hid_keycode_for_character(character: char) -> Option<u32> {
    match character.to_ascii_uppercase() {
        'A'..='Z' => Some((character.to_ascii_uppercase() as u32) - ('A' as u32) + 4),
        '1'..='9' => Some((character as u32) - ('1' as u32) + 30),
        '0' => Some(39),
        _ => None,
    }
}

fn apply_simulator_permission(
    backend: &IosSimulatorBackend,
    bundle_id: &str,
    permission: &UiPermissionSetting,
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
