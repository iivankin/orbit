use std::env;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;
use uuid::Uuid;

#[path = "ui/backend.rs"]
pub(crate) mod backend;
#[path = "ui/flow.rs"]
mod flow;
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
use self::trace::{
    UiTracePlan, apply_macos_ui_trace_prelaunch, plan_macos_ui_trace,
    prepare_macos_ui_trace_launch_command,
};
use crate::apple::build::toolchain::DestinationKind;
use crate::apple::logs::SimulatorAppLogStream;
use crate::apple::{build, runtime};
use crate::cli::TestArgs;
use crate::context::ProjectContext;
use crate::manifest::ApplePlatform;
use crate::util::{ensure_dir, format_elapsed, print_success, write_json_file};

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
    let flow_paths = collect_ui_flow_paths(project, ui_tests)?;
    if flow_paths.is_empty() {
        bail!("`tests.ui.sources` did not contain any `.yml` or `.yaml` files");
    }
    let trace_plan = match (platform, args.trace) {
        (ApplePlatform::Ios, kind) => {
            crate::apple::profile::ensure_simulator_profiling_supported(kind)?;
            None
        }
        (ApplePlatform::Macos, Some(_)) => Some(plan_macos_ui_trace(flow_paths.as_slice())?),
        _ => None,
    };
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
    let trace_recording = start_ui_trace_recording(
        project,
        args,
        prepared.build_outcome.receipt.bundle_id.as_str(),
        &prepared,
        trace_plan.as_ref(),
    )?;
    let run_result = (|| {
        let report_path = run_root.join("report.json");
        let mut flow_reports = Vec::new();
        let _app_logs = start_ui_app_logs(&prepared);
        let mut runner = UiFlowRunner::new(
            prepared.backend,
            artifacts_dir.clone(),
            prepared.build_outcome.receipt.bundle_id.clone(),
            trace_plan
                .as_ref()
                .is_some_and(|plan| plan.launch.is_some()),
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
    })();
    let trace_result = trace_recording
        .map(|(kind, recording)| crate::apple::profile::finish_started_trace(kind, recording))
        .transpose();
    match (run_result, trace_result) {
        (Err(run_error), Err(trace_error)) => {
            Err(run_error.context(format!("also failed to finalize UI trace: {trace_error:#}")))
        }
        (Err(run_error), Ok(Some(path))) => {
            println!("trace: {}", path.display());
            Err(run_error)
        }
        (Err(run_error), Ok(None)) => Err(run_error),
        (Ok(()), Err(trace_error)) => Err(trace_error),
        (Ok(()), Ok(Some(path))) => {
            println!("trace: {}", path.display());
            Ok(())
        }
        (Ok(()), Ok(None)) => Ok(()),
    }
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

pub(crate) fn idb_requirement_message() -> &'static str {
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

fn start_ui_trace_recording(
    project: &ProjectContext,
    args: &TestArgs,
    bundle_id: &str,
    prepared: &PreparedUiSession,
    trace_plan: Option<&UiTracePlan>,
) -> Result<
    Option<(
        crate::cli::ProfileKind,
        crate::apple::profile::TraceRecording,
    )>,
> {
    let Some(kind) = args.trace else {
        return Ok(None);
    };

    match prepared.build_outcome.receipt.platform {
        ApplePlatform::Macos => {
            let plan = trace_plan.expect("macOS trace plan must be prepared when tracing UI tests");
            let target = project.resolved_manifest.resolve_target(None)?;
            crate::apple::signing::prepare_macos_bundle_for_debug_tracing(
                project,
                target,
                &prepared.build_outcome.receipt.bundle_path,
            )?;
            apply_macos_ui_trace_prelaunch(prepared.backend.as_ref(), bundle_id, plan)?;
            let launch_command = prepare_macos_ui_trace_launch_command(
                project,
                &prepared.build_outcome.receipt,
                plan.launch.as_ref(),
            )?;
            let trace = crate::apple::profile::start_optional_launched_command_trace(
                &project.root,
                project.selected_xcode.as_ref(),
                project.app.interactive,
                Some(kind),
                &launch_command,
                None,
            )?
            .expect("trace kind should produce a launched trace");
            let traced_executable = launch_command
                .first()
                .expect("macOS trace launch command must include an executable");
            let recorder_pid = crate::apple::profile::trace_recording_process_id(&trace.1);
            prepared.backend.pin_running_target_by_executable(
                Path::new(traced_executable),
                Some(recorder_pid),
            )?;
            Ok(Some(trace))
        }
        ApplePlatform::Ios => {
            crate::apple::profile::ensure_simulator_profiling_supported(Some(kind))?;
            Ok(None)
        }
        _ => bail!(
            "UI test profiling currently supports only `--platform macos`; {} UI automation is not traceable yet",
            prepared.build_outcome.receipt.platform
        ),
    }
}

fn path_contains_executable(name: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|path| path.join(name).is_file()))
}

#[cfg(test)]
#[path = "ui/tests.rs"]
mod tests;
