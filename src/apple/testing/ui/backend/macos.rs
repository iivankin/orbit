use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;
use tempfile::{Builder as TempfileBuilder, TempDir, tempdir};

use super::super::{
    UiCrashDeleteRequest, UiCrashQuery, UiHardwareButton, UiKeyModifier, UiKeyPress,
    UiPermissionConfig, UiPressKey, UiSelector, UiSwipeDirection, UiTravel,
};
use super::{ActiveVideoRecording, MacosDoctorStatus, MacosWindowInfo, UiBackend};
use crate::apple::build::pipeline::macos_executable_path;
use crate::apple::logs::MacosInferiorLogRelay;
use crate::apple::script::{
    macos_quit_applescript, macos_xcode_log_redirect_env, shell_quote_arg, tcl_quote_arg,
};
use crate::apple::xcode::{SelectedXcode, lldb_path as selected_xcode_lldb_path, xcrun_command};
use crate::context::ProjectContext;
use crate::util::{
    command_output, command_output_allow_failure, ensure_dir, run_command, run_command_capture,
};

pub struct MacosBackend {
    helper_path: PathBuf,
    bundle_id: String,
    executable_path: PathBuf,
    selected_xcode: Option<SelectedXcode>,
    verbose: bool,
    pinned_target_pid: Mutex<Option<u32>>,
    launched_session: Mutex<Option<MacosLaunchedSession>>,
    last_tap_point: Mutex<Option<(f64, f64)>>,
    active_video: Option<ActiveVideoRecording>,
}

