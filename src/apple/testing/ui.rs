#[path = "ui/backend.rs"]
pub(crate) mod backend;
#[path = "ui/parser.rs"]
mod parser;

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use self::backend::{IosSimulatorBackend, MacosBackend, MacosDoctorStatus, UiBackend};
use self::parser::parse_ui_flow;
use crate::apple::build::toolchain::DestinationKind;
use crate::apple::logs::SimulatorAppLogStream;
use crate::apple::{build, runtime};
use crate::cli::TestArgs;
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, TestTargetManifest};
use crate::util::{
    collect_files_with_extensions, ensure_dir, format_elapsed, print_success, resolve_path,
    write_json_file,
};

const DEFAULT_ELEMENT_TIMEOUT: Duration = Duration::from_secs(7);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_SWIPE_DURATION_MS: u32 = 500;
const DEFAULT_SWIPE_DELTA: u32 = 5;
const DEFAULT_DRAG_DURATION_MS: u32 = 650;

#[derive(Debug, Clone)]
pub struct UiFlow {
    pub path: PathBuf,
    pub config: UiFlowConfig,
    pub commands: Vec<UiCommand>,
}

#[derive(Debug, Clone, Default)]
pub struct UiFlowConfig {
    pub app_id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    LaunchApp(UiLaunchApp),
    StopApp(Option<String>),
    KillApp(Option<String>),
    ClearState(Option<String>),
    ClearKeychain,
    TapOn(UiSelector),
    HoverOn(UiSelector),
    RightClickOn(UiSelector),
    TapOnPoint(UiPointExpr),
    DoubleTapOn(UiSelector),
    LongPressOn {
        target: UiSelector,
        duration_ms: u32,
    },
    Swipe(UiSwipe),
    SwipeOn(UiElementSwipe),
    DragAndDrop(UiDragAndDrop),
    Scroll(UiSwipeDirection),
    ScrollOn(UiElementScroll),
    ScrollUntilVisible(UiScrollUntilVisible),
    InputText(String),
    PasteText,
    SetClipboard(String),
    CopyTextFrom(UiSelector),
    EraseText(u32),
    PressKey(UiKeyPress),
    PressKeyCode {
        keycode: u32,
        duration_ms: Option<u32>,
        modifiers: Vec<UiKeyModifier>,
    },
    KeySequence(Vec<u32>),
    PressButton {
        button: UiHardwareButton,
        duration_ms: Option<u32>,
    },
    SelectMenuItem(Vec<String>),
    HideKeyboard,
    AssertVisible(UiSelector),
    AssertNotVisible(UiSelector),
    ExtendedWaitUntil(UiExtendedWaitUntil),
    WaitForAnimationToEnd(u32),
    TakeScreenshot(Option<String>),
    StartRecording(Option<String>),
    StopRecording,
    OpenLink(String),
    SetLocation {
        latitude: f64,
        longitude: f64,
    },
    SetPermissions(UiPermissionConfig),
    Travel(UiTravel),
    AddMedia(Vec<PathBuf>),
    RunFlow(PathBuf),
    Repeat {
        times: u32,
        commands: Vec<UiCommand>,
    },
    Retry {
        times: u32,
        commands: Vec<UiCommand>,
    },
}

