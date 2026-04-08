use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;

use super::backend::UiBackend;
use super::flow::{canonical_or_absolute, flow_uses_manual_recording, resolve_path_from_flow};
use super::matching::{
    UiElementMatch, directional_points_in_frame, find_visible_element_by_selector,
    find_visible_scroll_container, infer_screen_frame, resolve_point_expr,
};
use super::report::{
    FlowRunReport, RunStatus, StepRunReport, append_report_error, flow_name_from_path,
    sanitize_artifact_name, sanitize_extension_component, unix_timestamp_secs,
};
use super::{
    UiCommand, UiDragAndDrop, UiElementScroll, UiElementSwipe, UiExtendedWaitUntil, UiFlow,
    UiKeyPress, UiPressKey, UiScrollUntilVisible, UiSelector, UiSwipe, UiSwipeDirection,
};
use crate::util::write_json_file;

const DEFAULT_ELEMENT_TIMEOUT: Duration = Duration::from_secs(7);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_SWIPE_DURATION_MS: u32 = 500;
const DEFAULT_SWIPE_DELTA: u32 = 5;
const DEFAULT_DRAG_DURATION_MS: u32 = 650;

struct RetryBlock<'a> {
    flow_path: &'a Path,
    commands: &'a [UiCommand],
}

pub(super) struct UiFlowRunner {
    pub(super) backend: Box<dyn UiBackend>,
    artifacts_dir: PathBuf,
    bundle_id: String,
    stack: Vec<PathBuf>,
    clipboard: Option<String>,
    manual_recording: Option<PathBuf>,
    skip_initial_launch: bool,
}

impl UiFlowRunner {
    pub(super) fn new(
        backend: Box<dyn UiBackend>,
        artifacts_dir: PathBuf,
        bundle_id: String,
        skip_initial_launch: bool,
    ) -> Self {
        Self {
            backend,
            artifacts_dir,
            bundle_id,
            stack: Vec::new(),
            clipboard: None,
            manual_recording: None,
            skip_initial_launch,
        }
    }

    pub(super) fn execute_flow(
        &mut self,
        path: &Path,
        invoked_by: Option<&Path>,
        reports: &mut Vec<FlowRunReport>,
    ) -> Result<bool> {
        let flow_path = canonical_or_absolute(path)?;
        if self.stack.contains(&flow_path) {
            let chain = self
                .stack
                .iter()
                .map(|entry| entry.display().to_string())
                .chain([flow_path.display().to_string()])
                .collect::<Vec<_>>()
                .join(" -> ");
            bail!("detected recursive `runFlow` chain: {chain}");
        }

        let flow = super::parser::parse_ui_flow(&flow_path)?;
        let started_at_unix = unix_timestamp_secs();
        let started = Instant::now();
        let auto_video_enabled = invoked_by.is_none()
            && self.backend.auto_record_top_level_flows()
            && !flow_uses_manual_recording(
                flow.path.as_path(),
                flow.commands.as_slice(),
                &mut HashSet::new(),
            )?;
        let deferred_auto_video =
            auto_video_enabled && self.backend.requires_running_target_for_recording();
        let video_path = auto_video_enabled.then(|| {
            self.artifacts_dir.join(format!(
                "{}.{}",
                sanitize_artifact_name(&flow_name_from_path(&flow_path)),
                self.backend.video_extension()
            ))
        });
        let mut auto_video_started = false;
        let mut report = FlowRunReport {
            name: flow_name_from_path(&flow_path),
            path: flow_path.clone(),
            invoked_by: invoked_by.map(Path::to_path_buf),
            started_at_unix,
            finished_at_unix: started_at_unix,
            duration_ms: 0,
            status: RunStatus::Passed,
            error: None,
            failure_screenshot: None,
            failure_hierarchy: None,
            video: None,
            steps: Vec::new(),
        };

        if let Some(path) = video_path.as_deref()
            && !deferred_auto_video
        {
            self.backend.start_video_recording(path)?;
            auto_video_started = true;
        }

        let execution = (|| -> Result<()> {
            report.name = flow
                .config
                .name
                .clone()
                .unwrap_or_else(|| flow_name_from_path(&flow.path));
            self.validate_app_id(&flow)?;

            self.stack.push(flow_path.clone());
            let result = self.execute_commands(
                flow.path.as_path(),
                flow.commands.as_slice(),
                video_path.as_deref().filter(|_| deferred_auto_video),
                &mut auto_video_started,
                &mut report.steps,
                reports,
            );
            self.stack.pop();
            result
        })();

        if let Err(error) = execution {
            report.status = RunStatus::Failed;
            report.error = Some(error.to_string());
            self.capture_failure_artifacts(&flow_path, &mut report)?;
        }
        if let Some(path) = video_path.as_ref()
            && auto_video_started
        {
            if let Err(error) = self.backend.stop_video_recording() {
                report.status = RunStatus::Failed;
                append_report_error(&mut report, error.to_string());
                if report.failure_screenshot.is_none() {
                    self.capture_failure_artifacts(&flow_path, &mut report)?;
                }
            } else if path.exists() {
                report.video = Some(path.clone());
            }
        }
        if invoked_by.is_none()
            && let Some(path) = self.manual_recording.take()
        {
            let _ = self.backend.stop_video_recording();
            report.status = RunStatus::Failed;
            append_report_error(
                &mut report,
                format!(
                    "flow finished with an active manual recording; add `stopRecording` for {}",
                    path.display()
                ),
            );
        }

        report.finished_at_unix = unix_timestamp_secs();
        report.duration_ms = started.elapsed().as_millis() as u64;
        let passed = matches!(report.status, RunStatus::Passed);
        reports.push(report);
        Ok(passed)
    }

