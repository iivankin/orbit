use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;
use uuid::Uuid;

#[path = "ui/backend.rs"]
pub(crate) mod backend;
#[path = "ui/flow.rs"]
mod flow;
#[path = "ui/idb.rs"]
mod idb;
#[path = "ui/matching.rs"]
mod matching;
#[path = "ui/model.rs"]
mod model;
#[path = "ui/parser.rs"]
mod parser;
#[path = "ui/report.rs"]
mod report;
#[path = "ui/runner.rs"]
mod runner;
#[path = "ui/schema.rs"]
mod schema;
#[path = "ui/trace.rs"]
mod trace;

use self::backend::{IosSimulatorBackend, MacosBackend, MacosDoctorStatus, UiBackend};
use self::flow::collect_ui_flow_paths;
#[cfg(test)]
use self::matching::{
    find_element_by_selector, find_visible_element_by_selector, find_visible_scroll_container,
    infer_screen_frame,
};
pub(crate) use self::model::{
    UiCommand, UiCoordinate, UiCrashDeleteRequest, UiCrashQuery, UiDragAndDrop, UiElementScroll,
    UiElementSwipe, UiExtendedWaitUntil, UiFlow, UiFlowConfig, UiHardwareButton, UiKeyModifier,
    UiKeyPress, UiLaunchApp, UiLocationPoint, UiPermissionConfig, UiPermissionSetting,
    UiPermissionState, UiPointExpr, UiPressKey, UiScrollUntilVisible, UiSelector, UiSwipe,
    UiSwipeDirection, UiTravel,
};
use self::report::{RunStatus, UiTestRunReport, unix_timestamp_secs};
use self::runner::UiFlowRunner;
pub(crate) use self::schema::{schema_json, schema_text};
use self::trace::MacosUiTraceRuntime;
use crate::apple::build::toolchain::DestinationKind;
use crate::apple::logs::SimulatorAppLogStream;
use crate::apple::{build, runtime};
use crate::cli::{ProfileKind, TestArgs, UiCleanTraceTempArgs};
use crate::context::ProjectContext;
use crate::manifest::ApplePlatform;
use crate::util::{ensure_dir, format_elapsed, human_bytes, print_success, write_json_file};