#[derive(Debug, Clone)]
pub struct UiSwipe {
    pub start: UiPointExpr,
    pub end: UiPointExpr,
    pub duration_ms: Option<u32>,
    pub delta: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct UiElementSwipe {
    pub target: UiSelector,
    pub direction: UiSwipeDirection,
    pub duration_ms: Option<u32>,
    pub delta: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct UiElementScroll {
    pub target: UiSelector,
    pub direction: UiSwipeDirection,
}

#[derive(Debug, Clone)]
pub struct UiDragAndDrop {
    pub source: UiSelector,
    pub destination: UiSelector,
    pub duration_ms: Option<u32>,
    pub delta: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct UiPointExpr {
    pub x: UiCoordinate,
    pub y: UiCoordinate,
}

#[derive(Debug, Clone, Copy)]
pub enum UiCoordinate {
    Absolute(f64),
    Percent(f64),
}

#[derive(Debug, Clone, Copy)]
pub enum UiSwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub struct UiScrollUntilVisible {
    pub target: UiSelector,
    pub direction: UiSwipeDirection,
    pub timeout_ms: u32,
}

#[derive(Debug, Clone, Default)]
pub struct UiLaunchApp {
    pub app_id: Option<String>,
    pub clear_state: bool,
    pub clear_keychain: bool,
    pub stop_app: bool,
    pub permissions: Option<UiPermissionConfig>,
    pub arguments: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct UiExtendedWaitUntil {
    pub visible: Option<UiSelector>,
    pub not_visible: Option<UiSelector>,
    pub timeout_ms: u32,
}

#[derive(Debug, Clone)]
pub struct UiPermissionConfig {
    pub app_id: Option<String>,
    pub permissions: Vec<UiPermissionSetting>,
}

#[derive(Debug, Clone)]
pub struct UiPermissionSetting {
    pub name: String,
    pub state: UiPermissionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPermissionState {
    Allow,
    Deny,
    Unset,
}

#[derive(Debug, Clone)]
pub struct UiSelector {
    pub text: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UiKeyPress {
    pub key: UiPressKey,
    pub modifiers: Vec<UiKeyModifier>,
}

impl UiKeyPress {
    pub fn plain(key: UiPressKey) -> Self {
        Self {
            key,
            modifiers: Vec::new(),
        }
    }

    fn summary(&self) -> String {
        if self.modifiers.is_empty() {
            return self.key.summary();
        }

        let modifiers = self
            .modifiers
            .iter()
            .map(|modifier| modifier.summary())
            .collect::<Vec<_>>()
            .join("+");
        format!("{modifiers}+{}", self.key.summary())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiKeyModifier {
    Command,
    Shift,
    Option,
    Control,
    Function,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPressKey {
    Home,
    Lock,
    Enter,
    Backspace,
    Escape,
    Space,
    VolumeUp,
    VolumeDown,
    Tab,
    Back,
    Power,
    LeftArrow,
    RightArrow,
    UpArrow,
    DownArrow,
    Character(char),
}

#[derive(Debug, Clone, Copy)]
pub enum UiHardwareButton {
    ApplePay,
    Home,
    Lock,
    SideButton,
    Siri,
}

#[derive(Debug, Clone)]
pub struct UiTravel {
    pub points: Vec<UiLocationPoint>,
    pub speed_meters_per_second: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct UiCrashQuery {
    pub before: Option<String>,
    pub since: Option<String>,
    pub bundle_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UiCrashDeleteRequest {
    pub name: Option<String>,
    pub before: Option<String>,
    pub since: Option<String>,
    pub bundle_id: Option<String>,
    pub delete_all: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct UiLocationPoint {
    pub latitude: f64,
    pub longitude: f64,
}

impl UiCommand {
    fn summary(&self) -> String {
        match self {
            UiCommand::LaunchApp(command) => match command.app_id.as_deref() {
                Some(app_id) => format!("launchApp {app_id}"),
                None => "launchApp".to_owned(),
            },
            UiCommand::StopApp(app_id) => match app_id.as_deref() {
                Some(app_id) => format!("stopApp {app_id}"),
                None => "stopApp".to_owned(),
            },
            UiCommand::KillApp(app_id) => match app_id.as_deref() {
                Some(app_id) => format!("killApp {app_id}"),
                None => "killApp".to_owned(),
            },
            UiCommand::ClearState(app_id) => match app_id.as_deref() {
                Some(app_id) => format!("clearState {app_id}"),
                None => "clearState".to_owned(),
            },
            UiCommand::ClearKeychain => "clearKeychain".to_owned(),
            UiCommand::TapOn(target) => format!("tapOn {}", target.summary()),
            UiCommand::HoverOn(target) => format!("hoverOn {}", target.summary()),
            UiCommand::RightClickOn(target) => format!("rightClickOn {}", target.summary()),
            UiCommand::TapOnPoint(_) => "tapOnPoint".to_owned(),
            UiCommand::DoubleTapOn(target) => format!("doubleTapOn {}", target.summary()),
            UiCommand::LongPressOn { target, .. } => {
                format!("longPressOn {}", target.summary())
            }
            UiCommand::Swipe(_) => "swipe".to_owned(),
            UiCommand::SwipeOn(command) => {
                format!(
                    "swipeOn {} {:?}",
                    command.target.summary(),
                    command.direction
                )
            }
            UiCommand::DragAndDrop(command) => format!(
                "dragAndDrop {} -> {}",
                command.source.summary(),
                command.destination.summary()
            ),
            UiCommand::Scroll(direction) => format!("scroll {:?}", direction),
            UiCommand::ScrollOn(command) => {
                format!(
                    "scrollOn {} {:?}",
                    command.target.summary(),
                    command.direction
                )
            }
            UiCommand::ScrollUntilVisible(command) => {
                format!("scrollUntilVisible {}", command.target.summary())
            }
            UiCommand::InputText(text) => format!("inputText {}", preview_text(text)),
            UiCommand::PasteText => "pasteText".to_owned(),
            UiCommand::SetClipboard(text) => format!("setClipboard {}", preview_text(text)),
            UiCommand::CopyTextFrom(selector) => {
                format!("copyTextFrom {}", selector.summary())
            }
            UiCommand::EraseText(count) => format!("eraseText {count}"),
            UiCommand::PressKey(key) => format!("pressKey {}", key.summary()),
            UiCommand::PressKeyCode {
                keycode, modifiers, ..
            } => {
                if modifiers.is_empty() {
                    format!("pressKeyCode {keycode}")
                } else {
                    let modifiers = modifiers
                        .iter()
                        .map(|modifier| modifier.summary())
                        .collect::<Vec<_>>()
                        .join("+");
                    format!("pressKeyCode {modifiers}+{keycode}")
                }
            }
            UiCommand::KeySequence(keycodes) => format!("keySequence {}", keycodes.len()),
            UiCommand::PressButton { button, .. } => {
                format!("pressButton {}", button.summary())
            }
            UiCommand::SelectMenuItem(path) => {
                format!("selectMenuItem {}", path.join(" > "))
            }
            UiCommand::HideKeyboard => "hideKeyboard".to_owned(),
            UiCommand::AssertVisible(target) => {
                format!("assertVisible {}", target.summary())
            }
            UiCommand::AssertNotVisible(target) => {
                format!("assertNotVisible {}", target.summary())
            }
            UiCommand::ExtendedWaitUntil(command) => {
                if let Some(selector) = command.visible.as_ref() {
                    format!("extendedWaitUntil visible {}", selector.summary())
                } else if let Some(selector) = command.not_visible.as_ref() {
                    format!("extendedWaitUntil notVisible {}", selector.summary())
                } else {
                    "extendedWaitUntil".to_owned()
                }
            }
            UiCommand::WaitForAnimationToEnd(timeout_ms) => {
                format!("waitForAnimationToEnd {timeout_ms}ms")
            }
            UiCommand::TakeScreenshot(name) => match name {
                Some(name) => format!("takeScreenshot {name}"),
                None => "takeScreenshot".to_owned(),
            },
            UiCommand::StartRecording(path) => match path {
                Some(path) => format!("startRecording {path}"),
                None => "startRecording".to_owned(),
            },
            UiCommand::StopRecording => "stopRecording".to_owned(),
            UiCommand::OpenLink(url) => format!("openLink {url}"),
            UiCommand::SetLocation {
                latitude,
                longitude,
            } => format!("setLocation {latitude},{longitude}"),
            UiCommand::SetPermissions(command) => match command.app_id.as_deref() {
                Some(app_id) => format!("setPermissions {app_id}"),
                None => "setPermissions".to_owned(),
            },
            UiCommand::Travel(command) => format!("travel {}", command.points.len()),
            UiCommand::AddMedia(paths) => format!("addMedia {}", paths.len()),
            UiCommand::RunFlow(path) => format!("runFlow {}", path.display()),
            UiCommand::Repeat { times, .. } => format!("repeat {times}"),
            UiCommand::Retry { times, .. } => format!("retry {times}"),
        }
    }
}

impl UiSelector {
    fn summary(&self) -> String {
        match (self.text.as_deref(), self.id.as_deref()) {
            (Some(text), Some(id)) => format!("text={text}, id={id}"),
            (Some(text), None) => text.to_owned(),
            (None, Some(id)) => format!("id={id}"),
            (None, None) => "<selector>".to_owned(),
        }
    }
}

impl UiPressKey {
    fn summary(self) -> String {
        match self {
            UiPressKey::Home => "HOME".to_owned(),
            UiPressKey::Lock => "LOCK".to_owned(),
            UiPressKey::Enter => "ENTER".to_owned(),
            UiPressKey::Backspace => "BACKSPACE".to_owned(),
            UiPressKey::Escape => "ESCAPE".to_owned(),
            UiPressKey::Space => "SPACE".to_owned(),
            UiPressKey::VolumeUp => "VOLUME_UP".to_owned(),
            UiPressKey::VolumeDown => "VOLUME_DOWN".to_owned(),
            UiPressKey::Tab => "TAB".to_owned(),
            UiPressKey::Back => "BACK".to_owned(),
            UiPressKey::Power => "POWER".to_owned(),
            UiPressKey::LeftArrow => "LEFT".to_owned(),
            UiPressKey::RightArrow => "RIGHT".to_owned(),
            UiPressKey::UpArrow => "UP".to_owned(),
            UiPressKey::DownArrow => "DOWN".to_owned(),
            UiPressKey::Character(character) => character.to_ascii_uppercase().to_string(),
        }
    }
}

impl UiKeyModifier {
    fn summary(&self) -> &'static str {
        match self {
            UiKeyModifier::Command => "COMMAND",
            UiKeyModifier::Shift => "SHIFT",
            UiKeyModifier::Option => "OPTION",
            UiKeyModifier::Control => "CONTROL",
            UiKeyModifier::Function => "FUNCTION",
        }
    }
}

impl UiHardwareButton {
    fn summary(self) -> &'static str {
        match self {
            UiHardwareButton::ApplePay => "APPLE_PAY",
            UiHardwareButton::Home => "HOME",
            UiHardwareButton::Lock => "LOCK",
            UiHardwareButton::SideButton => "SIDE_BUTTON",
            UiHardwareButton::Siri => "SIRI",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RunStatus {
    Passed,
    Failed,
}

#[derive(Debug, Serialize)]
struct UiTestRunReport {
    id: String,
    platform: ApplePlatform,
    backend: String,
    bundle_id: String,
    bundle_path: PathBuf,
    receipt_path: PathBuf,
    target_name: String,
    target_id: String,
    report_path: PathBuf,
    artifacts_dir: PathBuf,
    started_at_unix: u64,
    finished_at_unix: u64,
    duration_ms: u64,
    status: RunStatus,
    flows: Vec<FlowRunReport>,
}

#[derive(Debug, Serialize)]
struct FlowRunReport {
    path: PathBuf,
    name: String,
    invoked_by: Option<PathBuf>,
    started_at_unix: u64,
    finished_at_unix: u64,
    duration_ms: u64,
    status: RunStatus,
    error: Option<String>,
    failure_screenshot: Option<PathBuf>,
    failure_hierarchy: Option<PathBuf>,
    video: Option<PathBuf>,
    steps: Vec<StepRunReport>,
}

#[derive(Debug, Serialize)]
struct StepRunReport {
    index: usize,
    command: String,
    duration_ms: u64,
    status: RunStatus,
    error: Option<String>,
    artifact: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct UiElementMatch {
    label: String,
    frame: Option<UiFrame>,
    score: u8,
    copied_text: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct UiFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl UiFrame {
    fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

struct PreparedUiSession {
    build_outcome: crate::apple::build::pipeline::BuildOutcome,
    backend: Box<dyn UiBackend>,
    verbose: bool,
    selected_xcode: Option<crate::apple::xcode::SelectedXcode>,
}

pub fn run_ui_tests(project: &ProjectContext, args: &TestArgs) -> Result<()> {
    let ui_tests = project
        .resolved_manifest
        .tests
        .ui
        .as_ref()
        .context("manifest does not declare `tests.ui`")?;
    let platform = runtime::resolve_platform(
        project,
        args.platform.map(runtime::apple_platform_from_cli),
        "Select a platform to test",
    )?;
    if platform == ApplePlatform::Macos {
        let status = backend::macos_doctor(project)?;
        ensure_macos_ui_test_requirements(&status)?;
    }
    let flow_paths = collect_ui_flow_paths(project, ui_tests)?;
    if flow_paths.is_empty() {
        bail!("`tests.ui.sources` did not contain any `.yml` or `.yaml` files");
    }

    let run_id = format!("{}-{}", unix_timestamp_secs(), Uuid::new_v4());
    let run_root = project
        .project_paths
        .orbit_dir
        .join("tests")
        .join("ui")
        .join(&run_id);
    let artifacts_dir = run_root.join("artifacts");
    ensure_dir(&artifacts_dir)?;

    let started_at_unix = unix_timestamp_secs();
    let started = Instant::now();
    let prepared = prepare_ui_session(project, platform, false)?;

    let report_path = run_root.join("report.json");
    let mut flow_reports = Vec::new();
    let _app_logs = start_ui_app_logs(&prepared);
    let mut runner = UiFlowRunner::new(
        prepared.backend,
        artifacts_dir.clone(),
        prepared.build_outcome.receipt.bundle_id.clone(),
    );
    let mut has_failures = false;
    for flow_path in &flow_paths {
        if !runner.execute_flow(flow_path, None, &mut flow_reports)? {
            has_failures = true;
        }
    }
    if let Err(error) = runner
        .backend
        .stop_app(prepared.build_outcome.receipt.bundle_id.as_str())
    {
        eprintln!(
            "warning: failed to stop `{}` after UI tests on {}: {error:#}",
            prepared.build_outcome.receipt.bundle_id,
            runner.backend.target_name()
        );
    }

    let finished_at_unix = unix_timestamp_secs();
    let report = UiTestRunReport {
        id: run_id,
        platform,
        backend: runner.backend.backend_name().to_owned(),
        bundle_id: prepared.build_outcome.receipt.bundle_id.clone(),
        bundle_path: prepared.build_outcome.receipt.bundle_path.clone(),
        receipt_path: prepared.build_outcome.receipt_path.clone(),
        target_name: runner.backend.target_name().to_owned(),
        target_id: runner.backend.target_id().to_owned(),
        report_path: report_path.clone(),
        artifacts_dir: artifacts_dir.clone(),
        started_at_unix,
        finished_at_unix,
        duration_ms: started.elapsed().as_millis() as u64,
        status: if has_failures {
            RunStatus::Failed
        } else {
            RunStatus::Passed
        },
        flows: flow_reports,
    };
    write_json_file(&report_path, &report)?;
    println!("report: {}", report_path.display());

    if has_failures {
        bail!("UI tests failed; see {}", report_path.display());
    }

    print_success(format!(
        "UI tests passed for `{}` on {} using {} flow(s) in {}.",
        prepared.build_outcome.receipt.target,
        runner.backend.target_name(),
        flow_paths.len(),
        format_elapsed(started.elapsed())
    ));
    Ok(())
}

fn start_ui_app_logs(prepared: &PreparedUiSession) -> Option<SimulatorAppLogStream> {
    if prepared.build_outcome.receipt.platform != ApplePlatform::Ios {
        return None;
    }
    match SimulatorAppLogStream::start(
        prepared.selected_xcode.as_ref(),
        prepared.backend.target_id(),
        prepared.build_outcome.receipt.target.as_str(),
        &prepared.build_outcome.receipt.bundle_id,
        prepared.verbose,
    ) {
        Ok(stream) => Some(stream),
        Err(error) => {
            eprintln!(
                "warning: failed to start app logs for `{}` on {}: {error:#}",
                prepared.build_outcome.receipt.target,
                prepared.backend.target_name()
            );
            None
        }
    }
}

pub(crate) fn dump_tree_json(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<JsonValue> {
    let prepared = prepare_ui_session(project, platform, true)?;
    prepared.backend.describe_all()
}

pub(crate) fn describe_point_json(
    project: &ProjectContext,
    platform: ApplePlatform,
    x: f64,
    y: f64,
) -> Result<JsonValue> {
    let prepared = prepare_ui_session(project, platform, true)?;
    prepared.backend.describe_point(x, y)
}

pub(crate) fn reset_idb() -> Result<()> {
    ensure_idb_tooling_available()?;
    let mut command = std::process::Command::new("idb");
    command.arg("kill");
    crate::util::run_command(&mut command).context(idb_requirement_message())
}

pub(crate) fn doctor(project: &ProjectContext, platform: ApplePlatform) -> Result<()> {
    match platform {
        ApplePlatform::Ios => {
            ensure_idb_tooling_available()?;
            println!("ui backend: orbit-idb-ios-simulator");
            println!("idb: ok");
            println!("idb_companion: ok");
            Ok(())
        }
        ApplePlatform::Macos => {
            let status = backend::macos_doctor(project)?;
            print_macos_doctor_status(&status);
            ensure_macos_ui_test_requirements(&status)
        }
        _ => bail!(
            "Orbit UI automation currently supports only `--platform ios` and `--platform macos`"
        ),
    }
}

pub(crate) fn attach_backend(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<Box<dyn UiBackend>> {
    match platform {
        ApplePlatform::Ios => {
            ensure_idb_tooling_available()?;
            Ok(Box::new(IosSimulatorBackend::attach(project)?))
        }
        ApplePlatform::Macos => {
            let prepared = prepare_ui_session(project, platform, true)?;
            Ok(prepared.backend)
        }
        _ => bail!(
            "Orbit UI automation currently supports only `--platform ios` and `--platform macos`"
        ),
    }
}

fn print_macos_doctor_status(status: &MacosDoctorStatus) {
    println!("ui backend: orbit-ax-macos");
    println!(
        "accessibility: {}",
        if status.accessibility_trusted {
            "ok"
        } else {
            "missing"
        }
    );
    println!(
        "screen recording: {}",
        if status.screen_capture_access {
            "ok"
        } else {
            "missing"
        }
    );
}

fn ensure_macos_ui_test_requirements(status: &MacosDoctorStatus) -> Result<()> {
    if status.accessibility_trusted && status.screen_capture_access {
        return Ok(());
    }

    let mut missing = Vec::new();
    if !status.accessibility_trusted {
        missing.push(
            "Accessibility access for Orbit or the calling terminal in System Settings > Privacy & Security > Accessibility",
        );
    }
    if !status.screen_capture_access {
        missing.push(
            "Screen Recording access for Orbit or the calling terminal in System Settings > Privacy & Security > Screen Recording",
        );
    }

    bail!(
        "macOS UI automation is not ready.\nMissing:\n  - {}",
        missing.join("\n  - ")
    )
}

struct UiFlowRunner {
    backend: Box<dyn UiBackend>,
    artifacts_dir: PathBuf,
    bundle_id: String,
    stack: Vec<PathBuf>,
    clipboard: Option<String>,
    manual_recording: Option<PathBuf>,
}

impl UiFlowRunner {
    fn new(backend: Box<dyn UiBackend>, artifacts_dir: PathBuf, bundle_id: String) -> Self {
        Self {
            backend,
            artifacts_dir,
            bundle_id,
            stack: Vec::new(),
            clipboard: None,
            manual_recording: None,
        }
    }

    fn execute_flow(
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

        let flow = parse_ui_flow(&flow_path)?;
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
        let video_path = if auto_video_enabled {
            Some(self.artifacts_dir.join(format!(
                "{}.{}",
                sanitize_artifact_name(&flow_name_from_path(&flow_path)),
                self.backend.video_extension()
            )))
        } else {
            None
        };
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
        if invoked_by.is_none() && self.manual_recording.is_some() {
            if let Some(path) = self.manual_recording.take() {
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
        }

        report.finished_at_unix = unix_timestamp_secs();
        report.duration_ms = started.elapsed().as_millis() as u64;
        let passed = matches!(report.status, RunStatus::Passed);
        reports.push(report);
        Ok(passed)
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
                    let nested_path = resolve_relative_flow(flow_path, relative_path);
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
                    flow_path,
                    *times,
                    commands,
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
        flow_path: &Path,
        times: u32,
        commands: &[UiCommand],
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
                flow_path,
                commands,
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
        let result = self.run_leaf_command(command);
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

    fn run_leaf_command(&mut self, command: &UiCommand) -> Result<Option<PathBuf>> {
        match command {
            UiCommand::LaunchApp(command) => {
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
                let Some(text) = element.copied_text.or_else(|| Some(element.label.clone())) else {
                    bail!(
                        "`copyTextFrom` could not resolve text for {}",
                        selector.summary()
                    );
                };
                self.clipboard = Some(text);
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
                    .map(|path| resolve_media_path(flow_path, path))
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
            match self.backend.describe_all() {
                Ok(tree) => {
                    if let Some(element) = find_visible_element_by_selector(&tree, selector) {
                        last_seen = Some(element.label);
                    } else {
                        return Ok(());
                    }
                }
                Err(_) => {}
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

    fn scroll_until_visible(&mut self, command: &UiScrollUntilVisible) -> Result<()> {
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
            match component {
                std::path::Component::Normal(value) => {
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
                _ => {}
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
            match self.backend.describe_all() {
                Ok(tree) => {
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
                Err(_) => {}
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

fn prepare_ui_session(
    project: &ProjectContext,
    platform: ApplePlatform,
    launch_app: bool,
) -> Result<PreparedUiSession> {
    match platform {
        ApplePlatform::Ios => {
            ensure_idb_tooling_available()?;
            let build_outcome = build::build_for_testing_destination(
                project,
                platform,
                DestinationKind::Simulator,
            )?;
            let backend = IosSimulatorBackend::prepare(project, &build_outcome.receipt)?;
            if launch_app {
                backend.launch_app(&build_outcome.receipt.bundle_id, true, &[])?;
            }
            Ok(PreparedUiSession {
                build_outcome,
                backend: Box::new(backend),
                verbose: project.app.verbose,
                selected_xcode: project.selected_xcode.clone(),
            })
        }
        ApplePlatform::Macos => {
            let build_outcome = build::build_for_testing_destination(
                project,
                platform,
                DestinationKind::Simulator,
            )?;
            let backend = MacosBackend::prepare(project, &build_outcome.receipt)?;
            if launch_app {
                backend.launch_app(&build_outcome.receipt.bundle_id, true, &[])?;
            }
            Ok(PreparedUiSession {
                build_outcome,
                backend: Box::new(backend),
                verbose: project.app.verbose,
                selected_xcode: project.selected_xcode.clone(),
            })
        }
        _ => bail!(
            "Orbit UI automation currently supports only `--platform ios` and `--platform macos`"
        ),
    }
}

pub(super) fn idb_requirement_message() -> &'static str {
    "Orbit UI tooling for iOS simulators requires `idb` and `idb_companion` on PATH.\n\nInstall them with:\n  brew tap facebook/fb\n  brew install idb-companion\n  python3 -m pip install fb-idb\n\nIf `idb` was installed with pip, ensure your user Python bin directory is on PATH, for example `~/Library/Python/3.12/bin`."
}

fn ensure_idb_tooling_available() -> Result<()> {
    let mut missing = Vec::new();
    if !path_contains_executable("idb") {
        missing.push("idb");
    }
    if !path_contains_executable("idb_companion") {
        missing.push("idb_companion");
    }
    if missing.is_empty() {
        return Ok(());
    }

    bail!(
        "{}\nMissing: {}.",
        idb_requirement_message(),
        missing
            .into_iter()
            .map(|entry| format!("`{entry}`"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn path_contains_executable(name: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|path| path.join(name).is_file()))
}

fn collect_ui_flow_paths(
    project: &ProjectContext,
    target: &TestTargetManifest,
) -> Result<Vec<PathBuf>> {
    let mut flows = Vec::new();
    let mut seen = HashSet::new();
    for root in &target.sources {
        let resolved = resolve_path(&project.root, root);
        if !resolved.exists() {
            bail!("declared path `{}` does not exist", resolved.display());
        }
        for path in collect_files_with_extensions(&resolved, &["yml", "yaml"])? {
            let canonical = canonical_or_absolute(&path)?;
            if seen.insert(canonical.clone()) {
                flows.push(canonical);
            }
        }
    }
    flows.sort();
    Ok(flows)
}

fn resolve_relative_flow(parent_flow: &Path, relative_path: &Path) -> PathBuf {
    let base_dir = parent_flow
        .parent()
        .expect("flow paths are always expected to have a parent directory");
    if relative_path.is_absolute() {
        relative_path.to_path_buf()
    } else {
        base_dir.join(relative_path)
    }
}

fn resolve_media_path(parent_flow: &Path, media_path: &Path) -> PathBuf {
    let base_dir = parent_flow
        .parent()
        .expect("flow paths are always expected to have a parent directory");
    if media_path.is_absolute() {
        media_path.to_path_buf()
    } else {
        base_dir.join(media_path)
    }
}

fn flow_uses_manual_recording(
    flow_path: &Path,
    commands: &[UiCommand],
    visited: &mut HashSet<PathBuf>,
) -> Result<bool> {
    let canonical = canonical_or_absolute(flow_path)?;
    if !visited.insert(canonical.clone()) {
        return Ok(false);
    }
    commands_use_manual_recording(canonical.as_path(), commands, visited)
}

fn commands_use_manual_recording(
    flow_path: &Path,
    commands: &[UiCommand],
    visited: &mut HashSet<PathBuf>,
) -> Result<bool> {
    for command in commands {
        match command {
            UiCommand::StartRecording(_) | UiCommand::StopRecording => return Ok(true),
            UiCommand::RunFlow(relative_path) => {
                let nested_path = resolve_relative_flow(flow_path, relative_path);
                let nested_flow = parse_ui_flow(&nested_path)?;
                if flow_uses_manual_recording(
                    nested_flow.path.as_path(),
                    nested_flow.commands.as_slice(),
                    visited,
                )? {
                    return Ok(true);
                }
            }
            UiCommand::Repeat { commands, .. } | UiCommand::Retry { commands, .. } => {
                if commands_use_manual_recording(flow_path, commands, visited)? {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        path.canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()))
    } else if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(fs::canonicalize(".")
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path))
    }
}

fn flow_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("flow")
        .to_owned()
}

fn append_report_error(report: &mut FlowRunReport, message: String) {
    match report.error.as_mut() {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&message);
        }
        None => report.error = Some(message),
    }
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn preview_text(value: &str) -> String {
    const LIMIT: usize = 24;
    if value.chars().count() <= LIMIT {
        format!("\"{value}\"")
    } else {
        let preview = value.chars().take(LIMIT).collect::<String>();
        format!("\"{preview}...\"")
    }
}

fn sanitize_artifact_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "artifact".to_owned()
    } else {
        sanitized
    }
}

fn sanitize_extension_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "artifact".to_owned()
    } else {
        sanitized
    }
}

fn directional_points_in_frame(
    frame: UiFrame,
    direction: UiSwipeDirection,
    invert_for_scroll: bool,
) -> ((f64, f64), (f64, f64)) {
    let direction = if invert_for_scroll {
        match direction {
            UiSwipeDirection::Left => UiSwipeDirection::Right,
            UiSwipeDirection::Right => UiSwipeDirection::Left,
            UiSwipeDirection::Up => UiSwipeDirection::Down,
            UiSwipeDirection::Down => UiSwipeDirection::Up,
        }
    } else {
        direction
    };

    let left = frame.x + (frame.width * 0.20);
    let right = frame.x + (frame.width * 0.80);
    let top = frame.y + (frame.height * 0.20);
    let bottom = frame.y + (frame.height * 0.80);
    let center_x = frame.x + (frame.width * 0.50);
    let center_y = frame.y + (frame.height * 0.50);

    match direction {
        UiSwipeDirection::Left => ((right, center_y), (left, center_y)),
        UiSwipeDirection::Right => ((left, center_y), (right, center_y)),
        UiSwipeDirection::Up => ((center_x, bottom), (center_x, top)),
        UiSwipeDirection::Down => ((center_x, top), (center_x, bottom)),
    }
}

fn resolve_point_expr(screen: &UiFrame, point: &UiPointExpr) -> (f64, f64) {
    (
        resolve_coordinate(screen.x, screen.width, point.x),
        resolve_coordinate(screen.y, screen.height, point.y),
    )
}

fn resolve_coordinate(origin: f64, span: f64, coordinate: UiCoordinate) -> f64 {
    match coordinate {
        UiCoordinate::Absolute(value) => value,
        UiCoordinate::Percent(percent) => origin + (span * percent / 100.0),
    }
}

pub(super) fn infer_screen_frame(tree: &JsonValue) -> Option<UiFrame> {
    let mut frames = Vec::new();
    collect_frames(tree, &mut frames);
    frames.into_iter().max_by(|left, right| {
        let left_area = left.width * left.height;
        let right_area = right.width * right.height;
        left_area
            .partial_cmp(&right_area)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn collect_frames(tree: &JsonValue, frames: &mut Vec<UiFrame>) {
    match tree {
        JsonValue::Array(values) => {
            for value in values {
                collect_frames(value, frames);
            }
        }
        JsonValue::Object(map) => {
            if let Some(frame) = extract_frame(map) {
                frames.push(frame);
            }
            for value in map.values() {
                collect_frames(value, frames);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
fn find_element_by_selector(tree: &JsonValue, selector: &UiSelector) -> Option<UiElementMatch> {
    let mut matches = Vec::new();
    collect_element_matches(tree, selector, &mut matches);
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.frame.is_some().cmp(&left.frame.is_some()))
            .then_with(|| left.label.cmp(&right.label))
    });
    matches.into_iter().next()
}

fn find_visible_element_by_selector(
    tree: &JsonValue,
    selector: &UiSelector,
) -> Option<UiElementMatch> {
    let screen = infer_screen_frame(tree);
    let mut matches = Vec::new();
    collect_element_matches(tree, selector, &mut matches);
    matches.retain(|element| {
        let Some(screen) = screen else {
            return true;
        };
        match element.frame {
            Some(frame) => frames_intersect(screen, frame),
            None => true,
        }
    });
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.frame.is_some().cmp(&left.frame.is_some()))
            .then_with(|| left.label.cmp(&right.label))
    });
    matches.into_iter().next()
}

fn find_visible_scroll_container(tree: &JsonValue) -> Option<UiFrame> {
    let screen = infer_screen_frame(tree);
    let mut frames = Vec::new();
    collect_visible_scroll_frames(tree, screen, &mut frames);
    frames.into_iter().max_by(|left, right| {
        let left_area = left.width * left.height;
        let right_area = right.width * right.height;
        left_area
            .partial_cmp(&right_area)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn collect_visible_scroll_frames(
    tree: &JsonValue,
    screen: Option<UiFrame>,
    frames: &mut Vec<UiFrame>,
) {
    match tree {
        JsonValue::Array(values) => {
            for value in values {
                collect_visible_scroll_frames(value, screen, frames);
            }
        }
        JsonValue::Object(map) => {
            if let Some(frame) = extract_frame(map)
                && frame.width > 1.0
                && frame.height > 1.0
                && screen
                    .map(|screen| frames_intersect(screen, frame))
                    .unwrap_or(true)
                && map
                    .get("AXRole")
                    .and_then(JsonValue::as_str)
                    .is_some_and(is_scrollable_role)
            {
                frames.push(frame);
            }
            for value in map.values() {
                collect_visible_scroll_frames(value, screen, frames);
            }
        }
        _ => {}
    }
}

fn is_scrollable_role(role: &str) -> bool {
    matches!(
        role,
        "AXScrollArea"
            | "AXScrollView"
            | "AXTable"
            | "AXOutline"
            | "AXList"
            | "AXCollectionView"
            | "XCUIElementTypeCollectionView"
            | "XCUIElementTypeScrollView"
            | "XCUIElementTypeTable"
    )
}

fn collect_element_matches(
    tree: &JsonValue,
    selector: &UiSelector,
    matches: &mut Vec<UiElementMatch>,
) {
    match tree {
        JsonValue::Array(values) => {
            for value in values {
                collect_element_matches(value, selector, matches);
            }
        }
        JsonValue::Object(map) => {
            if let Some(element) = match_element_object(map, selector) {
                matches.push(element);
            }
            for value in map.values() {
                collect_element_matches(value, selector, matches);
            }
        }
        _ => {}
    }
}

fn match_element_object(
    map: &serde_json::Map<String, JsonValue>,
    selector: &UiSelector,
) -> Option<UiElementMatch> {
    let text_candidates = ["AXLabel", "label", "title", "name", "value", "AXValue"];
    let id_candidates = ["identifier", "AXIdentifier", "id"];

    let (text_score, text_label) = selector
        .text
        .as_deref()
        .map(|needle| best_match_for_keys(map, &text_candidates, needle))
        .unwrap_or((1, None));
    if selector.text.is_some() && text_score == 0 {
        return None;
    }
    let (id_score, id_label) = selector
        .id
        .as_deref()
        .map(|needle| best_match_for_keys(map, &id_candidates, needle))
        .unwrap_or((1, None));
    if selector.id.is_some() && id_score == 0 {
        return None;
    }
    let score = text_score.saturating_add(id_score);
    if score == 0 {
        return None;
    }
    let copied_text = preferred_copy_text(map);
    let label = copied_text
        .clone()
        .or(text_label)
        .or(id_label)
        .or_else(|| selector.text.clone())
        .or_else(|| selector.id.clone())
        .unwrap_or_else(|| selector.summary());

    Some(UiElementMatch {
        label,
        frame: extract_frame(map),
        score,
        copied_text,
    })
}

fn match_score(value: &str, needle: &str) -> u8 {
    if value == needle {
        return 3;
    }
    if value.eq_ignore_ascii_case(needle) {
        return 2;
    }
    if value
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
    {
        return 1;
    }
    0
}

fn best_match_for_keys(
    map: &serde_json::Map<String, JsonValue>,
    keys: &[&str],
    needle: &str,
) -> (u8, Option<String>) {
    let mut best_label = None;
    let mut best_score = 0;
    for key in keys {
        let Some(value) = map.get(*key).and_then(JsonValue::as_str) else {
            continue;
        };
        let score = match_score(value, needle);
        if score > best_score {
            best_score = score;
            best_label = Some(value.to_owned());
        }
    }
    (best_score, best_label)
}

fn preferred_copy_text(map: &serde_json::Map<String, JsonValue>) -> Option<String> {
    ["AXValue", "value", "AXLabel", "label", "title", "name"]
        .into_iter()
        .find_map(|key| map.get(key).and_then(JsonValue::as_str).map(str::to_owned))
}

fn extract_frame(map: &serde_json::Map<String, JsonValue>) -> Option<UiFrame> {
    if let Some(frame) = map.get("frame").and_then(json_value_to_frame) {
        return Some(frame);
    }
    if let Some(frame) = map.get("rect").and_then(json_value_to_frame) {
        return Some(frame);
    }
    if let Some(origin) = map.get("origin").and_then(JsonValue::as_object)
        && let Some(size) = map.get("size").and_then(JsonValue::as_object)
    {
        return Some(UiFrame {
            x: json_number(origin.get("x")?)?,
            y: json_number(origin.get("y")?)?,
            width: json_number(size.get("width")?)?,
            height: json_number(size.get("height")?)?,
        });
    }
    None
}

fn frames_intersect(left: UiFrame, right: UiFrame) -> bool {
    let left_max_x = left.x + left.width;
    let left_max_y = left.y + left.height;
    let right_max_x = right.x + right.width;
    let right_max_y = right.y + right.height;
    left.x < right_max_x && left_max_x > right.x && left.y < right_max_y && left_max_y > right.y
}

fn json_value_to_frame(value: &JsonValue) -> Option<UiFrame> {
    let map = value.as_object()?;
    Some(UiFrame {
        x: json_number(map.get("x")?)?,
        y: json_number(map.get("y")?)?,
        width: json_number(map.get("width")?)?,
        height: json_number(map.get("height")?)?,
    })
}

fn json_number(value: &JsonValue) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        UiCommand, UiSelector, find_element_by_selector, find_visible_element_by_selector,
        find_visible_scroll_container, infer_screen_frame,
    };

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
        let summary = UiCommand::InputText(
            "this text is definitely longer than the preview limit".to_owned(),
        )
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
}