    #[cfg(test)]
    pub(super) fn run_leaf_command(&mut self, command: &UiCommand) -> Result<Option<PathBuf>> {
        self.run_leaf_command_inner(command)
    }

    fn execute_commands(
        &mut self,
        flow_path: &Path,
        commands: &[UiCommand],
        deferred_auto_video_path: Option<&Path>,
        auto_video_started: &mut bool,
        steps: &mut Vec<StepRunReport>,
        reports: &mut Vec<FlowRunReport>,
    ) -> Result<()> {
        for command in commands {
            match command {
                UiCommand::RunFlow(relative_path) => {
                    let nested_path = resolve_path_from_flow(flow_path, relative_path);
                    if !self.execute_flow(&nested_path, Some(flow_path), reports)? {
                        bail!("nested flow `{}` failed", nested_path.display());
                    }
                }
                UiCommand::Repeat { times, commands } => {
                    for _ in 0..*times {
                        self.execute_commands(
                            flow_path,
                            commands,
                            deferred_auto_video_path,
                            auto_video_started,
                            steps,
                            reports,
                        )?;
                    }
                }
                UiCommand::Retry { times, commands } => self.execute_retry_block(
                    RetryBlock {
                        flow_path,
                        commands,
                    },
                    *times,
                    deferred_auto_video_path,
                    auto_video_started,
                    steps,
                    reports,
                )?,
                _ => {
                    if let Some(path) = deferred_auto_video_path
                        && !*auto_video_started
                        && !matches!(command, UiCommand::LaunchApp(_))
                    {
                        self.backend.start_video_recording(path)?;
                        *auto_video_started = true;
                    }
                    self.execute_leaf_command(flow_path, command, steps)?;
                    if let Some(path) = deferred_auto_video_path
                        && !*auto_video_started
                        && matches!(command, UiCommand::LaunchApp(_))
                    {
                        self.backend.start_video_recording(path)?;
                        *auto_video_started = true;
                    }
                }
            }
        }
        Ok(())
    }

