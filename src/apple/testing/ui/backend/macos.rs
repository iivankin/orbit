use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use tempfile::{TempDir, tempdir};

use super::super::matching::{find_visible_element_by_selector, find_visible_scroll_container};
use super::super::{
    UiCrashDeleteRequest, UiCrashQuery, UiHardwareButton, UiKeyModifier, UiKeyPress,
    UiPermissionConfig, UiPressKey, UiSelector, UiSwipeDirection, UiTravel,
};
use super::{ActiveVideoRecording, MacosDoctorStatus, MacosWindowInfo, UiBackend};
use crate::apple::build::pipeline::macos_executable_path;
use crate::apple::logs::MacosInferiorLogRelay;
use crate::apple::xcode::{
    SelectedXcode, log_redirect_dylib_path as selected_xcode_log_redirect_dylib_path, xcrun_command,
};
use crate::context::ProjectContext;
use crate::util::{
    command_output, command_output_allow_failure, ensure_dir, run_command, run_command_capture,
    timestamp_slug,
};

pub struct MacosBackend {
    helper_path: PathBuf,
    bridge_dylib_path: PathBuf,
    bundle_id: String,
    bundle_path: PathBuf,
    executable_path: PathBuf,
    selected_xcode: Option<SelectedXcode>,
    verbose: bool,
    pinned_target_pid: Mutex<Option<u32>>,
    launched_session: Mutex<Option<MacosLaunchedSession>>,
    pending_trace_launch: Mutex<Option<PendingTraceLaunch>>,
    last_tap_point: Mutex<Option<(f64, f64)>>,
    active_video: Option<ActiveVideoRecording>,
}

struct MacosLaunchedSession {
    launched_pid: u32,
    _launch_dir: TempDir,
    log_pipe_anchor: Option<fs::File>,
    log_relay: Option<MacosInferiorLogRelay>,
    bridge_directory: PathBuf,
    bridge_notification_name: String,
    previous_frontmost_pid: Option<u32>,
}

impl MacosLaunchedSession {
    fn finish_logging(&mut self) {
        self.log_pipe_anchor.take();
        if let Some(mut relay) = self.log_relay.take() {
            relay.stop();
        }
    }
}

struct PendingTraceLaunch {
    launch_dir: TempDir,
    log_pipe_anchor: fs::File,
    log_relay: MacosInferiorLogRelay,
    launch_id: String,
    registration_path: PathBuf,
    bridge_directory: PathBuf,
    bridge_notification_name: String,
    previous_frontmost_pid: Option<u32>,
}

struct PreparedMacosLaunchSupport {
    launch_dir: TempDir,
    log_pipe_anchor: fs::File,
    log_relay: MacosInferiorLogRelay,
    bridge_directory: PathBuf,
    bridge_notification_name: String,
    launch_environment: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct MacosFrontmostApplication {
    pid: Option<u32>,
}

#[derive(Deserialize)]
struct MacosLaunchedApplication {
    pid: u32,
}

#[derive(Deserialize)]
struct MacosTraceLaunchRegistration {
    pid: u32,
    #[serde(rename = "launchId")]
    launch_id: String,
    #[serde(rename = "bundleId")]
    bundle_id: String,
}

impl MacosBackend {
    pub fn prepare(
        project: &ProjectContext,
        receipt: &crate::apple::build::receipt::BuildReceipt,
    ) -> Result<Self> {
        Ok(Self {
            helper_path: ensure_macos_driver_binary(project)?,
            bridge_dylib_path: ensure_macos_bridge_dylib(project)?,
            bundle_id: receipt.bundle_id.clone(),
            bundle_path: receipt.bundle_path.clone(),
            executable_path: macos_executable_path(receipt)?,
            selected_xcode: project.selected_xcode.clone(),
            verbose: project.app.verbose,
            pinned_target_pid: Mutex::new(None),
            launched_session: Mutex::new(None),
            pending_trace_launch: Mutex::new(None),
            last_tap_point: Mutex::new(None),
            active_video: None,
        })
    }