struct PreparedUiSession {
    build_outcome: crate::apple::build::pipeline::BuildOutcome,
    backend: Box<dyn UiBackend>,
    verbose: bool,
    selected_xcode: Option<crate::apple::xcode::SelectedXcode>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TempTraceCleanupSummary {
    scanned_files: usize,
    removed_files: usize,
    skipped_recent_files: usize,
    freed_bytes: u64,
}

pub fn run_ui_tests(project: &ProjectContext, args: &TestArgs) -> Result<()> {
    let ui_tests = project
        .resolved_manifest
        .tests
        .ui
        .as_deref()
        .context("manifest does not declare `tests.ui`")?;
    let platform = runtime::resolve_platform(
        project,
        args.platform.map(runtime::apple_platform_from_cli),
        "Select a platform to test",
    )?;
    let flow_paths = collect_ui_flow_paths(project, ui_tests, args.flows.as_slice())?;
    if flow_paths.is_empty() {
        bail!("`tests.ui` did not contain any `.yml` or `.yaml` files");
    }
    run_ui_flow_paths(project, platform, flow_paths, args.trace, args.focus)
}

pub fn run_ui_command(
    project: &ProjectContext,
    platform: ApplePlatform,
    command: UiCommand,
    focus_after_launch: bool,
) -> Result<()> {
    if platform == ApplePlatform::Macos {
        let status = backend::macos_doctor(project)?;
        ensure_macos_ui_test_requirements(&status)?;
    }

    let run_root = project.project_paths.artifacts_dir.join("ui").join(format!(
        "{}-{}",
        unix_timestamp_secs(),
        Uuid::new_v4()
    ));
    ensure_dir(&run_root)?;

    let bundle_id = project
        .resolved_manifest
        .resolve_target(None)?
        .bundle_id
        .clone();
    let backend = backend_for_ui_command(project, platform, &command, &bundle_id)?;
    let mut runner = UiFlowRunner::new(backend, run_root, bundle_id, focus_after_launch, None);
    let command_summary = command.summary();
    if let Some(path) = runner.run_leaf_command(&command)? {
        println!("artifact: {}", path.display());
    }
    print_success(format!(
        "UI command `{command_summary}` completed on {}.",
        runner.backend.target_name()
    ));
    Ok(())
}

fn run_ui_flow_paths(
    project: &ProjectContext,
    platform: ApplePlatform,
    flow_paths: Vec<std::path::PathBuf>,
    trace: Option<ProfileKind>,
    focus_after_launch: bool,
) -> Result<()> {
    if platform == ApplePlatform::Ios {
        crate::apple::profile::ensure_simulator_profiling_supported(trace)?;
    }
    if platform == ApplePlatform::Macos {
        let status = backend::macos_doctor(project)?;
        ensure_macos_ui_test_requirements(&status)?;
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
    if platform == ApplePlatform::Macos && trace.is_some() {
        let target = project.resolved_manifest.resolve_target(None)?;
        crate::apple::signing::prepare_macos_bundle_for_debug_tracing(
            project,
            target,
            &prepared.build_outcome.receipt.bundle_path,
        )?;
    }
    (|| {
        let report_path = run_root.join("report.json");
        let mut flow_reports = Vec::new();
        let _app_logs = start_ui_app_logs(&prepared);
        let mut runner = UiFlowRunner::new(
            prepared.backend,
            artifacts_dir.clone(),
            prepared.build_outcome.receipt.bundle_id.clone(),
            focus_after_launch,
            if platform == ApplePlatform::Macos {
                trace.map(|kind| {
                    MacosUiTraceRuntime::new(
                        project.root.clone(),
                        &project.project_paths,
                        project.selected_xcode.clone(),
                        &prepared.build_outcome.receipt,
                        kind,
                        project.app.interactive,
                    )
                })
            } else {
                None
            },
        );
        let mut has_failures = false;
        for flow_path in &flow_paths {
            if !runner.execute_flow(flow_path, None, &mut flow_reports)? {
                has_failures = true;
            }
        }
        if trace.is_none()
            && let Err(error) = runner
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
            bail!("UI flow run failed; see {}", report_path.display());
        }

        print_success(format!(
            "UI flows passed for `{}` on {} using {} flow(s) in {}.",
            prepared.build_outcome.receipt.target,
            runner.backend.target_name(),
            flow_paths.len(),
            format_elapsed(started.elapsed())
        ));
        Ok(())
    })()
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
    idb::ensure_tooling_available()?;
    let mut command = std::process::Command::new("idb");
    command.arg("kill");
    crate::util::run_command(&mut command).context(idb::requirement_message())
}

pub(crate) fn doctor(project: &ProjectContext, platform: ApplePlatform) -> Result<()> {
    match platform {
        ApplePlatform::Ios => {
            idb::ensure_tooling_available()?;
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

pub(crate) fn clean_trace_temp(args: &UiCleanTraceTempArgs) -> Result<()> {
    let temp_dir = std::env::temp_dir();
    let summary = cleanup_macos_trace_temp_dir(
        &temp_dir,
        args.all,
        Duration::from_secs(args.stale_minutes.unwrap_or(60).saturating_mul(60)),
    )?;
    println!("temp_dir: {}", temp_dir.display());
    println!("scanned_temp_trace_files: {}", summary.scanned_files);
    println!("removed_temp_trace_files: {}", summary.removed_files);
    println!(
        "skipped_recent_temp_trace_files: {}",
        summary.skipped_recent_files
    );
    println!(
        "freed_temp_trace_bytes: {} ({})",
        summary.freed_bytes,
        human_bytes(summary.freed_bytes)
    );
    Ok(())
}

pub(crate) fn attach_backend(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<Box<dyn UiBackend>> {
    match platform {
        ApplePlatform::Ios => {
            idb::ensure_tooling_available()?;
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

fn backend_for_ui_command(
    project: &ProjectContext,
    platform: ApplePlatform,
    command: &UiCommand,
    manifest_bundle_id: &str,
) -> Result<Box<dyn UiBackend>> {
    let needs_prepared_session = matches!(
        command,
        UiCommand::LaunchApp(launch)
            if launch.app_id.as_deref().unwrap_or(manifest_bundle_id) == manifest_bundle_id
    ) || matches!(
        command,
        UiCommand::ClearState(app_id)
            if app_id.as_deref().unwrap_or(manifest_bundle_id) == manifest_bundle_id
    );

    if needs_prepared_session {
        return Ok(prepare_ui_session(project, platform, false)?.backend);
    }

    attach_backend(project, platform)
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

fn cleanup_macos_trace_temp_dir(
    temp_dir: &Path,
    remove_all: bool,
    stale_after: Duration,
) -> Result<TempTraceCleanupSummary> {
    let mut summary = TempTraceCleanupSummary::default();
    let now = SystemTime::now();
    for entry in
        fs::read_dir(temp_dir).with_context(|| format!("failed to read {}", temp_dir.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with("instruments") || !name.ends_with(".ktrace") {
            continue;
        }

        summary.scanned_files += 1;
        let metadata = entry.metadata()?;
        if !remove_all {
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
            if age < stale_after {
                summary.skipped_recent_files += 1;
                continue;
            }
        }

        let bytes = metadata.len();
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove temp trace {}", path.display()))?;
        println!("removed_temp_trace: {}", path.display());
        summary.removed_files += 1;
        summary.freed_bytes += bytes;
    }
    Ok(summary)
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

fn prepare_ui_session(
    project: &ProjectContext,
    platform: ApplePlatform,
    launch_app: bool,
) -> Result<PreparedUiSession> {
    let destination = ui_testing_destination(platform);
    match platform {
        ApplePlatform::Ios => {
            idb::ensure_tooling_available()?;
            let build_outcome =
                build::build_for_testing_destination(project, platform, destination)?;
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
            let build_outcome =
                build::build_for_testing_destination(project, platform, destination)?;
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

fn ui_testing_destination(platform: ApplePlatform) -> DestinationKind {
    match platform {
        ApplePlatform::Macos => DestinationKind::Device,
        _ => DestinationKind::Simulator,
    }
}

#[cfg(test)]
#[path = "ui/tests.rs"]
mod tests;