    fn execute_retry_block(
        &mut self,
        retry: RetryBlock<'_>,
        times: u32,
        deferred_auto_video_path: Option<&Path>,
        auto_video_started: &mut bool,
        steps: &mut Vec<StepRunReport>,
        reports: &mut Vec<FlowRunReport>,
    ) -> Result<()> {
        if times == 0 {
            bail!("`retry.times` must be greater than zero");
        }

        let mut last_error = None;
        for attempt in 1..=times {
            let mut attempt_steps = Vec::new();
            match self.execute_commands(
                retry.flow_path,
                retry.commands,
                deferred_auto_video_path,
                auto_video_started,
                &mut attempt_steps,
                reports,
            ) {
                Ok(()) => {
                    steps.extend(attempt_steps);
                    return Ok(());
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    if attempt == times {
                        steps.extend(attempt_steps);
                    }
                }
            }
        }

        bail!(
            "retry block failed after {} attempt(s): {}",
            times,
            last_error.unwrap_or_else(|| "unknown error".to_owned())
        );
    }

    fn execute_leaf_command(
        &mut self,
        _flow_path: &Path,
        command: &UiCommand,
        steps: &mut Vec<StepRunReport>,
    ) -> Result<()> {
        let started = Instant::now();
        let result = self.run_leaf_command_inner(command);
        let duration_ms = started.elapsed().as_millis() as u64;
        let mut step = StepRunReport {
            index: steps.len(),
            command: command.summary(),
            duration_ms,
            status: RunStatus::Passed,
            error: None,
            artifact: None,
        };

        match result {
            Ok(artifact) => {
                step.artifact = artifact;
                steps.push(step);
                Ok(())
            }
            Err(error) => {
                step.status = RunStatus::Failed;
                step.error = Some(error.to_string());
                steps.push(step);
                Err(error)
            }
        }
    }