struct MacosLaunchedSession {
    launched_pid: u32,
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
            pinned_target_pid: Mutex::new(None),
            launched_session: Mutex::new(None),
            last_tap_point: Mutex::new(None),
            active_video: None,
        })
    }

    fn ensure_owned_bundle(&self, bundle_id: &str, action: &str) -> Result<()> {
        if bundle_id == self.bundle_id {
            return Ok(());
        }
        bail!(
            "{action} currently supports only Orbit's built app `{}` on macOS",
            self.bundle_id
        )
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

    fn target_selector_arguments(&self) -> Result<Vec<String>> {
        let pinned_pid = *self
            .pinned_target_pid
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend target state"))?;
        Ok(match pinned_pid {
            Some(pid) => vec!["--pid".to_owned(), pid.to_string()],
            None => vec!["--bundle-id".to_owned(), self.bundle_id.clone()],
        })
    }

    fn pin_running_target_pid(&self, pid: u32) -> Result<()> {
        *self
            .pinned_target_pid
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend target state"))? =
            Some(pid);
        Ok(())
    }

    fn wait_for_running_process_pid(
        &self,
        executable_path: &Path,
        ignored_pid: Option<u32>,
    ) -> Result<u32> {
        let executable = executable_path
            .to_str()
            .context("macOS traced executable path contains invalid UTF-8")?;
        let started = Instant::now();
        let mut last_seen_pids = Vec::new();
        while started.elapsed() < Duration::from_secs(10) {
            let mut command = Command::new("pgrep");
            command.args(["-f", executable]);
            let (success, stdout, _stderr) = command_output_allow_failure(&mut command)?;
            if success {
                let pids = traced_process_candidates(&stdout, ignored_pid);
                if pids.len() == 1 {
                    return Ok(pids[0]);
                }
                if !pids.is_empty() {
                    last_seen_pids = pids;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }

        if last_seen_pids.len() > 1 {
            bail!(
                "timed out waiting for traced macOS app `{}` to settle to a single process; saw pids {:?}",
                executable_path.display(),
                last_seen_pids
            );
        }
        bail!(
            "timed out waiting for traced macOS app process `{}` to appear",
            executable_path.display()
        )
    }

    fn start_attached_log_session(
        &self,
        arguments: &[(String, String)],
    ) -> Result<MacosLaunchedSession> {
        let launch_dir = tempdir().context("failed to create macOS UI launch tempdir")?;
        let log_pipe = launch_dir.path().join("inferior-stdio.pipe");
        let pid_file = launch_dir.path().join("inferior-pid.txt");
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
                pid_file.as_path(),
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
        let mut child = child
            .spawn()
            .with_context(|| format!("failed to start `{}` under LLDB", self.bundle_id))?;
        let launched_pid = match wait_for_launched_app_pid(&pid_file) {
            Ok(pid) => pid,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        Ok(MacosLaunchedSession {
            launched_pid,
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
            let mut arguments = vec!["window-info".to_owned()];
            arguments.extend(self.target_selector_arguments()?);
            command.args(arguments);
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
            let mut arguments = vec!["focus".to_owned()];
            arguments.extend(self.target_selector_arguments()?);
            command.args(arguments);
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
        let mut arguments = vec!["describe-all".to_owned()];
        arguments.extend(self.target_selector_arguments()?);
        let output = self.helper_output(&arguments)?;
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
        self.ensure_owned_bundle(bundle_id, "launchApp")?;
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
        self.pin_running_target_pid(session.launched_pid)?;
        *self
            .launched_session
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend process state"))? =
            Some(session);
        self.window_capture_info()?;
        self.wait_for_focusable_app()
    }

    fn stop_app(&self, bundle_id: &str) -> Result<()> {
        self.ensure_owned_bundle(bundle_id, "stopApp")?;
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
        *self
            .pinned_target_pid
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend target state"))? =
            None;
        Ok(())
    }

    fn clear_app_state(&self, bundle_id: &str) -> Result<()> {
        self.ensure_owned_bundle(bundle_id, "clearState")?;
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
        let mut arguments = vec!["focus".to_owned()];
        arguments.extend(self.target_selector_arguments()?);
        self.run_helper(&arguments)
    }

    fn pin_running_target_by_executable(
        &self,
        executable_path: &Path,
        ignored_pid: Option<u32>,
    ) -> Result<()> {
        let pid = self.wait_for_running_process_pid(executable_path, ignored_pid)?;
        self.pin_running_target_pid(pid)
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

    fn activate_selector(&self, selector: &UiSelector) -> Result<bool> {
        if selector.id.is_none() && selector.text.is_none() {
            return Ok(false);
        }

        self.focus()?;
        thread::sleep(Duration::from_millis(80));

        let mut arguments = vec!["activate-element".to_owned()];
        arguments.extend(self.target_selector_arguments()?);
        if let Some(id) = selector.id.as_ref() {
            arguments.push("--id".to_owned());
            arguments.push(id.clone());
        }
        if let Some(text) = selector.text.as_ref() {
            arguments.push("--text".to_owned());
            arguments.push(text.clone());
        }
        self.run_helper(&arguments)?;
        Ok(true)
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

        let mut arguments = vec!["menu-item".to_owned()];
        arguments.extend(self.target_selector_arguments()?);
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
            "--keycode".to_owned(),
            keycode.to_string(),
        ];
        arguments.splice(1..1, self.target_selector_arguments()?);
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
            "--keycode".to_owned(),
            keycode.to_string(),
        ];
        arguments.splice(1..1, self.target_selector_arguments()?);
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

fn traced_process_candidates(stdout: &str, ignored_pid: Option<u32>) -> Vec<u32> {
    let mut pids = stdout
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    if let Some(ignored_pid) = ignored_pid {
        pids.retain(|pid| *pid != ignored_pid);
    }
    pids
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

fn wait_for_launched_app_pid(pid_file: &Path) -> Result<u32> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if let Ok(contents) = fs::read_to_string(pid_file) {
            let pid_text = contents.trim();
            if !pid_text.is_empty() {
                let pid = pid_text.parse::<u32>().with_context(|| {
                    format!(
                        "failed to parse launched macOS app pid from `{}`",
                        pid_file.display()
                    )
                })?;
                return Ok(pid);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!(
        "timed out waiting for the launched macOS app pid in `{}`",
        pid_file.display()
    )
}

fn macos_lldb_launch_arguments(arguments: &[(String, String)]) -> Vec<String> {
    arguments
        .iter()
        .flat_map(|(key, value)| [format!("-{key}"), value.clone()])
        .collect()
}

fn macos_tcl_list_items(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", tcl_quote_arg(value)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn macos_lldb_launch_command_setup(arguments: &[(String, String)]) -> String {
    let launch_arguments = macos_lldb_launch_arguments(arguments);
    let launch_argument_items = macos_tcl_list_items(&launch_arguments);
    format!(
        r#"set launch_arguments [list {launch_argument_items}]
set launch_parts [list process launch -s -o $log_pipe -e $log_pipe]
if {{[llength $launch_arguments] > 0}} {{
    lappend launch_parts --
    foreach arg $launch_arguments {{
        set escaped_arg [string map [list "\\" "\\\\" "\"" "\\\""] $arg]
        lappend launch_parts "\"$escaped_arg\""
    }}
}}
set launch_command [join $launch_parts " "]
"#
    )
}

fn macos_lldb_run_script(
    selected_xcode: Option<&SelectedXcode>,
    arguments: &[(String, String)],
) -> Result<String> {
    let lldb_path = selected_xcode_lldb_path(selected_xcode)?;
    let launch_command_setup = macos_lldb_launch_command_setup(arguments);
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

proc wait_for_launch_and_record_pid {{pattern pid_file message}} {{
    expect {{
        -re $pattern {{
            set inferior_pid $expect_out(1,string)
            set file_handle [open $pid_file "w"]
            puts $file_handle $inferior_pid
            close $file_handle
            return
        }}
        timeout {{ send_user "$message\n"; exit 1 }}
        eof {{ send_user "LLDB exited unexpectedly\n"; exit 1 }}
    }}
}}

set exe [lindex $argv 0]
set log_pipe [lindex $argv 1]
set pid_file [lindex $argv 2]
set lldb_path "{lldb_path}"
{launch_command_setup}

spawn $lldb_path $exe
wait_for_prompt
send -- "settings set target.env-vars {env_vars}\r"
wait_for_prompt
send -- "$launch_command\r"
wait_for_launch_and_record_pid {{Process ([0-9]+) launched}} $pid_file "timed out waiting for LLDB to launch the macOS app"
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
        launch_command_setup = launch_command_setup,
    ))
}

fn macos_attached_launch_wrapper(
    bundle_id: &str,
    executable_path: &Path,
    lldb_script_path: &Path,
    log_pipe_path: &Path,
    pid_file_path: &Path,
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

 /usr/bin/expect -f {lldb_script} {executable} {log_pipe} {pid_file} &
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
        pid_file = shell_quote_arg(
            pid_file_path
                .to_str()
                .context("macOS pid file path contains invalid UTF-8")?,
        ),
    ))
}

fn macos_expect_wrapper_coordinator() -> String {
    r"set timeout -1
set wrapper [lindex $argv 0]

spawn -noecho /bin/zsh $wrapper
expect eof
"
    .to_owned()
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

#[cfg(test)]
mod tests {
    use super::{
        macos_attached_launch_wrapper, macos_lldb_launch_arguments,
        macos_lldb_launch_command_setup, traced_process_candidates, wait_for_launched_app_pid,
    };
    use std::fs;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn filters_launch_recorder_pid_from_traced_process_candidates() {
        let pids = traced_process_candidates("30368\n30421\n", Some(30368));
        assert_eq!(pids, vec![30421]);
    }

    #[test]
    fn keeps_unique_sorted_traced_process_candidates() {
        let pids = traced_process_candidates("42\n7\n42\n", None);
        assert_eq!(pids, vec![7, 42]);
    }

    #[test]
    fn flattens_launch_argument_pairs_for_lldb() {
        let arguments = macos_lldb_launch_arguments(&[
            ("mockOpenAIOAuth".to_owned(), "instant_success".to_owned()),
            ("mockOpenAIEmail".to_owned(), "qa@example.com".to_owned()),
        ]);
        assert_eq!(
            arguments,
            vec![
                "-mockOpenAIOAuth".to_owned(),
                "instant_success".to_owned(),
                "-mockOpenAIEmail".to_owned(),
                "qa@example.com".to_owned(),
            ]
        );
    }

    #[test]
    fn lldb_launch_setup_uses_tcl_list_instead_of_inline_quoted_send() {
        let setup = macos_lldb_launch_command_setup(&[
            ("mockOpenAIOAuth".to_owned(), "instant_success".to_owned()),
            ("mockOpenAIEmail".to_owned(), "qa@example.com".to_owned()),
        ]);
        assert!(setup.contains(
            r#"set launch_arguments [list "-mockOpenAIOAuth" "instant_success" "-mockOpenAIEmail" "qa@example.com"]"#
        ));
        assert!(setup.contains(r#"foreach arg $launch_arguments {"#));
        assert!(setup.contains(r#"lappend launch_parts "\"$escaped_arg\"""#));
    }

    #[test]
    fn attached_launch_wrapper_passes_pid_file_to_expect() {
        let wrapper = macos_attached_launch_wrapper(
            "sh.orbit.desktop",
            Path::new("/tmp/Accord.app/Contents/MacOS/Accord"),
            Path::new("/tmp/run.expect"),
            Path::new("/tmp/inferior-stdio.pipe"),
            Path::new("/tmp/inferior-pid.txt"),
        )
        .unwrap();
        assert!(wrapper.contains("/usr/bin/expect -f"));
        assert!(wrapper.contains("/tmp/run.expect"));
        assert!(wrapper.contains("/tmp/Accord.app/Contents/MacOS/Accord"));
        assert!(wrapper.contains("/tmp/inferior-stdio.pipe"));
        assert!(wrapper.contains("/tmp/inferior-pid.txt"));
    }

    #[test]
    fn waits_for_launched_app_pid_file() {
        let temp = tempdir().unwrap();
        let pid_file = temp.path().join("inferior-pid.txt");
        let writer_path = pid_file.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(120));
            fs::write(writer_path, "43210\n").unwrap();
        });

        let pid = wait_for_launched_app_pid(&pid_file).unwrap();
        writer.join().unwrap();
        assert_eq!(pid, 43210);
    }
}