    fn ensure_owned_bundle(&self, bundle_id: &str, action: &str) -> Result<()> {
        if bundle_id == self.bundle_id {
            return Ok(());
        }
        bail!(
            "{action} currently supports only Orbi's built app `{}` on macOS",
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
        terminate_macos_process_tree(session.launched_pid)?;
        session.finish_logging();
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

    fn bridge_arguments(&self) -> Result<Option<Vec<String>>> {
        let session = self
            .launched_session
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend process state"))?;
        let Some(session) = session.as_ref() else {
            return Ok(None);
        };

        Ok(Some(vec![
            "--bridge-dir".to_owned(),
            session
                .bridge_directory
                .to_str()
                .context("macOS UI bridge directory contains invalid UTF-8")?
                .to_owned(),
            "--bridge-name".to_owned(),
            session.bridge_notification_name.clone(),
        ]))
    }

    fn set_pinned_target_pid(&self, pid: u32) -> Result<()> {
        *self
            .pinned_target_pid
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend target state"))? =
            Some(pid);
        Ok(())
    }

    fn prepare_launch_support(&self, tempdir_context: &str) -> Result<PreparedMacosLaunchSupport> {
        let launch_dir = tempdir().with_context(|| tempdir_context.to_owned())?;
        let bridge_directory = launch_dir.path().join("ui-bridge");
        ensure_dir(&bridge_directory)?;
        let bridge_notification_name = format!(
            "dev.orbi.ui.{}",
            launch_dir
                .path()
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("bridge")
        );
        let log_pipe = launch_dir.path().join("inferior-stdio.pipe");

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
        let launch_environment = macos_ui_bridge_launch_environment(
            self.selected_xcode.as_ref(),
            &self.bridge_dylib_path,
            &bridge_directory,
            &bridge_notification_name,
            &log_pipe,
        )?;

        Ok(PreparedMacosLaunchSupport {
            launch_dir,
            log_pipe_anchor,
            log_relay,
            bridge_directory,
            bridge_notification_name,
            launch_environment,
        })
    }

    fn wait_for_trace_launch_registration(
        &self,
        pending: &PendingTraceLaunch,
    ) -> Result<MacosTraceLaunchRegistration> {
        let started = Instant::now();
        let mut last_error = None;
        while started.elapsed() < Duration::from_secs(10) {
            if let Some(registration) = read_trace_launch_registration(&pending.registration_path)?
            {
                if registration.launch_id != pending.launch_id {
                    last_error = Some(format!(
                        "trace launch registration `{}` reported unexpected launch id `{}`",
                        pending.registration_path.display(),
                        registration.launch_id
                    ));
                } else if registration.bundle_id != self.bundle_id {
                    last_error = Some(format!(
                        "trace launch registration `{}` reported unexpected bundle `{}`",
                        pending.registration_path.display(),
                        registration.bundle_id
                    ));
                } else if !process_is_running(registration.pid)? {
                    last_error = Some(format!(
                        "trace launch registration `{}` reported stale pid `{}`",
                        pending.registration_path.display(),
                        registration.pid
                    ));
                } else {
                    return Ok(registration);
                }
            }
            thread::sleep(Duration::from_millis(50));
        }

        match last_error {
            Some(detail) => Err(anyhow::anyhow!(detail)),
            None => bail!(
                "timed out waiting for traced macOS app `{}` to register launch `{}`",
                self.bundle_id,
                pending.launch_id
            ),
        }
    }

    fn start_attached_log_session(
        &self,
        arguments: &[(String, String)],
    ) -> Result<MacosLaunchedSession> {
        let PreparedMacosLaunchSupport {
            launch_dir,
            log_pipe_anchor,
            log_relay,
            bridge_directory,
            bridge_notification_name,
            launch_environment,
        } = self.prepare_launch_support("failed to create macOS UI launch tempdir")?;
        let launch_arguments = macos_launch_arguments(arguments);
        let bundle_path = self
            .bundle_path
            .to_str()
            .context("macOS app bundle path contains invalid UTF-8")?;
        let mut command = Command::new(&self.helper_path);
        command.args(["launch-app", "--app-path", bundle_path]);
        for argument in launch_arguments {
            command.arg("--argument").arg(argument);
        }
        for (key, value) in launch_environment {
            command.arg("--env").arg(format!("{key}={value}"));
        }
        let output = command_output(&mut command)
            .with_context(|| format!("failed to launch `{}` without activation", self.bundle_id))?;
        let launched: MacosLaunchedApplication =
            serde_json::from_str(&output).context("failed to parse macOS launch helper output")?;

        Ok(MacosLaunchedSession {
            launched_pid: launched.pid,
            _launch_dir: launch_dir,
            log_pipe_anchor: Some(log_pipe_anchor),
            log_relay: Some(log_relay),
            bridge_directory,
            bridge_notification_name,
            previous_frontmost_pid: None,
        })
    }

    fn prepare_pending_trace_launch(
        &self,
        previous_frontmost_pid: Option<u32>,
    ) -> Result<Vec<(String, String)>> {
        let PreparedMacosLaunchSupport {
            launch_dir,
            log_pipe_anchor,
            log_relay,
            bridge_directory,
            bridge_notification_name,
            mut launch_environment,
        } = self.prepare_launch_support("failed to create macOS traced-session tempdir")?;
        let launch_id = timestamp_slug();
        let registration_path = bridge_directory.join(format!("trace-launch-{launch_id}.json"));
        launch_environment.push((
            "ORBI_MACOS_UI_TRACE_LAUNCH_ID".to_owned(),
            launch_id.clone(),
        ));
        launch_environment.push((
            "ORBI_MACOS_UI_TRACE_REGISTRATION_PATH".to_owned(),
            registration_path.display().to_string(),
        ));
        launch_environment.push((
            "ORBI_MACOS_UI_TRACE_EXPECTED_BUNDLE_ID".to_owned(),
            self.bundle_id.clone(),
        ));

        *self
            .pending_trace_launch
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS traced launch state"))? =
            Some(PendingTraceLaunch {
                launch_dir,
                log_pipe_anchor,
                log_relay,
                launch_id,
                registration_path,
                bridge_directory,
                bridge_notification_name,
                previous_frontmost_pid,
            });

        Ok(launch_environment)
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

    fn wait_for_actionable_app(&self) -> Result<()> {
        let started = Instant::now();
        let mut last_error = None;
        while started.elapsed() < Duration::from_secs(10) {
            let mut command = Command::new(&self.helper_path);
            let mut arguments = vec!["describe-all".to_owned()];
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
            None => bail!("timed out waiting for macOS app accessibility tree"),
        }
    }

    fn wait_for_bridge_ready(&self) -> Result<()> {
        let Some(bridge_arguments) = self.bridge_arguments()? else {
            return Ok(());
        };

        let started = Instant::now();
        let mut last_error = None;
        while started.elapsed() < Duration::from_secs(5) {
            let mut command = Command::new(&self.helper_path);
            let mut arguments = vec!["bridge-ping".to_owned()];
            arguments.extend(bridge_arguments.clone());
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
            None => bail!("timed out waiting for the injected macOS UI bridge"),
        }
    }

    fn frontmost_application_pid(&self) -> Result<Option<u32>> {
        let mut command = Command::new(&self.helper_path);
        command.arg("frontmost-application");
        let (success, stdout, _stderr) = command_output_allow_failure(&mut command)?;
        if !success {
            return Ok(None);
        }
        let info: MacosFrontmostApplication =
            serde_json::from_str(&stdout).context("failed to parse macOS frontmost app")?;
        Ok(info.pid)
    }

    fn restore_frontmost_application(
        &self,
        pid: Option<u32>,
        ignored_pid: Option<u32>,
    ) -> Result<()> {
        let Some(pid) = pid else {
            return Ok(());
        };
        if Some(pid) == ignored_pid {
            return Ok(());
        }

        let pid_string = pid.to_string();
        let mut command = Command::new(&self.helper_path);
        command.args(["focus", "--pid", pid_string.as_str()]);
        let _ = command_output_allow_failure(&mut command)?;
        Ok(())
    }

    fn reopen_window(&self) -> Result<()> {
        let started = Instant::now();
        let mut last_error = None;
        while started.elapsed() < Duration::from_secs(10) {
            let mut command = Command::new(&self.helper_path);
            let mut arguments = vec!["reopen-app".to_owned()];
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
        "orbi-ax-macos"
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
        let has_running_session = if stop_app {
            false
        } else {
            let mut launched_session = self.launched_session.lock().map_err(|_| {
                anyhow::anyhow!("failed to lock the macOS UI backend process state")
            })?;
            if let Some(existing_session) = launched_session.as_ref() {
                if process_is_running(existing_session.launched_pid)? {
                    true
                } else {
                    launched_session.take();
                    *self.pinned_target_pid.lock().map_err(|_| {
                        anyhow::anyhow!("failed to lock the macOS UI backend target state")
                    })? = None;
                    false
                }
            } else {
                false
            }
        };
        if has_running_session {
            let result = (|| -> Result<()> {
                self.reopen_window()?;
                self.window_capture_info()?;
                self.wait_for_actionable_app()?;
                self.wait_for_bridge_ready()
            })();
            return result;
        }

        let session = self.start_attached_log_session(arguments)?;
        let launched_pid = session.launched_pid;
        self.set_pinned_target_pid(launched_pid)?;
        *self
            .launched_session
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend process state"))? =
            Some(session);
        (|| -> Result<()> {
            self.reopen_window()?;
            self.window_capture_info()?;
            self.wait_for_actionable_app()?;
            self.wait_for_bridge_ready()
        })()
    }

    fn stop_app(&self, bundle_id: &str) -> Result<()> {
        self.ensure_owned_bundle(bundle_id, "stopApp")?;
        self.stop_launched_process()?;

        let mut pinned_target_pid = self
            .pinned_target_pid
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend target state"))?;
        if let Some(pid) = *pinned_target_pid {
            terminate_macos_process_tree(pid)?;
        }
        *pinned_target_pid = None;
        Ok(())
    }

    fn clear_app_state(&self, bundle_id: &str) -> Result<()> {
        self.ensure_owned_bundle(bundle_id, "clearState")?;
        self.stop_app(bundle_id)?;

        // Keep the cleanup scoped to bundle-specific storage roots so Orbi does not
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

    fn frontmost_application_pid(&self) -> Result<Option<u32>> {
        MacosBackend::frontmost_application_pid(self)
    }

    fn pin_pending_trace_launch(&self) -> Result<()> {
        let pending_trace_launch = self
            .pending_trace_launch
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS traced launch state"))?
            .take();
        let session = if let Some(mut pending) = pending_trace_launch {
            let pid = match self.wait_for_trace_launch_registration(&pending) {
                Ok(registration) => registration.pid,
                Err(error) => {
                    pending.log_relay.stop();
                    return Err(error);
                }
            };
            self.set_pinned_target_pid(pid)?;
            MacosLaunchedSession {
                launched_pid: pid,
                _launch_dir: pending.launch_dir,
                log_pipe_anchor: Some(pending.log_pipe_anchor),
                log_relay: Some(pending.log_relay),
                bridge_directory: pending.bridge_directory,
                bridge_notification_name: pending.bridge_notification_name,
                previous_frontmost_pid: pending.previous_frontmost_pid,
            }
        } else {
            bail!("missing pending macOS trace launch registration")
        };
        *self
            .launched_session
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend process state"))? =
            Some(session);
        Ok(())
    }

    fn prepare_trace_launch_environment(
        &self,
        previous_frontmost_pid: Option<u32>,
    ) -> Result<Vec<(String, String)>> {
        self.prepare_pending_trace_launch(previous_frontmost_pid)
    }

    fn abort_pending_trace_launch(&self) -> Result<()> {
        let pending = self
            .pending_trace_launch
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS traced launch state"))?
            .take();
        let Some(mut pending) = pending else {
            return Ok(());
        };

        if let Some(registration) = read_trace_launch_registration(&pending.registration_path)?
            && registration.launch_id == pending.launch_id
            && registration.bundle_id == self.bundle_id
        {
            terminate_macos_process_tree(registration.pid)?;
        }
        pending.log_relay.stop();
        Ok(())
    }

    fn prepare_external_running_target(&self) -> Result<()> {
        if let Err(error) = self.reopen_window()
            && !error.to_string().contains("procNotFound")
        {
            return Err(error);
        }
        self.window_capture_info()?;
        self.wait_for_actionable_app()?;
        self.wait_for_bridge_ready()?;

        let restore_target = self
            .launched_session
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend process state"))?
            .as_ref()
            .and_then(|session| session.previous_frontmost_pid);
        if let Some(previous_frontmost_pid) = restore_target {
            let ignored_pid = *self
                .pinned_target_pid
                .lock()
                .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend target state"))?;
            let restore_deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < restore_deadline {
                if self.frontmost_application_pid()? == ignored_pid {
                    self.restore_frontmost_application(Some(previous_frontmost_pid), ignored_pid)?;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        Ok(())
    }

    fn tap_point(&self, x: f64, y: f64, duration_ms: Option<u32>) -> Result<()> {
        let mut arguments = vec![
            "tap".to_owned(),
            "--x".to_owned(),
            x.to_string(),
            "--y".to_owned(),
            y.to_string(),
        ];
        if let Some(bridge_arguments) = self.bridge_arguments()? {
            arguments.extend(bridge_arguments);
        }
        arguments.extend(self.target_selector_arguments()?);
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

        let matched_center = self
            .describe_all()
            .ok()
            .and_then(|tree| find_visible_element_by_selector(&tree, selector))
            .and_then(|element| element.frame.map(|frame| frame.center()));

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
        if let Some((x, y)) = matched_center {
            let mut last_tap = self
                .last_tap_point
                .lock()
                .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend tap state"))?;
            *last_tap = Some((x, y));
        }
        Ok(true)
    }

    fn hover_point(&self, x: f64, y: f64) -> Result<()> {
        let mut arguments = vec![
            "move".to_owned(),
            "--x".to_owned(),
            x.to_string(),
            "--y".to_owned(),
            y.to_string(),
        ];
        if let Some(bridge_arguments) = self.bridge_arguments()? {
            arguments.extend(bridge_arguments);
        }
        arguments.extend(self.target_selector_arguments()?);
        self.run_helper(&arguments)
    }

    fn right_click_point(&self, x: f64, y: f64) -> Result<()> {
        let mut arguments = vec![
            "right-click".to_owned(),
            "--x".to_owned(),
            x.to_string(),
            "--y".to_owned(),
            y.to_string(),
        ];
        if let Some(bridge_arguments) = self.bridge_arguments()? {
            arguments.extend(bridge_arguments);
        }
        arguments.extend(self.target_selector_arguments()?);
        self.run_helper(&arguments)
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
        arguments.extend(self.target_selector_arguments()?);
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
        let previous_frontmost_pid = self.frontmost_application_pid()?;
        self.focus()?;
        thread::sleep(Duration::from_millis(150));

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
        let result = self.run_helper(&arguments);
        let target_pid = *self
            .pinned_target_pid
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI backend target state"))?;
        let _ = self.restore_frontmost_application(previous_frontmost_pid, target_pid);
        result
    }

    fn input_text(&self, text: &str) -> Result<()> {
        let target_arguments = self.target_selector_arguments()?;
        let mut arguments = vec!["text".to_owned()];
        arguments.extend(target_arguments);
        arguments.push("--text".to_owned());
        arguments.push(text.to_owned());
        self.run_helper(&arguments)
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
        let mut arguments = vec![
            "screenshot-window".to_owned(),
            "--output".to_owned(),
            path.to_str()
                .context("screenshot path contains invalid UTF-8")?
                .to_owned(),
        ];
        arguments.splice(1..1, self.target_selector_arguments()?);
        self.run_helper(&arguments)
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
        let point = self
            .describe_all()
            .ok()
            .and_then(|tree| find_visible_scroll_container(&tree))
            .map(|frame| frame.center())
            .or_else(|| {
                self.window_capture_info().ok().map(|window| {
                    (
                        window.frame.x + (window.frame.width / 2.0),
                        window.frame.y + (window.frame.height / 2.0),
                    )
                })
            })
            .unwrap_or((0.0, 0.0));
        let mut arguments = vec![
            "scroll".to_owned(),
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
        ];
        if let Some(bridge_arguments) = self.bridge_arguments()? {
            arguments.extend(bridge_arguments);
        }
        arguments.extend(self.target_selector_arguments()?);
        self.run_helper(&arguments)
    }

    fn scroll_at_point(&self, direction: UiSwipeDirection, point: (f64, f64)) -> Result<()> {
        let mut arguments = vec![
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
        ];
        if let Some(bridge_arguments) = self.bridge_arguments()? {
            arguments.extend(bridge_arguments);
        }
        arguments.extend(self.target_selector_arguments()?);
        self.run_helper(&arguments)
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

fn read_trace_launch_registration(path: &Path) -> Result<Option<MacosTraceLaunchRegistration>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let registration = serde_json::from_str(&contents).with_context(|| {
        format!(
            "failed to parse macOS trace launch registration {}",
            path.display()
        )
    })?;
    Ok(Some(registration))
}

fn process_is_running(pid: u32) -> Result<bool> {
    let mut command = Command::new("kill");
    command.args(["-0", &pid.to_string()]);
    let (success, _stdout, _stderr) = command_output_allow_failure(&mut command)?;
    Ok(success)
}

fn terminate_macos_process_tree(pid: u32) -> Result<()> {
    if !process_is_running(pid)? {
        return Ok(());
    }

    terminate_macos_launch_descendants(pid, "TERM")?;
    let mut terminate = Command::new("kill");
    terminate.args(["-TERM", &pid.to_string()]);
    let _ = command_output_allow_failure(&mut terminate)?;

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if !process_is_running(pid)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }

    terminate_macos_launch_descendants(pid, "KILL")?;
    let mut kill = Command::new("kill");
    kill.args(["-KILL", &pid.to_string()]);
    let _ = command_output_allow_failure(&mut kill)?;
    Ok(())
}

fn terminate_macos_launch_descendants(parent_pid: u32, signal: &str) -> Result<()> {
    let mut command = Command::new("pkill");
    command.args([format!("-{signal}").as_str(), "-P", &parent_pid.to_string()]);
    let _ = command_output_allow_failure(&mut command)?;
    Ok(())
}

fn macos_launch_arguments(arguments: &[(String, String)]) -> Vec<String> {
    arguments
        .iter()
        .flat_map(|(key, value)| [format!("-{key}"), value.clone()])
        .collect()
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
    "Orbi macOS UI automation requires Accessibility access and the built-in Swift toolchain on this Mac"
}

fn should_rebuild_macos_ui_artifact(source_path: &Path, binary_path: &Path) -> Result<bool> {
    if !binary_path.exists() {
        return Ok(true);
    }

    let source_modified = fs::metadata(source_path)
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("failed to read {}", source_path.display()))?;
    let binary_modified = fs::metadata(binary_path)
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("failed to read {}", binary_path.display()))?;
    Ok(source_modified > binary_modified)
}

fn compile_macos_ui_artifact(
    project: &ProjectContext,
    source_path: &Path,
    binary_path: &Path,
    compiler_arguments: &[&str],
    description: &str,
) -> Result<()> {
    let mut command = xcrun_command(project.selected_xcode.as_ref());
    command.args(compiler_arguments);
    command.arg(source_path);
    command.arg("-o");
    command.arg(binary_path);
    let _ = run_command_capture(&mut command).with_context(|| {
        format!(
            "failed to compile {description} from {}",
            source_path.display()
        )
    })?;
    Ok(())
}

fn macos_ui_bridge_launch_environment(
    selected_xcode: Option<&SelectedXcode>,
    bridge_dylib_path: &Path,
    bridge_directory: &Path,
    bridge_notification_name: &str,
    log_pipe_path: &Path,
) -> Result<Vec<(String, String)>> {
    let log_redirect_dylib = selected_xcode_log_redirect_dylib_path(selected_xcode)?;
    let bridge_dylib_path = bridge_dylib_path
        .to_str()
        .context("macOS UI bridge dylib path contains invalid UTF-8")?;
    let bridge_directory = bridge_directory
        .to_str()
        .context("macOS UI bridge directory contains invalid UTF-8")?;
    let log_pipe_path = log_pipe_path
        .to_str()
        .context("macOS UI log pipe path contains invalid UTF-8")?;

    Ok(vec![
        ("NSUnbufferedIO".to_owned(), "YES".to_owned()),
        ("OS_LOG_TRANSLATE_PRINT_MODE".to_owned(), "0x80".to_owned()),
        (
            "IDE_DISABLED_OS_ACTIVITY_DT_MODE".to_owned(),
            "1".to_owned(),
        ),
        ("OS_LOG_DT_HOOK_MODE".to_owned(), "0x07".to_owned()),
        ("CFLOG_FORCE_DISABLE_STDERR".to_owned(), "1".to_owned()),
        (
            "DYLD_INSERT_LIBRARIES".to_owned(),
            format!("{}:{bridge_dylib_path}", log_redirect_dylib.display()),
        ),
        (
            "ORBI_MACOS_UI_BRIDGE_DIR".to_owned(),
            bridge_directory.to_owned(),
        ),
        (
            "ORBI_MACOS_UI_BRIDGE_NOTIFICATION".to_owned(),
            bridge_notification_name.to_owned(),
        ),
        (
            "ORBI_MACOS_UI_LOG_PIPE".to_owned(),
            log_pipe_path.to_owned(),
        ),
    ])
}

pub(crate) fn macos_doctor(project: &ProjectContext) -> Result<MacosDoctorStatus> {
    let helper_path = ensure_macos_driver_binary(project)?;
    let mut command = Command::new(helper_path);
    command.arg("doctor");
    let output = command_output(&mut command).with_context(macos_requirement_message)?;
    serde_json::from_str(&output).context("failed to parse macOS UI doctor output")
}

fn ensure_macos_driver_binary(project: &ProjectContext) -> Result<PathBuf> {
    let tools_dir = project.project_paths.orbi_dir.join("tools");
    ensure_dir(&tools_dir)?;
    let binary_path = tools_dir.join("orbi-macos-ui-driver");
    let source_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("apple")
        .join("testing")
        .join("ui")
        .join("macos_driver.swift");

    if !should_rebuild_macos_ui_artifact(&source_path, &binary_path)? {
        return Ok(binary_path);
    }

    compile_macos_ui_artifact(
        project,
        &source_path,
        &binary_path,
        &["--sdk", "macosx", "swiftc", "-O"],
        "macOS UI helper",
    )?;
    Ok(binary_path)
}

fn ensure_macos_bridge_dylib(project: &ProjectContext) -> Result<PathBuf> {
    let tools_dir = project.project_paths.orbi_dir.join("tools");
    ensure_dir(&tools_dir)?;
    let binary_path = tools_dir.join("orbi-macos-ui-bridge.dylib");
    let source_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("apple")
        .join("testing")
        .join("ui")
        .join("macos_bridge.m");

    if !should_rebuild_macos_ui_artifact(&source_path, &binary_path)? {
        return Ok(binary_path);
    }

    compile_macos_ui_artifact(
        project,
        &source_path,
        &binary_path,
        &[
            "--sdk",
            "macosx",
            "clang",
            "-dynamiclib",
            "-fobjc-arc",
            "-framework",
            "Foundation",
            "-framework",
            "AppKit",
            "-framework",
            "CoreGraphics",
        ],
        "macOS UI bridge",
    )?;
    Ok(binary_path)
}

#[cfg(test)]
mod tests {
    use super::{
        MacosBackend, macos_launch_arguments, macos_ui_bridge_launch_environment,
        read_trace_launch_registration,
    };
    use crate::apple::testing::ui::backend::UiBackend;
    use crate::apple::xcode::SelectedXcode;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use tempfile::tempdir;

    #[test]
    fn flattens_launch_argument_pairs_for_direct_launch() {
        let arguments = macos_launch_arguments(&[
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
    fn launch_environment_includes_log_redirect_and_bridge_injection() {
        let temp = tempdir().unwrap();
        let developer_dir = temp.path().join("Xcode.app/Contents/Developer");
        let log_redirect = developer_dir.join("usr/lib/libLogRedirect.dylib");
        fs::create_dir_all(log_redirect.parent().unwrap()).unwrap();
        fs::write(&log_redirect, b"").unwrap();

        let selected_xcode = SelectedXcode {
            version: "26.4".to_owned(),
            build_version: "17E192".to_owned(),
            app_path: PathBuf::from("/Applications/Xcode-26.4.app"),
            developer_dir,
        };
        let bridge_dylib = temp.path().join("orbi-macos-ui-bridge.dylib");
        let bridge_directory = temp.path().join("ui-bridge");
        let log_pipe_path = temp.path().join("inferior-stdio.pipe");

        let environment = macos_ui_bridge_launch_environment(
            Some(&selected_xcode),
            &bridge_dylib,
            &bridge_directory,
            "dev.orbi.ui.test",
            &log_pipe_path,
        )
        .unwrap();

        assert!(environment.contains(&("NSUnbufferedIO".to_owned(), "YES".to_owned())));
        assert!(environment.contains(&(
            "DYLD_INSERT_LIBRARIES".to_owned(),
            format!("{}:{}", log_redirect.display(), bridge_dylib.display()),
        )));
        assert!(environment.contains(&(
            "ORBI_MACOS_UI_BRIDGE_DIR".to_owned(),
            bridge_directory.display().to_string(),
        )));
        assert!(environment.contains(&(
            "ORBI_MACOS_UI_BRIDGE_NOTIFICATION".to_owned(),
            "dev.orbi.ui.test".to_owned(),
        )));
        assert!(environment.contains(&(
            "ORBI_MACOS_UI_LOG_PIPE".to_owned(),
            log_pipe_path.display().to_string(),
        )));
    }

    #[test]
    fn reads_trace_launch_registration_file() {
        let temp = tempdir().unwrap();
        let registration_path = temp.path().join("trace-launch.json");
        fs::write(
            &registration_path,
            r#"{"pid":4242,"launchId":"launch-1","bundleId":"sh.orbi.desktop"}"#,
        )
        .unwrap();

        let registration = read_trace_launch_registration(&registration_path)
            .unwrap()
            .expect("registration should be present");
        assert_eq!(registration.pid, 4242);
        assert_eq!(registration.launch_id, "launch-1");
        assert_eq!(registration.bundle_id, "sh.orbi.desktop");
    }

    #[test]
    fn take_screenshot_uses_helper_window_capture_command() {
        let temp = tempdir().unwrap();
        let helper_path = temp.path().join("helper.sh");
        let args_path = temp.path().join("helper-args.txt");
        let screenshot_path = temp.path().join("artifacts").join("capture.png");

        fs::write(
            &helper_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
                args_path.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&helper_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper_path, permissions).unwrap();

        let backend = MacosBackend {
            helper_path,
            bridge_dylib_path: temp.path().join("bridge.dylib"),
            bundle_id: "sh.orbi.desktop".to_owned(),
            bundle_path: Path::new("/tmp/Accord.app").to_path_buf(),
            executable_path: Path::new("/tmp/Accord.app/Contents/MacOS/Accord").to_path_buf(),
            selected_xcode: None,
            verbose: false,
            pinned_target_pid: Mutex::new(None),
            launched_session: Mutex::new(None),
            pending_trace_launch: Mutex::new(None),
            last_tap_point: Mutex::new(None),
            active_video: None,
        };

        backend.take_screenshot(&screenshot_path).unwrap();

        let arguments = fs::read_to_string(args_path).unwrap();
        assert_eq!(
            arguments.lines().collect::<Vec<_>>(),
            vec![
                "screenshot-window",
                "--bundle-id",
                "sh.orbi.desktop",
                "--output",
                screenshot_path.to_str().unwrap(),
            ]
        );
    }

    #[test]
    fn tap_point_routes_events_to_the_pinned_target_pid() {
        let temp = tempdir().unwrap();
        let helper_path = temp.path().join("helper.sh");
        let args_path = temp.path().join("helper-args.txt");

        fs::write(
            &helper_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
                args_path.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&helper_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper_path, permissions).unwrap();

        let backend = MacosBackend {
            helper_path,
            bridge_dylib_path: temp.path().join("bridge.dylib"),
            bundle_id: "sh.orbi.desktop".to_owned(),
            bundle_path: Path::new("/tmp/Accord.app").to_path_buf(),
            executable_path: Path::new("/tmp/Accord.app/Contents/MacOS/Accord").to_path_buf(),
            selected_xcode: None,
            verbose: false,
            pinned_target_pid: Mutex::new(Some(4242)),
            launched_session: Mutex::new(None),
            pending_trace_launch: Mutex::new(None),
            last_tap_point: Mutex::new(None),
            active_video: None,
        };

        backend.tap_point(12.0, 34.0, None).unwrap();

        let arguments = fs::read_to_string(args_path).unwrap();
        assert_eq!(
            arguments.lines().collect::<Vec<_>>(),
            vec!["tap", "--x", "12", "--y", "34", "--pid", "4242"]
        );
    }

    #[test]
    fn hover_point_routes_events_to_the_target_bundle() {
        let temp = tempdir().unwrap();
        let helper_path = temp.path().join("helper.sh");
        let args_path = temp.path().join("helper-args.txt");

        fs::write(
            &helper_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
                args_path.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&helper_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper_path, permissions).unwrap();

        let backend = MacosBackend {
            helper_path,
            bridge_dylib_path: temp.path().join("bridge.dylib"),
            bundle_id: "sh.orbi.desktop".to_owned(),
            bundle_path: Path::new("/tmp/Accord.app").to_path_buf(),
            executable_path: Path::new("/tmp/Accord.app/Contents/MacOS/Accord").to_path_buf(),
            selected_xcode: None,
            verbose: false,
            pinned_target_pid: Mutex::new(None),
            launched_session: Mutex::new(None),
            pending_trace_launch: Mutex::new(None),
            last_tap_point: Mutex::new(None),
            active_video: None,
        };

        backend.hover_point(90.0, 120.0).unwrap();

        let arguments = fs::read_to_string(args_path).unwrap();
        assert_eq!(
            arguments.lines().collect::<Vec<_>>(),
            vec![
                "move",
                "--x",
                "90",
                "--y",
                "120",
                "--bundle-id",
                "sh.orbi.desktop",
            ]
        );
    }
}