    fn run_leaf_command_inner(&mut self, command: &UiCommand) -> Result<Option<PathBuf>> {
        match command {
            UiCommand::LaunchApp(command) => {
                if self.skip_initial_launch {
                    self.skip_initial_launch = false;
                    return Ok(None);
                }
                let app_id = self.resolve_bundle_id(command.app_id.as_deref());
                if command.clear_keychain {
                    self.backend.clear_keychain()?;
                }
                if command.clear_state {
                    self.backend.clear_app_state(app_id)?;
                }
                if let Some(permissions) = command.permissions.as_ref() {
                    self.backend.set_permissions(app_id, permissions)?;
                }
                self.backend
                    .launch_app(app_id, command.stop_app, command.arguments.as_slice())?;
                Ok(None)
            }
            UiCommand::StopApp(app_id) | UiCommand::KillApp(app_id) => {
                self.backend
                    .stop_app(self.resolve_bundle_id(app_id.as_deref()))?;
                Ok(None)
            }
            UiCommand::ClearState(app_id) => {
                self.backend
                    .clear_app_state(self.resolve_bundle_id(app_id.as_deref()))?;
                Ok(None)
            }
            UiCommand::ClearKeychain => {
                self.backend.clear_keychain()?;
                Ok(None)
            }
            UiCommand::TapOn(target) => {
                if self.backend.activate_selector(target)? {
                    return Ok(None);
                }
                let element = self.find_tappable_element(target)?;
                let frame = element.frame.expect("tappable element must expose a frame");
                let (x, y) = frame.center();
                self.backend.tap_point(x, y, None)?;
                Ok(None)
            }
            UiCommand::HoverOn(target) => {
                let element = self.find_visible_element(target)?;
                let frame = element.frame.expect("hover target must expose a frame");
                let (x, y) = frame.center();
                self.backend.hover_point(x, y)?;
                Ok(None)
            }
            UiCommand::RightClickOn(target) => {
                let element = self.find_tappable_element(target)?;
                let frame = element
                    .frame
                    .expect("right-click target must expose a frame");
                let (x, y) = frame.center();
                self.backend.right_click_point(x, y)?;
                Ok(None)
            }
            UiCommand::TapOnPoint(point) => {
                let screen = infer_screen_frame(&self.backend.describe_all()?).context(
                    "could not infer simulator screen bounds from the accessibility tree",
                )?;
                let (x, y) = resolve_point_expr(&screen, point);
                self.backend.tap_point(x, y, None)?;
                Ok(None)
            }
            UiCommand::DoubleTapOn(target) => {
                let element = self.find_tappable_element(target)?;
                let frame = element.frame.expect("tappable element must expose a frame");
                let (x, y) = frame.center();
                self.backend.tap_point(x, y, None)?;
                thread::sleep(Duration::from_millis(120));
                self.backend.tap_point(x, y, None)?;
                Ok(None)
            }
            UiCommand::LongPressOn {
                target,
                duration_ms,
            } => {
                let element = self.find_tappable_element(target)?;
                let frame = element.frame.expect("tappable element must expose a frame");
                let (x, y) = frame.center();
                self.backend.tap_point(x, y, Some(*duration_ms))?;
                Ok(None)
            }
            UiCommand::Swipe(swipe) => {
                self.perform_swipe(swipe)?;
                Ok(None)
            }
            UiCommand::SwipeOn(command) => {
                self.perform_swipe_on(command)?;
                Ok(None)
            }
            UiCommand::DragAndDrop(command) => {
                self.perform_drag_and_drop(command)?;
                Ok(None)
            }
            UiCommand::Scroll(direction) => {
                self.perform_scroll(*direction)?;
                Ok(None)
            }
            UiCommand::ScrollOn(command) => {
                self.perform_scroll_on(command)?;
                Ok(None)
            }
            UiCommand::ScrollUntilVisible(command) => {
                self.scroll_until_visible(command)?;
                Ok(None)
            }
            UiCommand::InputText(text) => {
                self.backend.input_text(text)?;
                Ok(None)
            }
            UiCommand::PasteText => {
                let Some(text) = self.clipboard.as_deref() else {
                    bail!(
                        "pasteText requires a clipboard value; call `setClipboard` or `copyTextFrom` first"
                    );
                };
                self.backend.input_text(text)?;
                Ok(None)
            }
            UiCommand::SetClipboard(text) => {
                self.clipboard = Some(text.clone());
                Ok(None)
            }
            UiCommand::CopyTextFrom(selector) => {
                let element = self.find_visible_element(selector)?;
                self.clipboard = Some(element.copied_text.unwrap_or(element.label));
                Ok(None)
            }
            UiCommand::EraseText(characters) => {
                for _ in 0..*characters {
                    self.backend
                        .press_key(&UiKeyPress::plain(UiPressKey::Backspace))?;
                }
                Ok(None)
            }
            UiCommand::PressKey(key) => {
                self.backend.press_key(key)?;
                Ok(None)
            }
            UiCommand::PressKeyCode {
                keycode,
                duration_ms,
                modifiers,
            } => {
                self.backend
                    .press_key_code(*keycode, *duration_ms, modifiers.as_slice())?;
                Ok(None)
            }
            UiCommand::KeySequence(keycodes) => {
                self.backend.press_key_sequence(keycodes)?;
                Ok(None)
            }
            UiCommand::PressButton {
                button,
                duration_ms,
            } => {
                self.backend.press_button(*button, *duration_ms)?;
                Ok(None)
            }
            UiCommand::SelectMenuItem(path) => {
                self.backend.select_menu_item(path)?;
                Ok(None)
            }
            UiCommand::HideKeyboard => {
                self.backend.hide_keyboard()?;
                Ok(None)
            }
            UiCommand::AssertVisible(target) => {
                self.find_visible_element(target)?;
                Ok(None)
            }
            UiCommand::AssertNotVisible(target) => {
                self.wait_for_element_absence(target)?;
                Ok(None)
            }
            UiCommand::ExtendedWaitUntil(command) => {
                self.extended_wait_until(command)?;
                Ok(None)
            }
            UiCommand::WaitForAnimationToEnd(timeout_ms) => {
                self.wait_for_animation_to_end(*timeout_ms)?;
                Ok(None)
            }
            UiCommand::TakeScreenshot(name) => {
                let path = self.artifact_path(name.as_deref().unwrap_or("screenshot"), "png");
                self.backend.take_screenshot(&path)?;
                Ok(Some(path))
            }
            UiCommand::StartRecording(name) => {
                if self.manual_recording.is_some() {
                    bail!(
                        "video recording is already active; call `stopRecording` before starting another one"
                    );
                }
                let path = self.artifact_path(
                    name.as_deref().unwrap_or("recording"),
                    self.backend.video_extension(),
                );
                self.backend.start_video_recording(&path)?;
                self.manual_recording = Some(path);
                Ok(None)
            }
            UiCommand::StopRecording => {
                let Some(path) = self.manual_recording.take() else {
                    return Ok(None);
                };
                self.backend.stop_video_recording()?;
                Ok(Some(path))
            }
            UiCommand::OpenLink(url) => {
                self.backend.open_link(url)?;
                Ok(None)
            }
            UiCommand::SetLocation {
                latitude,
                longitude,
            } => {
                self.backend.set_location(*latitude, *longitude)?;
                Ok(None)
            }
            UiCommand::SetPermissions(command) => {
                let app_id = self.resolve_bundle_id(command.app_id.as_deref());
                self.backend.set_permissions(app_id, command)?;
                Ok(None)
            }
            UiCommand::Travel(command) => {
                self.backend.travel(command)?;
                Ok(None)
            }
            UiCommand::AddMedia(paths) => {
                let flow_path = self.current_flow_path()?;
                let resolved = paths
                    .iter()
                    .map(|path| resolve_path_from_flow(flow_path, path))
                    .collect::<Vec<_>>();
                self.backend.add_media(&resolved)?;
                Ok(None)
            }
            UiCommand::RunFlow(_) | UiCommand::Repeat { .. } | UiCommand::Retry { .. } => {
                unreachable!("control-flow commands are handled outside `run_leaf_command`")
            }
        }
    }

