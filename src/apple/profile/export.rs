use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::apple::xcode::{SelectedXcode, xcrun_command};
use crate::util::{command_output_allow_failure, debug_command};

#[derive(Clone, Copy)]
pub(super) enum TraceExportMode<'a> {
    Toc,
    XPath(&'a str),
}

pub(super) fn debug_export_command(
    trace_path: &Path,
    selected_xcode: Option<&SelectedXcode>,
    mode: TraceExportMode<'_>,
) -> String {
    let mut command = xctrace_export_command_with_xcode(trace_path, selected_xcode);
    apply_trace_export_mode(&mut command, mode);
    debug_command(&command)
}

pub(super) fn capture_xctrace_export(
    trace_path: &Path,
    selected_xcode: Option<&SelectedXcode>,
    mode: TraceExportMode<'_>,
    debug: &str,
) -> Result<String> {
    let started = Instant::now();
    let mut last_error = None;

    while started.elapsed() < Duration::from_secs(10) {
        let mut command = xctrace_export_command_with_xcode(trace_path, selected_xcode);
        apply_trace_export_mode(&mut command, mode);
        let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
        if success {
            return Ok(stdout);
        }

        let stderr = stderr.trim();
        if !stderr.is_empty() {
            last_error = Some(stderr.to_owned());
        }
        thread::sleep(Duration::from_millis(100));
    }

    if let Some(error) = last_error {
        bail!(
            "timed out waiting for `{debug}` to succeed for {}; last export error: {error}",
            trace_path.display()
        );
    }

    bail!(
        "timed out waiting for `{debug}` to succeed for {}",
        trace_path.display()
    )
}

pub(super) fn wait_for_exportable_trace(
    output_path: &Path,
    selected_xcode: Option<&SelectedXcode>,
    debug: &str,
) -> Result<()> {
    let started = Instant::now();
    let mut last_error = None;

    while started.elapsed() < Duration::from_secs(10) {
        let mut command = xctrace_export_command_with_xcode(output_path, selected_xcode);
        command.arg("--toc");
        let (success, _stdout, stderr) = command_output_allow_failure(&mut command)?;
        if success {
            return Ok(());
        }

        let stderr = stderr.trim();
        if !stderr.is_empty() {
            last_error = Some(stderr.to_owned());
        }
        thread::sleep(Duration::from_millis(100));
    }

    if let Some(error) = last_error {
        bail!(
            "timed out waiting for `{debug}` to finalize an exportable trace at {}; last export error: {error}",
            output_path.display()
        );
    }

    bail!(
        "timed out waiting for `{debug}` to finalize an exportable trace at {}",
        output_path.display()
    )
}

fn xctrace_export_command_with_xcode(
    trace_path: &Path,
    selected_xcode: Option<&SelectedXcode>,
) -> std::process::Command {
    let mut command = xcrun_command(selected_xcode);
    command.arg("xctrace");
    command.arg("export");
    command.arg("--input");
    command.arg(trace_path);
    command
}

fn apply_trace_export_mode(command: &mut std::process::Command, mode: TraceExportMode<'_>) {
    match mode {
        TraceExportMode::Toc => {
            command.arg("--toc");
        }
        TraceExportMode::XPath(xpath) => {
            command.arg("--xpath");
            command.arg(xpath);
        }
    }
}