    fn validate_app_id(&self, flow: &UiFlow) -> Result<()> {
        if let Some(app_id) = flow.config.app_id.as_deref()
            && app_id != self.bundle_id
        {
            bail!(
                "flow `{}` targets appId `{app_id}`, but Orbit built `{}`",
                flow.path.display(),
                self.bundle_id
            );
        }
        Ok(())
    }

    fn find_visible_element(&self, selector: &UiSelector) -> Result<UiElementMatch> {
        self.find_visible_element_with_timeout(selector, DEFAULT_ELEMENT_TIMEOUT)
    }

    fn find_visible_element_with_timeout(
        &self,
        selector: &UiSelector,
        timeout: Duration,
    ) -> Result<UiElementMatch> {
        let mut last_reason = None;
        let started = Instant::now();
        while started.elapsed() < timeout {
            match self.backend.describe_all() {
                Ok(tree) => {
                    if let Some(element) = find_visible_element_by_selector(&tree, selector) {
                        return Ok(element);
                    }
                    last_reason = Some(format!(
                        "could not find `{}` in the current accessibility tree",
                        selector.summary()
                    ));
                }
                Err(error) => last_reason = Some(error.to_string()),
            }
            thread::sleep(DEFAULT_POLL_INTERVAL);
        }

        Err(anyhow::anyhow!(
            "{} after waiting {}s",
            last_reason.unwrap_or_else(|| format!(
                "could not find `{}` in the current accessibility tree",
                selector.summary()
            )),
            timeout.as_secs()
        ))
    }

    fn find_tappable_element(&self, selector: &UiSelector) -> Result<UiElementMatch> {
        let mut last_reason = None;
        let started = Instant::now();
        while started.elapsed() < DEFAULT_ELEMENT_TIMEOUT {
            match self.backend.describe_all() {
                Ok(tree) => {
                    if let Some(element) = find_visible_element_by_selector(&tree, selector) {
                        if element.frame.is_some() {
                            return Ok(element);
                        }
                        last_reason = Some(format!(
                            "found `{}`, but it did not expose a tappable frame",
                            selector.summary()
                        ));
                    } else {
                        last_reason = Some(format!(
                            "could not find `{}` in the current accessibility tree",
                            selector.summary()
                        ));
                    }
                }
                Err(error) => last_reason = Some(error.to_string()),
            }
            thread::sleep(DEFAULT_POLL_INTERVAL);
        }

        Err(anyhow::anyhow!(
            "{} after waiting {}s",
            last_reason.unwrap_or_else(|| format!(
                "could not find `{}` in the current accessibility tree",
                selector.summary()
            )),
            DEFAULT_ELEMENT_TIMEOUT.as_secs()
        ))
    }

    fn wait_for_element_absence(&self, selector: &UiSelector) -> Result<()> {
        self.wait_for_element_absence_with_timeout(selector, DEFAULT_ELEMENT_TIMEOUT)
    }

    fn wait_for_element_absence_with_timeout(
        &self,
        selector: &UiSelector,
        timeout: Duration,
    ) -> Result<()> {
        let mut last_seen = None;
        let started = Instant::now();
        while started.elapsed() < timeout {
            if let Ok(tree) = self.backend.describe_all() {
                if let Some(element) = find_visible_element_by_selector(&tree, selector) {
                    last_seen = Some(element.label);
                } else {
                    return Ok(());
                }
            }
            thread::sleep(DEFAULT_POLL_INTERVAL);
        }

        let label = last_seen.unwrap_or_else(|| selector.summary());
        bail!(
            "unexpectedly found `{}` on screen as `{label}` after waiting {}s",
            selector.summary(),
            timeout.as_secs()
        );
    }

    fn scroll_until_visible(&self, command: &UiScrollUntilVisible) -> Result<()> {
        let timeout = Duration::from_millis(u64::from(command.timeout_ms));
        let started = Instant::now();
        while started.elapsed() < timeout {
            let tree = self.backend.describe_all()?;
            if find_visible_element_by_selector(&tree, &command.target).is_some() {
                return Ok(());
            }
            self.perform_scroll_with_tree(command.direction, &tree)?;
            thread::sleep(Duration::from_millis(350));
        }

        bail!(
            "failed to reveal `{}` after scrolling for {}ms",
            command.target.summary(),
            command.timeout_ms
        );
    }

    fn perform_swipe(&self, swipe: &UiSwipe) -> Result<()> {
        let screen = infer_screen_frame(&self.backend.describe_all()?)
            .context("could not infer simulator screen bounds from the accessibility tree")?;
        let start = resolve_point_expr(&screen, &swipe.start);
        let end = resolve_point_expr(&screen, &swipe.end);
        // `idb` defaults to coarse 10pt steps, which is too rough for some SwiftUI pagers.
        // Orbit applies a denser path unless the flow overrides it explicitly.
        let duration_ms = swipe.duration_ms.or(Some(DEFAULT_SWIPE_DURATION_MS));
        let delta = swipe.delta.or(Some(DEFAULT_SWIPE_DELTA));
        self.backend.swipe_points(start, end, duration_ms, delta)?;
        Ok(())
    }

    fn perform_swipe_on(&self, command: &UiElementSwipe) -> Result<()> {
        let element = self.find_tappable_element(&command.target)?;
        let frame = element.frame.expect("swipe target must expose a frame");
        let (start, end) = directional_points_in_frame(frame, command.direction, false);
        self.backend.swipe_points(
            start,
            end,
            command.duration_ms.or(Some(DEFAULT_SWIPE_DURATION_MS)),
            command.delta.or(Some(DEFAULT_SWIPE_DELTA)),
        )?;
        Ok(())
    }

    fn perform_drag_and_drop(&self, command: &UiDragAndDrop) -> Result<()> {
        let source = self.find_tappable_element(&command.source)?;
        let source_frame = source.frame.expect("drag source must expose a frame");
        let destination = self.find_tappable_element(&command.destination)?;
        let destination_frame = destination
            .frame
            .expect("drag destination must expose a frame");
        self.backend.drag_points(
            source_frame.center(),
            destination_frame.center(),
            command.duration_ms.or(Some(DEFAULT_DRAG_DURATION_MS)),
            command.delta.or(Some(DEFAULT_SWIPE_DELTA)),
        )
    }

    fn perform_scroll_on(&self, command: &UiElementScroll) -> Result<()> {
        let element = self.find_tappable_element(&command.target)?;
        let frame = element.frame.expect("scroll target must expose a frame");
        self.backend
            .scroll_at_point(command.direction, frame.center())
    }

    fn perform_scroll(&self, direction: UiSwipeDirection) -> Result<()> {
        let tree = self.backend.describe_all()?;
        self.perform_scroll_with_tree(direction, &tree)
    }

    fn perform_scroll_with_tree(
        &self,
        direction: UiSwipeDirection,
        tree: &JsonValue,
    ) -> Result<()> {
        if let Some(container) = find_visible_scroll_container(tree) {
            self.backend.scroll_at_point(direction, container.center())
        } else {
            self.backend.scroll_in_direction(direction)
        }
    }

    fn current_flow_path(&self) -> Result<&Path> {
        self.stack
            .last()
            .map(PathBuf::as_path)
            .context("UI runner lost track of the active flow path")
    }

    fn resolve_bundle_id<'a>(&'a self, app_id: Option<&'a str>) -> &'a str {
        app_id.unwrap_or(&self.bundle_id)
    }

    fn artifact_path(&self, name: &str, extension: &str) -> PathBuf {
        let mut relative = PathBuf::from(name);
        if relative.extension().is_none() {
            relative.set_extension(extension);
        }
        let mut sanitized = PathBuf::new();
        for component in relative.components() {
            if let std::path::Component::Normal(value) = component {
                let raw = value.to_string_lossy();
                let component_path = Path::new(raw.as_ref());
                let stem = component_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .map(sanitize_artifact_name)
                    .unwrap_or_else(|| sanitize_artifact_name(raw.as_ref()));
                let extension = component_path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(sanitize_extension_component);
                let mut file_name = stem;
                if let Some(extension) = extension {
                    file_name.push('.');
                    file_name.push_str(&extension);
                }
                sanitized.push(file_name);
            }
        }
        self.artifacts_dir.join(sanitized)
    }

    fn extended_wait_until(&self, command: &UiExtendedWaitUntil) -> Result<()> {
        let timeout = Duration::from_millis(u64::from(command.timeout_ms));
        if let Some(selector) = command.visible.as_ref() {
            self.find_visible_element_with_timeout(selector, timeout)?;
        }
        if let Some(selector) = command.not_visible.as_ref() {
            self.wait_for_element_absence_with_timeout(selector, timeout)?;
        }
        Ok(())
    }

    fn wait_for_animation_to_end(&self, timeout_ms: u32) -> Result<()> {
        let timeout = Duration::from_millis(u64::from(timeout_ms));
        let started = Instant::now();
        let mut last_tree = None;
        let mut stable_polls = 0_u8;
        while started.elapsed() < timeout {
            if let Ok(tree) = self.backend.describe_all() {
                let serialized = serde_json::to_string(&tree).context(
                    "failed to serialize accessibility tree while waiting for animations",
                )?;
                if last_tree.as_deref() == Some(serialized.as_str()) {
                    stable_polls = stable_polls.saturating_add(1);
                    if stable_polls >= 2 {
                        return Ok(());
                    }
                } else {
                    last_tree = Some(serialized);
                    stable_polls = 0;
                }
            }
            thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    }

    fn capture_failure_artifacts(
        &self,
        flow_path: &Path,
        report: &mut FlowRunReport,
    ) -> Result<()> {
        let stem = sanitize_artifact_name(&flow_name_from_path(flow_path));
        let screenshot_path = self.artifacts_dir.join(format!("{stem}-failure.png"));
        if self.backend.take_screenshot(&screenshot_path).is_ok() {
            report.failure_screenshot = Some(screenshot_path);
        }

        let hierarchy_path = self.artifacts_dir.join(format!("{stem}-hierarchy.json"));
        if let Ok(tree) = self.backend.describe_all() {
            write_json_file(&hierarchy_path, &tree)?;
            report.failure_hierarchy = Some(hierarchy_path);
        }
        Ok(())
    }
}
