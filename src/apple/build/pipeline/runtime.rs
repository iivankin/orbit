use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use plist::Value;
use serde::Deserialize;
use tempfile::{NamedTempFile, tempdir};

use crate::apple::build::receipt::BuildReceipt;
use crate::apple::logs::{DeviceConsoleRelay, MacosInferiorLogRelay, SimulatorAppLogStream};
use crate::apple::script::{
    lldb_quote_arg, macos_quit_applescript, macos_xcode_log_redirect_env, shell_quote_arg,
    tcl_quote_arg,
};
use crate::apple::simulator::SimulatorDevice;
use crate::apple::xcode::{
    SelectedXcode, lldb_path as selected_xcode_lldb_path, open_simulator_command,
    xcodebuild_command, xcrun_command,
};
use crate::cli::ProfileKind;
use crate::context::ProjectContext;
use crate::manifest::ApplePlatform;
use crate::util::{
    CliSpinner, combine_command_output, command_output_allow_failure, debug_command, ensure_dir,
    prompt_select, run_command, timestamp_slug,
};

pub(super) fn validate_run_platform(platform: ApplePlatform) -> Result<()> {
    match platform {
        ApplePlatform::Ios
        | ApplePlatform::Macos
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => Ok(()),
    }
}

pub(super) fn run_on_macos(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    trace: Option<ProfileKind>,
) -> Result<()> {
    stop_existing_macos_application(receipt)?;
    if let Some(kind) = trace {
        let target = project
            .resolved_manifest
            .resolve_target(Some(&receipt.target))
            .with_context(|| format!("missing manifest target `{}`", receipt.target))?;
        crate::apple::signing::prepare_macos_bundle_for_debug_tracing(
            project,
            target,
            &receipt.bundle_path,
        )?;
        let launch_target = prepare_macos_trace_launch_executable(project, receipt)?;
        println!(
            "Launching {} under xctrace on the local Mac. Orbit will wait for the recording to finish; press Ctrl-C to stop.",
            receipt.bundle_id
        );
        if project.app.interactive {
            spawn_macos_focus_helper(receipt.target.as_str())?;
        }
        let trace = crate::apple::profile::start_optional_launched_process_trace(
            &project.root,
            project.selected_xcode.as_ref(),
            project.app.interactive,
            Some(kind),
            &launch_target,
            None,
        )?
        .expect("trace kind should produce a launched trace");
        return crate::apple::profile::wait_for_launched_trace_exit(trace.0, trace.1);
    }

    println!(
        "Launching {} on the local Mac. Orbit will hand control to the app until it exits; press Ctrl-C to stop.",
        receipt.bundle_id
    );
    run_macos_application(project, receipt)
}

fn stop_existing_macos_application(receipt: &BuildReceipt) -> Result<()> {
    let mut script = Command::new("osascript");
    script.args(["-e", &macos_quit_applescript(receipt.bundle_id.as_str())]);
    let _ = command_output_allow_failure(&mut script)?;

    let executable = macos_executable_path(receipt)?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if !macos_process_running(&executable)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    let mut command = Command::new("pkill");
    command.args(["-f"]);
    command.arg(executable);
    let _ = command_output_allow_failure(&mut command)?;
    Ok(())
}

fn macos_process_running(executable: &Path) -> Result<bool> {
    let mut command = Command::new("pgrep");
    command.args(["-f"]);
    command.arg(executable);
    let (success, _stdout, _stderr) = command_output_allow_failure(&mut command)?;
    Ok(success)
}

fn write_temp_script(contents: &str, context: &str, executable: bool) -> Result<NamedTempFile> {
    let script = NamedTempFile::new().with_context(|| format!("failed to create {context}"))?;
    fs::write(script.path(), contents.as_bytes())
        .with_context(|| format!("failed to write {}", script.path().display()))?;

    if executable {
        let mut chmod = Command::new("chmod");
        chmod.args(["+x"]);
        chmod.arg(script.path());
        run_command(&mut chmod)?;
    }

    Ok(script)
}

fn run_inherited_shell_script(script_path: &Path) -> Result<()> {
    let mut command = Command::new("/bin/zsh");
    command.arg(script_path);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());
    run_command(&mut command)
}

fn run_macos_application(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    let executable = macos_executable_path(receipt)?;
    let executable = executable
        .to_str()
        .context("macOS executable path contains invalid UTF-8")?;
    let launch_dir = tempdir().context("failed to create macOS run tempdir")?;
    let log_pipe = launch_dir.path().join("inferior-stdio.pipe");
    let lldb_script = launch_dir.path().join("run.expect");
    let (_log_relay, _log_pipe_anchor) =
        create_macos_inferior_log_relay(project, receipt, &log_pipe)?;

    fs::write(
        &lldb_script,
        lldb_expect_macos_run_script(project.selected_xcode.as_ref())?.as_bytes(),
    )
    .with_context(|| format!("failed to write {}", lldb_script.display()))?;
    // Drive the local macOS app through a small wrapper so Orbit can:
    // - launch the app through an LLDB/debugserver path that matches Xcode more closely,
    // - stream stdout/stderr and translated os_log records through a named pipe,
    // - and quit the app when Orbit receives INT/TERM/HUP.
    let script_contents = format!(
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
        lldb_script = shell_quote_arg(
            lldb_script
                .to_str()
                .context("macOS LLDB script path contains invalid UTF-8")?,
        ),
        executable = shell_quote_arg(executable),
        log_pipe = shell_quote_arg(
            log_pipe
                .to_str()
                .context("macOS log pipe path contains invalid UTF-8")?,
        ),
        quit_script = shell_quote_arg(&macos_quit_applescript(&receipt.bundle_id)),
    );
    let script = write_temp_script(&script_contents, "macOS launch wrapper", true)?;

    if project.app.interactive {
        spawn_macos_focus_helper(receipt.target.as_str())?;
        return run_macos_wrapper_session(script.path());
    }

    run_inherited_shell_script(script.path())
}

pub(super) fn debug_on_macos(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    let executable = macos_executable_path(receipt)?;
    stop_existing_macos_application(receipt)?;
    let launch_dir = tempdir().context("failed to create macOS debug tempdir")?;
    let log_pipe = launch_dir.path().join("inferior-stdio.pipe");
    let lldb_script = launch_dir.path().join("debug.expect");
    let (_log_relay, _log_pipe_anchor) =
        create_macos_inferior_log_relay(project, receipt, &log_pipe)?;
    fs::write(
        &lldb_script,
        lldb_expect_macos_launch_script(project.selected_xcode.as_ref())?.as_bytes(),
    )
    .with_context(|| format!("failed to write {}", lldb_script.display()))?;
    println!(
        "Launching LLDB for {} on the local Mac. Orbit will launch the app and keep LLDB attached while it runs. Use `quit` to end the session; Ctrl-C exits LLDB and stops the app.",
        receipt.bundle_id
    );

    if project.app.interactive {
        spawn_macos_focus_helper(receipt.target.as_str())?;
    }

    run_macos_debug_session(project, receipt, &executable, &lldb_script, &log_pipe)
}

fn spawn_macos_focus_helper(process_name: &str) -> Result<()> {
    let mut command = Command::new("/bin/zsh");
    command.arg("-lc").arg(format!(
        r#"for _ in {{1..100}}; do
  pid=$(/usr/bin/pgrep -n -x {process_name} 2>/dev/null || true)
  if [[ -n "$pid" ]]; then
    for _ in {{1..20}}; do
      /usr/bin/osascript - "$pid" <<'APPLESCRIPT' >/dev/null 2>&1 && exit 0
on run argv
  set targetPid to (item 1 of argv) as integer
  tell application "System Events"
    set targetProcess to first process whose unix id is targetPid
    if (count of windows of targetProcess) = 0 then
      error number 1
    end if
    set frontmost of targetProcess to true
  end tell
end run
APPLESCRIPT
      sleep 0.1
    done
  fi
  sleep 0.1
done
exit 0"#,
        process_name = shell_quote_arg(process_name),
    ));
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command
        .spawn()
        .with_context(|| "failed to start macOS focus helper".to_owned())?;
    Ok(())
}

pub(super) fn run_on_simulator(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    trace: Option<ProfileKind>,
) -> Result<()> {
    crate::apple::profile::ensure_simulator_profiling_supported(trace)?;

    let device = prepare_simulator_installation(project, receipt)?;
    let _app_logs = start_simulator_app_logs(project, &device, receipt);

    println!(
        "Launching {} on {}. Orbit will stay attached to the simulator console; press Ctrl-C to stop.",
        receipt.bundle_id, device.name
    );

    let script = write_temp_script(
        &simulator_run_wrapper_script(project.selected_xcode.as_ref(), &device, receipt)?,
        "iOS simulator launch wrapper",
        true,
    )?;
    let result = if project.app.interactive {
        run_macos_wrapper_session(script.path())
    } else {
        run_inherited_shell_script(script.path())
    };
    debug_assert!(trace.is_none());
    result
}

pub(super) fn debug_on_simulator(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    let device = prepare_simulator_installation(project, receipt)?;
    let executable = bundle_debug_executable_path(receipt)?;
    let _app_logs = start_simulator_app_logs(project, &device, receipt);

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB, attach, and continue the app. Use `quit` to end the session; Ctrl-C exits LLDB and stops the app.",
        receipt.bundle_id, device.name
    );

    let mut launch = xcrun_command(project.selected_xcode.as_ref());
    launch.args([
        "simctl",
        "launch",
        "--wait-for-debugger",
        "--terminate-running-process",
        &device.udid,
        &receipt.bundle_id,
    ]);
    run_command(&mut launch)?;

    run_lldb_simulator_attach_session(
        project.selected_xcode.as_ref(),
        &executable,
        simulator_process_name(receipt),
    )
}

pub(super) fn run_on_device(
    project: &ProjectContext,
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
    trace: Option<ProfileKind>,
) -> Result<()> {
    let installed = install_on_device(project.selected_xcode.as_ref(), device, receipt)?;
    if let Some(kind) = trace {
        println!(
            "Launching {} under xctrace on {}. Orbit will wait for the recording to finish; press Ctrl-C to stop.",
            receipt.bundle_id,
            device.name()
        );
        return run_on_device_with_trace(project, device, &installed.installation_url, kind);
    }

    println!(
        "Launching {} on {}. Orbit will stay attached to the device console; press Ctrl-C to stop.",
        receipt.bundle_id,
        device.name()
    );

    let remote_bundle_path = remote_app_bundle_path(&installed.installation_url)?;
    launch_device_console_process(
        project.selected_xcode.as_ref(),
        device,
        &remote_bundle_path,
        receipt.target.as_str(),
        project.app.verbose,
    )
}

pub(super) fn debug_on_device(
    project: &ProjectContext,
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
) -> Result<()> {
    if receipt.platform == ApplePlatform::Ios {
        return debug_on_ios_device(project, device, receipt);
    }

    let installed = install_on_device(project.selected_xcode.as_ref(), device, receipt)?;
    let executable = bundle_debug_executable_path(receipt)?;
    ensure_device_is_unlocked_for_debugging(
        project.selected_xcode.as_ref(),
        device,
        receipt.platform,
    )?;
    let symbol_root = ensure_device_symbols_available(project, device, receipt.platform)?;
    ensure_device_is_unlocked_for_debugging(
        project.selected_xcode.as_ref(),
        device,
        receipt.platform,
    )?;

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB, attach, and continue the app. Use `quit` to end the session; Ctrl-C interrupts the target.",
        receipt.bundle_id,
        device.name()
    );

    let remote_bundle_path = remote_app_bundle_path(&installed.installation_url)?;
    let launch = launch_device_app(
        project.selected_xcode.as_ref(),
        device,
        &remote_bundle_path,
        true,
    )?;

    let mut command = Command::new("lldb");
    command.arg("--file").arg(&executable);
    if let Some(symbol_root) = &symbol_root {
        let symbol_root = symbol_root
            .to_str()
            .context("device symbol cache path contains invalid UTF-8")?;
        let symbol_root = lldb_quote_arg(symbol_root);
        command.arg("-o").arg(format!(
            "settings append target.exec-search-paths {symbol_root}"
        ));
        command.arg("-o").arg(format!(
            "settings append target.debug-file-search-paths {symbol_root}"
        ));
    }
    command
        .arg("-o")
        .arg(format!("device select {}", device.identifier));
    command.arg("-o").arg(format!(
        "device process attach -c -p {}",
        launch.process_identifier
    ));
    if let Some(bundle_root) = receipt.bundle_path.parent() {
        command.current_dir(bundle_root);
    }
    run_command(&mut command)
}

pub(super) fn select_physical_device(
    project: &ProjectContext,
    requested_identifier: Option<&str>,
    platform: ApplePlatform,
) -> Result<PhysicalDevice> {
    let devices = list_devicectl_devices(project.selected_xcode.as_ref(), platform)?;
    select_physical_device_from_candidates(
        devices,
        requested_identifier,
        project.app.interactive,
        platform,
    )
}

fn select_physical_device_from_candidates(
    mut devices: Vec<PhysicalDevice>,
    requested_identifier: Option<&str>,
    interactive: bool,
    platform: ApplePlatform,
) -> Result<PhysicalDevice> {
    if let Some(identifier) = requested_identifier {
        return devices
            .into_iter()
            .find(|device| device.matches(identifier))
            .with_context(|| format!("no connected {platform} device matched `{identifier}`"));
    }

    if devices.is_empty() {
        bail!("no connected {platform} devices were found through `devicectl`");
    }

    if devices.len() == 1 {
        return Ok(devices.remove(0));
    }

    if !interactive {
        let available = devices
            .iter()
            .map(PhysicalDevice::selection_label)
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "multiple connected {platform} devices were found; pass `--device-id` to choose one: {available}"
        );
    }

    let labels = devices
        .iter()
        .map(PhysicalDevice::selection_label)
        .collect::<Vec<_>>();
    let index = prompt_select("Select a physical device", &labels)?;
    Ok(devices.remove(index))
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCtlEnvelope<T> {
    result: T,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceListResult {
    devices: Vec<PhysicalDevice>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct PhysicalDevice {
    identifier: String,
    #[serde(rename = "deviceProperties")]
    device_properties: PhysicalDeviceProperties,
    #[serde(rename = "hardwareProperties")]
    hardware_properties: PhysicalHardwareProperties,
}

impl PhysicalDevice {
    pub(super) fn provisioning_udid(&self) -> &str {
        &self.hardware_properties.udid
    }

    fn name(&self) -> &str {
        &self.device_properties.name
    }

    fn matches(&self, identifier: &str) -> bool {
        self.identifier == identifier
            || self.hardware_properties.udid == identifier
            || self.device_properties.name == identifier
    }

    fn selection_label(&self) -> String {
        format!(
            "{} ({})",
            self.device_properties.name, self.hardware_properties.udid
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalDeviceProperties {
    name: String,
    #[serde(rename = "osBuildUpdate")]
    os_build_update: Option<String>,
    #[serde(rename = "osVersionNumber")]
    os_version_number: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalHardwareProperties {
    #[serde(rename = "cpuType")]
    cpu_type: PhysicalCpuType,
    platform: String,
    #[serde(rename = "productType")]
    product_type: Option<String>,
    udid: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalCpuType {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct InstalledApplicationsResult {
    #[serde(rename = "installedApplications")]
    installed_applications: Vec<InstalledApplication>,
}

#[derive(Debug, Clone, Deserialize)]
struct InstalledApplication {
    #[serde(rename = "installationURL")]
    installation_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LaunchedProcessResult {
    process: DeviceLaunchedProcess,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceLaunchedProcess {
    #[serde(rename = "processIdentifier")]
    process_identifier: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct RunningProcessesResult {
    #[serde(rename = "runningProcesses", default)]
    running_processes: Vec<DeviceRunningProcess>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceRunningProcess {
    executable: Option<String>,
    #[serde(rename = "processIdentifier")]
    process_identifier: u64,
}

pub(crate) fn macos_executable_path(receipt: &BuildReceipt) -> Result<PathBuf> {
    let bundle_binary = receipt
        .bundle_path
        .join("Contents")
        .join("MacOS")
        .join(&receipt.target);
    if bundle_binary.exists() {
        return Ok(bundle_binary);
    }

    if receipt.bundle_path.is_file() {
        return Ok(receipt.bundle_path.clone());
    }
    if receipt.artifact_path.is_file() {
        return Ok(receipt.artifact_path.clone());
    }

    bail!(
        "failed to find a runnable macOS executable inside {}",
        receipt.bundle_path.display()
    )
}

fn debug_on_ios_device(
    project: &ProjectContext,
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
) -> Result<()> {
    let installed = install_on_device(project.selected_xcode.as_ref(), device, receipt)?;
    let executable = bundle_debug_executable_path(receipt)?;
    ensure_device_is_unlocked_for_debugging(
        project.selected_xcode.as_ref(),
        device,
        receipt.platform,
    )?;
    let symbol_root = ensure_device_symbols_available(project, device, receipt.platform)?;
    ensure_device_is_unlocked_for_debugging(
        project.selected_xcode.as_ref(),
        device,
        receipt.platform,
    )?;

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB and attach to the launched app. Use `quit` to end the session; Ctrl-C exits LLDB and stops the app.",
        receipt.bundle_id,
        device.name()
    );

    let mut launch = spawn_ios_debug_launch_session(
        project.selected_xcode.as_ref(),
        device,
        &receipt.bundle_id,
        receipt.target.as_str(),
        project.app.verbose,
    )?;
    let process = wait_for_device_process_for_installation(
        project.selected_xcode.as_ref(),
        device,
        &installed.installation_url,
        Duration::from_secs(15),
        Some(&mut launch),
    )?;

    let result = run_lldb_device_attach_session(
        device,
        &executable,
        process.process_identifier,
        symbol_root.as_deref(),
    );

    let _ = launch.kill();
    let _ = launch.wait();

    result
}

fn run_on_device_with_trace(
    project: &ProjectContext,
    device: &PhysicalDevice,
    installation_url: &str,
    kind: ProfileKind,
) -> Result<()> {
    // On physical iOS devices, `xctrace --launch` is reliable only when given the
    // installed remote `.app` path; bundle IDs and remote executables produced
    // broken traces in live validation.
    let remote_bundle_path = remote_app_bundle_path(installation_url)?;
    let trace = crate::apple::profile::start_optional_launched_process_trace(
        &project.root,
        project.selected_xcode.as_ref(),
        project.app.interactive,
        Some(kind),
        &remote_bundle_path,
        Some(device.provisioning_udid()),
    )?
    .expect("trace kind should produce a launched trace");
    crate::apple::profile::wait_for_launched_trace_exit(trace.0, trace.1)
}

pub(crate) fn prepare_macos_trace_launch_executable(
    project: &ProjectContext,
    receipt: &BuildReceipt,
) -> Result<String> {
    let launch_dir = project
        .project_paths
        .artifacts_dir
        .join("profiles")
        .join("launch-targets");
    ensure_dir(&launch_dir)?;
    let launch_alias = launch_dir.join(format!("{}-{}.app", timestamp_slug(), receipt.target));
    remove_existing_trace_launch_target(&launch_alias)?;
    let mut command = Command::new("ditto");
    command.arg(&receipt.bundle_path);
    command.arg(&launch_alias);
    run_command(&mut command).with_context(|| {
        format!(
            "failed to materialize macOS trace launch bundle {} from {}",
            launch_alias.display(),
            receipt.bundle_path.display()
        )
    })?;
    // `xctrace --launch -- <bundle.app>` spawns an extra Dock-visible app instance on macOS.
    // Launching a uniquely named executable inside a copied bundle avoids that duplicate process,
    // but still gives Orbit a stable, unambiguous target for tracing.
    let launch_executable_name = macos_trace_launch_executable_name(
        launch_alias
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(receipt.target.as_str()),
    );
    let launch_executable =
        rewrite_macos_trace_launch_bundle_executable(&launch_alias, &launch_executable_name)?;
    let mut codesign = Command::new("codesign");
    // The copied trace launch target must stay ad-hoc signed without extra entitlements.
    // Adding `get-task-allow` here makes `xctrace --launch` spawn a duplicate Dock-visible
    // macOS app process for this copied bundle in live validation.
    codesign.args(["--force", "--sign", "-"]);
    codesign.arg(&launch_alias);
    run_command(&mut codesign).with_context(|| {
        format!(
            "failed to re-sign macOS trace launch bundle {}",
            launch_alias.display()
        )
    })?;
    launch_executable
        .to_str()
        .map(ToOwned::to_owned)
        .context("macOS trace launch executable path contains invalid UTF-8")
}

fn remove_existing_trace_launch_target(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path).with_context(|| format!("failed to replace {}", path.display()))?;
        return Ok(());
    }

    if metadata.is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        return Ok(());
    }

    bail!(
        "unsupported existing macOS trace launch target {}",
        path.display()
    )
}

fn rewrite_macos_trace_launch_bundle_executable(
    bundle_path: &Path,
    executable_name: &str,
) -> Result<PathBuf> {
    let info_plist_path = bundle_path.join("Contents").join("Info.plist");
    let mut info_plist = Value::from_file(&info_plist_path)
        .with_context(|| format!("failed to parse {}", info_plist_path.display()))?
        .into_dictionary()
        .context("Info.plist must contain a top-level dictionary")?;
    let original_executable = info_plist
        .get("CFBundleExecutable")
        .and_then(Value::as_string)
        .context("Info.plist is missing `CFBundleExecutable`")?;
    let macos_dir = bundle_path.join("Contents").join("MacOS");
    let original_executable_path = macos_dir.join(original_executable);
    let renamed_executable_path = macos_dir.join(executable_name);
    fs::rename(&original_executable_path, &renamed_executable_path).with_context(|| {
        format!(
            "failed to rename macOS trace launch executable {} to {}",
            original_executable_path.display(),
            renamed_executable_path.display()
        )
    })?;
    info_plist.insert(
        "CFBundleExecutable".to_owned(),
        Value::String(executable_name.to_owned()),
    );
    Value::Dictionary(info_plist)
        .to_file_xml(&info_plist_path)
        .with_context(|| format!("failed to write {}", info_plist_path.display()))?;
    Ok(renamed_executable_path)
}

fn macos_trace_launch_executable_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' => character,
            _ => '_',
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "OrbitTraceLaunch".to_owned()
    } else {
        format!("OrbitTrace{trimmed}")
    }
}

fn launch_device_console_process(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    bundle_id_or_path: &str,
    process_name: &str,
    verbose: bool,
) -> Result<()> {
    let (mut relay, debug) = spawn_device_console_launch_session(
        selected_xcode,
        device,
        bundle_id_or_path,
        process_name,
        verbose,
    )?;
    wait_for_device_console_process(&mut relay, &debug)
}

fn spawn_device_console_launch_session(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    bundle_id_or_path: &str,
    process_name: &str,
    verbose: bool,
) -> Result<(DeviceConsoleRelay, String)> {
    let mut launch = xcrun_command(selected_xcode);
    launch.args([
        "devicectl",
        "device",
        "process",
        "launch",
        "--console",
        "--terminate-existing",
        "--device",
        device.provisioning_udid(),
    ]);
    apply_device_console_environment(&mut launch);
    launch.arg(bundle_id_or_path);
    let debug = debug_command(&launch);
    let relay = DeviceConsoleRelay::start(&mut launch, process_name, verbose)
        .with_context(|| format!("failed to execute `{debug}`"))?;
    Ok((relay, debug))
}

fn wait_for_device_console_process(relay: &mut DeviceConsoleRelay, debug: &str) -> Result<()> {
    let status = relay.wait()?;
    if !status.success() {
        bail!("`{debug}` failed with {status}");
    }
    Ok(())
}

fn spawn_ios_debug_launch_session(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    bundle_id: &str,
    process_name: &str,
    verbose: bool,
) -> Result<DeviceConsoleRelay> {
    let mut launch = xcrun_command(selected_xcode);
    launch.args([
        "devicectl",
        "device",
        "process",
        "launch",
        "--console",
        "--start-stopped",
        "--terminate-existing",
        "--device",
        device.provisioning_udid(),
    ]);
    apply_device_console_environment(&mut launch);
    launch.arg(bundle_id);
    let debug = crate::util::debug_command(&launch);
    DeviceConsoleRelay::start_without_stdin(&mut launch, process_name, verbose)
        .with_context(|| format!("failed to execute `{debug}`"))
}

fn prepare_simulator_installation(
    project: &ProjectContext,
    receipt: &BuildReceipt,
) -> Result<SimulatorDevice> {
    let device = select_simulator_device(project, receipt.platform)?;
    if !device.is_booted() {
        let mut boot = xcrun_command(project.selected_xcode.as_ref());
        boot.args(["simctl", "boot", &device.udid]);
        run_command(&mut boot)?;
    }

    let mut bootstatus = xcrun_command(project.selected_xcode.as_ref());
    bootstatus.args(["simctl", "bootstatus", &device.udid, "-b"]);
    run_command(&mut bootstatus)?;

    let mut open_simulator = open_simulator_command(project.selected_xcode.as_ref(), &device.udid);
    run_command(&mut open_simulator)?;

    let mut install = xcrun_command(project.selected_xcode.as_ref());
    install.args([
        "simctl",
        "install",
        &device.udid,
        receipt
            .bundle_path
            .to_str()
            .context("bundle path contains invalid UTF-8")?,
    ]);
    run_command(&mut install)?;

    Ok(device)
}

fn install_on_device(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
) -> Result<InstalledApplication> {
    let output_path = NamedTempFile::new()?;
    let mut install = xcrun_command(selected_xcode);
    install.args([
        "devicectl",
        "device",
        "install",
        "app",
        "--device",
        device.provisioning_udid(),
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
        receipt
            .bundle_path
            .to_str()
            .context("bundle path contains invalid UTF-8")?,
    ]);
    run_command(&mut install)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let installed: DeviceCtlEnvelope<InstalledApplicationsResult> = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device install app` output")?;
    installed
        .result
        .installed_applications
        .into_iter()
        .next()
        .context("`devicectl device install app` did not report an installed application")
}

fn remote_app_bundle_path(installation_url: &str) -> Result<String> {
    let path = installation_url
        .strip_prefix("file://")
        .unwrap_or(installation_url)
        .trim_end_matches('/');
    if path.is_empty() {
        bail!(
            "installed application URL `{installation_url}` did not include a remote bundle path"
        );
    }
    Ok(path.to_owned())
}

fn launch_device_app(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    remote_bundle_path: &str,
    start_stopped: bool,
) -> Result<DeviceLaunchedProcess> {
    let output_path = NamedTempFile::new()?;
    let mut launch = xcrun_command(selected_xcode);
    launch.args(["devicectl", "device", "process", "launch"]);
    if start_stopped {
        launch.arg("--start-stopped");
    }
    launch.args([
        "--terminate-existing",
        "--device",
        device.provisioning_udid(),
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
    ]);
    apply_device_console_environment(&mut launch);
    launch.arg(remote_bundle_path);
    run_command(&mut launch)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let launched: DeviceCtlEnvelope<LaunchedProcessResult> = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device process launch` output")?;
    Ok(launched.result.process)
}

fn list_device_processes(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
) -> Result<Vec<DeviceRunningProcess>> {
    let output_path = NamedTempFile::new()?;
    let mut command = xcrun_command(selected_xcode);
    command.args([
        "devicectl",
        "device",
        "info",
        "processes",
        "--device",
        device.provisioning_udid(),
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
    ]);
    let debug = debug_command(&command);
    let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
    if !success {
        bail!("`{debug}` failed\nstdout:\n{}\nstderr:\n{}", stdout, stderr);
    }
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let processes: DeviceCtlEnvelope<RunningProcessesResult> = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device info processes` output")?;
    Ok(processes.result.running_processes)
}

fn apply_device_console_environment(command: &mut Command) {
    // Xcode launches apps in a developer-tools logging mode so os_log/Logger messages
    // are mirrored into the attached debug console. Mirror that behavior for device runs.
    command.args([
        "--environment-variables",
        r#"{"OS_ACTIVITY_DT_MODE":"1","IDEPreferLogStreaming":"YES"}"#,
    ]);
}

fn wait_for_device_process_for_installation(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    installation_url: &str,
    timeout: Duration,
    mut launch_child: Option<&mut DeviceConsoleRelay>,
) -> Result<DeviceRunningProcess> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(process) = find_running_process_for_installation(
            &list_device_processes(selected_xcode, device)?,
            installation_url,
        ) {
            return Ok(process.clone());
        }

        if let Some(child) = launch_child.as_deref_mut()
            && let Some(status) = child.try_wait()?
            && !status.success()
        {
            if let Some(signal) = status.signal() {
                bail!(
                    "`devicectl device process launch --console --start-stopped` exited from signal {signal} before Orbit could attach LLDB"
                );
            }
            bail!(
                "`devicectl device process launch --console --start-stopped` exited with {status} before Orbit could attach LLDB"
            );
        }

        thread::sleep(Duration::from_millis(250));
    }

    bail!(
        "failed to identify the launched `{}` process on device {} ({})",
        bundle_name_from_installation_url(installation_url),
        device.name(),
        device.provisioning_udid()
    )
}

fn find_running_process_for_installation<'a>(
    processes: &'a [DeviceRunningProcess],
    installation_url: &str,
) -> Option<&'a DeviceRunningProcess> {
    processes.iter().find(|process| {
        process
            .executable
            .as_deref()
            .is_some_and(|executable| executable.starts_with(installation_url))
    })
}

fn bundle_name_from_installation_url(installation_url: &str) -> String {
    installation_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(installation_url)
        .trim_end_matches(".app")
        .to_owned()
}

fn run_lldb_device_attach_session(
    device: &PhysicalDevice,
    executable: &Path,
    process_identifier: u64,
    symbol_root: Option<&Path>,
) -> Result<()> {
    let script = NamedTempFile::new()?;
    fs::write(
        script.path(),
        lldb_expect_attach_script(symbol_root)?.as_bytes(),
    )
    .with_context(|| format!("failed to write {}", script.path().display()))?;

    let mut command = Command::new("expect");
    command.arg("-f").arg(script.path());
    command.arg(device.provisioning_udid());
    command.arg(process_identifier.to_string());
    command.arg(executable);
    let debug = debug_command(&command);
    let status = command
        .status()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    if status.success() || status.code() == Some(130) {
        return Ok(());
    }
    bail!("`{debug}` failed with {status}")
}

fn run_lldb_simulator_attach_session(
    selected_xcode: Option<&SelectedXcode>,
    executable: &Path,
    process_name: &str,
) -> Result<()> {
    let script = NamedTempFile::new()?;
    fs::write(
        script.path(),
        lldb_expect_simulator_attach_script(selected_xcode)?.as_bytes(),
    )
    .with_context(|| format!("failed to write {}", script.path().display()))?;

    let mut command = Command::new("expect");
    command.arg("-f").arg(script.path());
    command.arg(executable);
    command.arg(process_name);
    let debug = debug_command(&command);
    let status = command
        .status()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    if status.success() || status.code() == Some(130) {
        return Ok(());
    }
    bail!("`{debug}` failed with {status}")
}

fn run_macos_debug_session(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    executable: &Path,
    lldb_script: &Path,
    log_pipe: &Path,
) -> Result<()> {
    let executable = executable
        .to_str()
        .context("macOS executable path contains invalid UTF-8")?;
    let lldb_script = lldb_script
        .to_str()
        .context("macOS LLDB script path contains invalid UTF-8")?;
    let log_pipe = log_pipe
        .to_str()
        .context("macOS log pipe path contains invalid UTF-8")?;

    let script = write_temp_script(
        &macos_debug_wrapper_script(executable, lldb_script, log_pipe, &receipt.bundle_id)?,
        "macOS debug wrapper",
        true,
    )?;

    if project.app.interactive {
        return run_macos_wrapper_session(script.path());
    }

    run_inherited_shell_script(script.path())
}

fn run_macos_wrapper_session(wrapper_script: &Path) -> Result<()> {
    let script = write_temp_script(
        &expect_macos_wrapper_script()?,
        "macOS wrapper coordinator",
        false,
    )?;

    let mut command = Command::new("expect");
    command.arg("-f").arg(script.path());
    command.arg(wrapper_script);
    let debug = debug_command(&command);
    let status = command
        .status()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    if status.success() || status.code() == Some(130) {
        return Ok(());
    }
    bail!("`{debug}` failed with {status}");
}

fn create_macos_inferior_log_relay(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    pipe_path: &Path,
) -> Result<(MacosInferiorLogRelay, fs::File)> {
    let mut mkfifo = Command::new("mkfifo");
    mkfifo.arg(pipe_path);
    run_command(&mut mkfifo)?;

    let anchor = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_path)
        .with_context(|| {
            format!(
                "failed to open macOS run log pipe `{}`",
                pipe_path.display()
            )
        })?;

    // Return the relay before the anchor so the local bindings can be declared
    // in `(relay, anchor)` order. Rust drops locals in reverse declaration
    // order, which closes the FIFO anchor before joining the reader thread and
    // guarantees EOF reaches the relay on shutdown.
    Ok((
        MacosInferiorLogRelay::start(pipe_path, &receipt.bundle_id, project.app.verbose),
        anchor,
    ))
}

fn expect_macos_wrapper_script() -> Result<String> {
    Ok(r"set timeout -1
set wrapper [lindex $argv 0]

spawn -noecho /bin/zsh $wrapper
interact {
    \003 {
        catch {exec /bin/kill -TERM [exp_pid]}
        catch {exec /usr/bin/pkill -TERM -P [exp_pid]}
        expect {
            eof { exit 130 }
            timeout {
                catch {exec /usr/bin/pkill -KILL -P [exp_pid]}
                catch {exec /bin/kill -KILL [exp_pid]}
                exit 130
            }
        }
        exit 130
    }
}
"
    .to_owned())
}

fn macos_debug_wrapper_script(
    executable: &str,
    lldb_script: &str,
    log_pipe: &str,
    bundle_id: &str,
) -> Result<String> {
    let orbit_pid = std::process::id();
    Ok(format!(
        r#"#!/bin/zsh
set -uo pipefail

cleanup() {{
  local exit_code="${{1:-0}}"
  trap - INT TERM HUP EXIT

  if [[ -n "${{guardian_pid:-}}" ]]; then
    kill -TERM "${{guardian_pid}}" >/dev/null 2>&1 || true
  fi
  /usr/bin/pkill -TERM -P $$ >/dev/null 2>&1 || true
  /usr/bin/osascript -e {quit_script} >/dev/null 2>&1 || true
  for _ in {{1..20}}; do
    if ! /usr/bin/pgrep -f {executable} >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
  done
  /usr/bin/pkill -f {executable} >/dev/null 2>&1 || true
  /usr/bin/pkill -KILL -f {executable} >/dev/null 2>&1 || true
  exit "${{exit_code}}"
}}

trap 'cleanup 130' INT
trap 'cleanup 143' TERM HUP
trap 'cleanup $?' EXIT

/usr/bin/setsid /bin/zsh -c '
parent_pid="$1"
executable="$2"
quit_script="$3"
while /bin/kill -0 "$parent_pid" >/dev/null 2>&1; do
  sleep 0.5
done
/usr/bin/osascript -e "$quit_script" >/dev/null 2>&1 || true
/usr/bin/pkill -TERM -f "$executable" >/dev/null 2>&1 || true
sleep 0.5
/usr/bin/pkill -KILL -f "$executable" >/dev/null 2>&1 || true
' _ {orbit_pid} {executable} {quit_script} >/dev/null 2>&1 &
guardian_pid=$!

/usr/bin/expect -f {lldb_script} {executable} {log_pipe}
launcher_status=$?
cleanup "${{launcher_status}}"
"#,
        executable = shell_quote_arg(executable),
        lldb_script = shell_quote_arg(lldb_script),
        log_pipe = shell_quote_arg(log_pipe),
        orbit_pid = orbit_pid,
        quit_script = shell_quote_arg(&macos_quit_applescript(bundle_id)),
    ))
}

fn lldb_expect_macos_run_script(selected_xcode: Option<&SelectedXcode>) -> Result<String> {
    let lldb_path = selected_xcode_lldb_path(selected_xcode)?;
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

proc kill_target {{exe}} {{
    catch {{exec /usr/bin/pkill -f -- $exe}}
}}

set exe [lindex $argv 0]
set log_pipe [lindex $argv 1]
set lldb_path "{lldb_path}"

spawn $lldb_path $exe
wait_for_prompt
send -- "settings set target.env-vars {env_vars}\r"
wait_for_prompt
send -- "process launch -s -o $log_pipe -e $log_pipe\r"
wait_for_message {{Process [0-9]+ launched}} "timed out waiting for LLDB to launch the macOS app"
wait_for_prompt
send -- "continue\r"
wait_for_message {{Process [0-9]+ resuming}} "timed out waiting for LLDB to continue the macOS app"
expect {{
    -re {{Process [0-9]+ exited}} {{}}
    -re {{Process [0-9]+ stopped}} {{}}
    -re {{\(lldb\)}} {{}}
    eof {{ exit 0 }}
}}
send -- "quit\r"
expect {{
    -re {{Do you really want to proceed: \[Y/n\]}} {{
        send -- "Y\r"
        expect eof
        exit 0
    }}
    eof {{ exit 0 }}
}}
"#,
        lldb_path = tcl_quote_arg(
            lldb_path
                .to_str()
                .context("macOS LLDB path contains invalid UTF-8")?,
        ),
        env_vars = tcl_quote_arg(&macos_xcode_log_redirect_env(selected_xcode)?),
    ))
}

fn lldb_expect_macos_launch_script(selected_xcode: Option<&SelectedXcode>) -> Result<String> {
    let lldb_path = selected_xcode_lldb_path(selected_xcode)?;
    Ok(format!(
        r#"set timeout 60

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

proc kill_target {{exe}} {{
    catch {{exec /usr/bin/pkill -f -- $exe}}
}}

set exe [lindex $argv 0]
set log_pipe [lindex $argv 1]
set lldb_path "{lldb_path}"

spawn $lldb_path $exe
wait_for_prompt
send -- "settings set target.env-vars {env_vars}\r"
wait_for_prompt
send -- "process launch -s -o $log_pipe -e $log_pipe\r"
wait_for_message {{Process [0-9]+ launched}} "timed out waiting for LLDB to launch the macOS app"
wait_for_prompt
send -- "continue\r"
wait_for_message {{Process [0-9]+ resuming}} "timed out waiting for LLDB to continue the macOS app"
wait_for_prompt
interact {{
    \003 {{
        send -- "quit\r"
        expect {{
            -re {{Do you really want to proceed: \[Y/n\]}} {{
                send -- "Y\r"
                expect {{
                    eof {{}}
                    timeout {{
                        kill_target $exe
                        send_user "timed out waiting for LLDB to acknowledge quit after Ctrl-C\n"
                        exit 130
                    }}
                }}
            }}
            eof {{}}
            timeout {{
                kill_target $exe
                send_user "timed out waiting for LLDB to acknowledge quit after Ctrl-C\n"
                exit 130
            }}
        }}
        kill_target $exe
        exit 130
    }}
}}
"#,
        lldb_path = tcl_quote_arg(
            lldb_path
                .to_str()
                .context("macOS LLDB path contains invalid UTF-8")?,
        ),
        env_vars = tcl_quote_arg(&macos_xcode_log_redirect_env(selected_xcode)?),
    ))
}

fn lldb_expect_attach_script(symbol_root: Option<&Path>) -> Result<String> {
    let expect_symbol_root = symbol_root
        .map(|path| {
            path.to_str()
                .context("device symbol cache path contains invalid UTF-8")
                .map(tcl_quote_arg)
        })
        .transpose()?
        .unwrap_or_default();
    let symbol_setup = if expect_symbol_root.is_empty() {
        String::new()
    } else {
        format!(
            r#"send -- "settings append target.exec-search-paths \"{symbol_root}\"\r"
wait_for_prompt
send -- "settings append target.debug-file-search-paths \"{symbol_root}\"\r"
wait_for_prompt
"#,
            symbol_root = expect_symbol_root
        )
    };
    Ok(format!(
        r#"set timeout 60

proc wait_for_prompt {{}} {{
    expect {{
        -re {{\(lldb\)}} {{ return }}
        timeout {{ send_user "timed out waiting for LLDB prompt\n"; exit 1 }}
        eof {{ send_user "LLDB exited before it became interactive\n"; exit 1 }}
    }}
}}

proc wait_for_log {{pattern message}} {{
    expect {{
        -re $pattern {{ return }}
        timeout {{ send_user "$message\n"; exit 1 }}
        eof {{ send_user "LLDB exited unexpectedly\n"; exit 1 }}
    }}
}}

proc wait_for_stop_or_prompt {{pid message}} {{
    expect {{
        -re [format {{Process %s stopped}} $pid] {{ return }}
        -re {{\(lldb\)}} {{ return }}
        timeout {{ send_user "$message\n"; exit 130 }}
        eof {{ exit 130 }}
    }}
}}

set udid [lindex $argv 0]
set pid [lindex $argv 1]
set exe [lindex $argv 2]

spawn lldb $exe
wait_for_prompt
{symbol_setup}send -- "device select $udid\r"
wait_for_prompt
send -- "device process attach --pid $pid\r"
wait_for_log [format {{Process %s stopped}} $pid] [format {{timed out waiting for LLDB to attach to pid %s}} $pid]
wait_for_prompt
send -- "process continue\r"
wait_for_log [format {{Process %s resuming}} $pid] [format {{timed out waiting for LLDB to resume pid %s}} $pid]
wait_for_prompt
interact {{
    \003 {{
        send -- "\003"
        wait_for_stop_or_prompt $pid [format {{timed out waiting for LLDB to interrupt pid %s after Ctrl-C}} $pid]
        send -- "process kill\r"
        expect {{
            -re {{\(lldb\)}} {{}}
            timeout {{ send_user [format {{timed out waiting for LLDB to kill pid %s after Ctrl-C\n}} $pid]; exit 130 }}
            eof {{ exit 130 }}
        }}
        send -- "quit\r"
        expect {{
            -re {{Do you really want to proceed: \[Y/n\]}} {{
                send -- "Y\r"
                expect {{
                    eof {{}}
                    timeout {{ exit 130 }}
                }}
            }}
            eof {{}}
            timeout {{ exit 130 }}
        }}
        exit 130
    }}
}}
"#,
        symbol_setup = symbol_setup
    ))
}

fn lldb_expect_simulator_attach_script(selected_xcode: Option<&SelectedXcode>) -> Result<String> {
    let lldb_path = selected_xcode_lldb_path(selected_xcode)?;
    Ok(format!(
        r#"set timeout 60
log_user 0

proc wait_for_prompt {{}} {{
    expect {{
        -re {{\(lldb\)}} {{ return }}
        timeout {{ send_user "timed out waiting for LLDB prompt\n"; exit 1 }}
        eof {{ send_user "LLDB exited before it became interactive\n"; exit 1 }}
    }}
}}

proc wait_for_log {{pattern message}} {{
    expect {{
        -re $pattern {{ return }}
        timeout {{ send_user "$message\n"; exit 1 }}
        eof {{ send_user "LLDB exited unexpectedly\n"; exit 1 }}
    }}
}}

proc wait_for_stop_or_prompt {{message}} {{
    expect {{
        -re {{Process [0-9]+ stopped}} {{ return }}
        -re {{\(lldb\)}} {{ return }}
        timeout {{ send_user "$message\n"; exit 130 }}
        eof {{ exit 130 }}
    }}
}}

set exe [lindex $argv 0]
set process_name [lindex $argv 1]
set lldb_path "{lldb_path}"

spawn -noecho $lldb_path $exe
wait_for_prompt
send -- "settings set interpreter.echo-commands false\r"
wait_for_prompt
send -- "settings set show-progress false\r"
wait_for_prompt
send -- "settings set show-statusline false\r"
wait_for_prompt
send -- "settings set use-color false\r"
wait_for_prompt
send -- "process attach -i -w -n $process_name\r"
wait_for_log {{Process [0-9]+ stopped}} [format {{timed out waiting for LLDB to attach to process %s}} $process_name]
wait_for_prompt
send -- "process continue\r"
wait_for_log {{Process [0-9]+ resuming}} [format {{timed out waiting for LLDB to resume process %s}} $process_name]
wait_for_prompt
log_user 1
send_user "(lldb) "
interact {{
    \003 {{
        log_user 0
        send -- "\003"
        wait_for_stop_or_prompt [format {{timed out waiting for LLDB to interrupt process %s after Ctrl-C}} $process_name]
        send -- "process kill\r"
        expect {{
            -re {{\(lldb\)}} {{}}
            timeout {{ send_user [format {{timed out waiting for LLDB to kill process %s after Ctrl-C\n}} $process_name]; exit 130 }}
            eof {{ exit 130 }}
        }}
        send -- "quit\r"
        expect {{
            -re {{Do you really want to proceed: \[Y/n\]}} {{
                send -- "Y\r"
                expect {{
                    eof {{}}
                    timeout {{ exit 130 }}
                }}
            }}
            eof {{}}
            timeout {{ exit 130 }}
        }}
        exit 130
    }}
}}
"#,
        lldb_path = tcl_quote_arg(
            lldb_path
                .to_str()
                .context("simulator LLDB path contains invalid UTF-8")?,
        ),
    ))
}

fn simulator_run_wrapper_script(
    selected_xcode: Option<&SelectedXcode>,
    device: &SimulatorDevice,
    receipt: &BuildReceipt,
) -> Result<String> {
    let developer_dir_export = match selected_xcode {
        Some(selected_xcode) => format!(
            "export DEVELOPER_DIR={}\n",
            shell_quote_arg(
                selected_xcode
                    .developer_dir
                    .to_str()
                    .context("selected Xcode developer dir contains invalid UTF-8")?
            )
        ),
        None => String::new(),
    };

    Ok(format!(
        r#"#!/bin/zsh
set -uo pipefail
{developer_dir_export}cleanup() {{
  local exit_code="${{1:-0}}"
  trap - INT TERM HUP EXIT

  /usr/bin/xcrun simctl terminate {udid} {bundle_id} >/dev/null 2>&1 || true
  if [[ -n "${{launcher_pid:-}}" ]]; then
    /bin/kill -TERM "${{launcher_pid}}" >/dev/null 2>&1 || true
    wait "${{launcher_pid}}" 2>/dev/null || true
  fi

  exit "${{exit_code}}"
}}

trap 'cleanup 0' INT
trap 'cleanup 0' TERM HUP
trap 'cleanup $?' EXIT

/usr/bin/xcrun simctl launch --console-pty --terminate-running-process {udid} {bundle_id} >/dev/null 2>&1 &
launcher_pid=$!
wait "${{launcher_pid}}"
launcher_status=$?
cleanup "${{launcher_status}}"
"#,
        developer_dir_export = developer_dir_export,
        udid = shell_quote_arg(&device.udid),
        bundle_id = shell_quote_arg(&receipt.bundle_id),
    ))
}

fn bundle_debug_executable_path(receipt: &BuildReceipt) -> Result<PathBuf> {
    let path = receipt.bundle_path.join(&receipt.target);
    if path.exists() {
        return Ok(path);
    }
    macos_executable_path(receipt)
}

fn simulator_process_name(receipt: &BuildReceipt) -> &str {
    receipt.target.as_str()
}

fn start_simulator_app_logs(
    project: &ProjectContext,
    device: &SimulatorDevice,
    receipt: &BuildReceipt,
) -> Option<SimulatorAppLogStream> {
    match SimulatorAppLogStream::start(
        project.selected_xcode.as_ref(),
        &device.udid,
        simulator_process_name(receipt),
        &receipt.bundle_id,
        project.app.verbose,
    ) {
        Ok(stream) => Some(stream),
        Err(error) => {
            eprintln!(
                "warning: failed to start app logs for `{}` on {}: {error:#}",
                simulator_process_name(receipt),
                device.name
            );
            None
        }
    }
}

fn select_simulator_device(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<SimulatorDevice> {
    crate::apple::simulator::select_simulator_device(project, platform)
}

fn list_devicectl_devices(
    selected_xcode: Option<&SelectedXcode>,
    platform: ApplePlatform,
) -> Result<Vec<PhysicalDevice>> {
    let output_path = NamedTempFile::new()?;
    let mut list = xcrun_command(selected_xcode);
    list.args([
        "devicectl",
        "list",
        "devices",
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
    ]);
    run_command(&mut list)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let devices: DeviceCtlEnvelope<DeviceListResult> = serde_json::from_str(&contents)?;
    Ok(devices
        .result
        .devices
        .into_iter()
        .filter(|device| physical_device_matches_platform(device, platform))
        .collect())
}

fn ensure_device_symbols_available(
    project: &ProjectContext,
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<Option<PathBuf>> {
    let symbol_root = resolve_device_symbol_root(project, device, platform);
    if device_symbol_root_ready(&symbol_root) {
        return Ok(Some(symbol_root));
    }

    ensure_device_is_unlocked_for_symbol_download(
        project.selected_xcode.as_ref(),
        device,
        platform,
    )?;

    let spinner = CliSpinner::new(format!("Caching device symbols for {platform}"));
    match prepare_device_support_symbols(project.selected_xcode.as_ref(), device, platform) {
        Ok(()) => {
            let symbol_root = resolve_device_symbol_root(project, device, platform);
            if device_symbol_root_ready(&symbol_root) {
                spinner.finish_success(format!("Prepared device symbols for {platform}."));
                Ok(Some(symbol_root))
            } else {
                spinner.finish_warning(format!(
                    "Orbit prepared device support for {platform}, but no usable symbol root was found. LLDB will fall back to reading symbols from the device."
                ));
                Ok(None)
            }
        }
        Err(error) => {
            if error_mentions_locked_device(&error.to_string()) {
                spinner.finish_clear();
                return Err(error);
            }
            spinner.finish_warning(format!(
                "Orbit could not cache device symbols for {platform}: {error}. LLDB will fall back to reading symbols from the device."
            ));
            Ok(None)
        }
    }
}

fn prepare_device_support_symbols(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<()> {
    let os_version = device
        .device_properties
        .os_version_number
        .as_deref()
        .context("device is missing an OS version in `devicectl list devices` output")?;
    let model_code = device
        .hardware_properties
        .product_type
        .as_deref()
        .context("device is missing a product type in `devicectl list devices` output")?;

    let mut command = xcodebuild_command(selected_xcode);
    command.args([
        "-prepareDeviceSupport",
        "-platform",
        devicectl_platform_name(platform),
        "-osVersion",
        os_version,
        "-modelCode",
        model_code,
        "-architecture",
        &device.hardware_properties.cpu_type.name,
    ]);
    let debug = crate::util::debug_command(&command);
    let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
    let output = combine_command_output(&stdout, &stderr);
    if error_mentions_locked_device(&output) {
        bail!(locked_device_symbol_download_message(device));
    }
    if !success {
        bail!("`{debug}` failed\nstdout:\n{}\nstderr:\n{}", stdout, stderr);
    }

    Ok(())
}

fn ensure_device_is_unlocked_for_symbol_download(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<()> {
    ensure_device_is_unlocked(
        selected_xcode,
        device,
        platform,
        locked_device_symbol_download_message(device),
    )
}

fn ensure_device_is_unlocked_for_debugging(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<()> {
    ensure_device_is_unlocked(
        selected_xcode,
        device,
        platform,
        locked_device_debug_message(device),
    )
}

fn ensure_device_is_unlocked(
    selected_xcode: Option<&SelectedXcode>,
    device: &PhysicalDevice,
    platform: ApplePlatform,
    failure_message: String,
) -> Result<()> {
    if platform == ApplePlatform::Macos {
        return Ok(());
    }

    let output_path = NamedTempFile::new()?;
    let mut command = xcrun_command(selected_xcode);
    command.args([
        "devicectl",
        "device",
        "info",
        "lockState",
        "--device",
        device.provisioning_udid(),
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
    ]);
    let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
    let output = combine_command_output(&stdout, &stderr);
    if error_mentions_locked_device(&output) {
        bail!(failure_message);
    }
    if !success || !output_path.path().exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let details: serde_json::Value = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device info lockState` output")?;
    if device_is_locked_from_details(&details).unwrap_or(false) {
        bail!(failure_message);
    }

    Ok(())
}

fn resolve_device_symbol_root(
    project: &ProjectContext,
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> PathBuf {
    let support_root = device_support_root(project, platform);
    let candidates = device_support_label_candidates(device)
        .into_iter()
        .map(|label| support_root.join(label).join("Symbols"))
        .collect::<Vec<_>>();
    candidates
        .iter()
        .find(|candidate| device_symbol_root_ready(candidate))
        .cloned()
        .unwrap_or_else(|| {
            candidates.into_iter().next().unwrap_or_else(|| {
                support_root
                    .join(format!(
                        "Orbit {}",
                        sanitize_device_support_component(device.provisioning_udid())
                    ))
                    .join("Symbols")
            })
        })
}

fn device_support_label_from_device(device: &PhysicalDevice) -> Option<String> {
    match (
        device.device_properties.os_version_number.as_deref(),
        device.device_properties.os_build_update.as_deref(),
    ) {
        (Some(version), Some(build)) if version != build => Some(format!("{version} ({build})")),
        (Some(version), _) => Some(version.to_owned()),
        (_, Some(build)) => Some(build.to_owned()),
        _ => None,
    }
}

fn device_support_root(project: &ProjectContext, platform: ApplePlatform) -> PathBuf {
    dirs::home_dir()
        .map(|home| {
            home.join("Library")
                .join("Developer")
                .join("Xcode")
                .join(device_support_directory(platform))
        })
        .unwrap_or_else(|| {
            project
                .app
                .global_paths
                .cache_dir
                .join("device-support")
                .join(platform.to_string())
        })
}

fn device_support_label_candidates(device: &PhysicalDevice) -> Vec<String> {
    let mut labels = Vec::new();
    if let Some(label) = device_support_model_label_from_device(device) {
        labels.push(label);
    }
    if let Some(label) = device_support_label_from_device(device) {
        labels.push(label);
    }
    if labels.is_empty() {
        labels.push(format!(
            "Orbit {}",
            sanitize_device_support_component(device.provisioning_udid())
        ));
    }
    labels
}

fn device_support_model_label_from_device(device: &PhysicalDevice) -> Option<String> {
    let model = device.hardware_properties.product_type.as_deref()?;
    let base = device_support_label_from_device(device)?;
    Some(format!("{model} {base}"))
}

fn json_value_label(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Array(values) => values.iter().find_map(json_value_label),
        serde_json::Value::Object(map) => {
            let major = map.get("major").and_then(serde_json::Value::as_u64);
            let minor = map.get("minor").and_then(serde_json::Value::as_u64);
            if let (Some(major), Some(minor)) = (major, minor) {
                let patch = map.get("patch").and_then(serde_json::Value::as_u64);
                return Some(match patch {
                    Some(patch) => format!("{major}.{minor}.{patch}"),
                    None => format!("{major}.{minor}"),
                });
            }

            for key in [
                "description",
                "stringValue",
                "value",
                "buildVersion",
                "productBuildVersion",
                "build",
                "trainName",
                "name",
            ] {
                if let Some(label) = map.get(key).and_then(json_value_label) {
                    return Some(label);
                }
            }

            map.values().find_map(json_value_label)
        }
        serde_json::Value::Bool(_) | serde_json::Value::Null => None,
    }
}

fn device_is_locked_from_details(details: &serde_json::Value) -> Option<bool> {
    match details {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_ascii_lowercase();
                if matches!(key.as_str(), "passcoderequired" | "ispasscoderequired")
                    && let Some(value) = value.as_bool()
                {
                    return Some(value);
                }
                if matches!(key.as_str(), "islocked" | "locked") {
                    if let Some(value) = value.as_bool() {
                        return Some(value);
                    }
                    if let Some(value) = json_value_label(value)
                        .as_deref()
                        .and_then(parse_lock_state_label)
                    {
                        return Some(value);
                    }
                }
                if key.contains("lockstate")
                    && let Some(value) = parse_lock_state_value(value)
                {
                    return Some(value);
                }
            }

            map.values().find_map(device_is_locked_from_details)
        }
        serde_json::Value::Array(values) => values.iter().find_map(device_is_locked_from_details),
        _ => None,
    }
}

fn parse_lock_state_value(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::String(value) => parse_lock_state_label(value),
        serde_json::Value::Object(map) => {
            for key in ["name", "description", "stringValue", "value"] {
                if let Some(value) = map.get(key).and_then(json_value_label)
                    && let Some(value) = parse_lock_state_label(&value)
                {
                    return Some(value);
                }
            }
            map.values().find_map(parse_lock_state_value)
        }
        serde_json::Value::Array(values) => values.iter().find_map(parse_lock_state_value),
        serde_json::Value::Number(_) | serde_json::Value::Null => None,
    }
}

fn parse_lock_state_label(value: &str) -> Option<bool> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("unlocked") {
        return Some(false);
    }
    if normalized.contains("locked") {
        return Some(true);
    }
    None
}

fn device_support_directory(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Ios => "iOS DeviceSupport",
        ApplePlatform::Macos => "macOS DeviceSupport",
        ApplePlatform::Tvos => "tvOS DeviceSupport",
        ApplePlatform::Visionos => "visionOS DeviceSupport",
        ApplePlatform::Watchos => "watchOS DeviceSupport",
    }
}

fn sanitize_device_support_component(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect()
}

fn device_symbol_cache_dir(symbol_root: &Path) -> PathBuf {
    symbol_root
        .join("System")
        .join("Library")
        .join("Caches")
        .join("com.apple.dyld")
}

fn device_symbol_root_ready(symbol_root: &Path) -> bool {
    if symbol_root.join("usr").join("lib").join("dyld").exists() {
        return true;
    }
    count_device_symbol_cache_files(symbol_root) > 0
}

fn count_device_symbol_cache_files(symbol_root: &Path) -> usize {
    let cache_dir = device_symbol_cache_dir(symbol_root);
    cache_dir
        .read_dir()
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("dyld_shared_cache_"))
        })
        .count()
}

fn error_mentions_locked_device(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("device is locked")
        || normalized.contains("device needs to be unlocked")
        || normalized.contains("unlock the device and try again")
        || normalized.contains("operation failed since the device is locked")
}

fn locked_device_symbol_download_message(device: &PhysicalDevice) -> String {
    format!(
        "device symbol download requires an unlocked device. Unlock {} ({}) and try again.",
        device.name(),
        device.provisioning_udid()
    )
}

fn locked_device_debug_message(device: &PhysicalDevice) -> String {
    format!(
        "device debugging requires an unlocked device. Unlock {} ({}) and try again.",
        device.name(),
        device.provisioning_udid()
    )
}

fn devicectl_platform_name(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Ios => "iOS",
        ApplePlatform::Macos => "macOS",
        ApplePlatform::Tvos => "tvOS",
        ApplePlatform::Visionos => "visionOS",
        ApplePlatform::Watchos => "watchOS",
    }
}

fn physical_device_matches_platform(device: &PhysicalDevice, platform: ApplePlatform) -> bool {
    let platform_name = device.hardware_properties.platform.as_str();
    match platform {
        ApplePlatform::Ios => platform_name.eq_ignore_ascii_case("iOS"),
        ApplePlatform::Tvos => platform_name.eq_ignore_ascii_case("tvOS"),
        ApplePlatform::Visionos => {
            platform_name.eq_ignore_ascii_case("visionOS")
                || platform_name.eq_ignore_ascii_case("xrOS")
        }
        ApplePlatform::Watchos => platform_name.eq_ignore_ascii_case("watchOS"),
        ApplePlatform::Macos => platform_name.eq_ignore_ascii_case("macOS"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;
    use tempfile::TempDir;

    use super::{
        ApplePlatform, BuildReceipt, DeviceRunningProcess, PhysicalCpuType, PhysicalDevice,
        PhysicalDeviceProperties, PhysicalHardwareProperties, device_is_locked_from_details,
        error_mentions_locked_device, expect_macos_wrapper_script,
        find_running_process_for_installation, lldb_expect_attach_script,
        lldb_expect_macos_launch_script, lldb_expect_macos_run_script,
        lldb_expect_simulator_attach_script, macos_debug_wrapper_script, macos_executable_path,
        macos_trace_launch_executable_name, macos_xcode_log_redirect_env,
        select_physical_device_from_candidates, simulator_run_wrapper_script,
    };
    use crate::apple::build::receipt::BuildReceiptInput;
    use crate::apple::simulator::SimulatorDevice;
    use crate::apple::xcode::{SelectedXcode, lldb_path as selected_xcode_lldb_path};
    use crate::manifest::{BuildConfiguration, DistributionKind};

    fn physical_device(identifier: &str, udid: &str, name: &str, platform: &str) -> PhysicalDevice {
        PhysicalDevice {
            identifier: identifier.to_owned(),
            device_properties: PhysicalDeviceProperties {
                name: name.to_owned(),
                os_build_update: None,
                os_version_number: None,
            },
            hardware_properties: PhysicalHardwareProperties {
                cpu_type: PhysicalCpuType {
                    name: "arm64e".to_owned(),
                },
                platform: platform.to_owned(),
                product_type: None,
                udid: udid.to_owned(),
            },
        }
    }

    #[test]
    fn builds_trace_launch_executable_name() {
        assert_eq!(
            macos_trace_launch_executable_name("1775304762 ExampleMacApp.app"),
            "OrbitTrace1775304762_ExampleMacApp_app"
        );
        assert_eq!(
            macos_trace_launch_executable_name("%%%"),
            "OrbitTraceLaunch"
        );
    }

    fn fake_selected_xcode() -> (TempDir, SelectedXcode) {
        let temp = tempfile::tempdir().unwrap();
        let developer_dir = temp
            .path()
            .join("Xcode-26.4.app")
            .join("Contents")
            .join("Developer");
        let lldb = developer_dir.join("usr").join("bin").join("lldb");
        let log_redirect = developer_dir
            .join("usr")
            .join("lib")
            .join("libLogRedirect.dylib");
        std::fs::create_dir_all(lldb.parent().unwrap()).unwrap();
        std::fs::create_dir_all(log_redirect.parent().unwrap()).unwrap();
        std::fs::write(&lldb, b"").unwrap();
        std::fs::write(&log_redirect, b"").unwrap();

        let selected = SelectedXcode {
            version: "26.4".to_owned(),
            build_version: "17E192".to_owned(),
            app_path: temp.path().join("Xcode-26.4.app"),
            developer_dir,
        };

        (temp, selected)
    }

    #[test]
    fn detects_locked_device_from_devicectl_lock_state_details() {
        let details = json!({
            "result": {
                "device": {
                    "lockState": {
                        "name": "locked"
                    }
                }
            }
        });

        assert_eq!(device_is_locked_from_details(&details), Some(true));
    }

    #[test]
    fn detects_unlocked_device_from_devicectl_lock_state_details() {
        let details = json!({
            "result": {
                "device": {
                    "connectionProperties": {
                        "lockState": "unlocked"
                    }
                }
            }
        });

        assert_eq!(device_is_locked_from_details(&details), Some(false));
    }

    #[test]
    fn detects_locked_device_from_passcode_required_field() {
        let details = json!({
            "result": {
                "deviceIdentifier": "F1E218C7-32D3-5E36-BD5D-BC0CA366504B",
                "passcodeRequired": true,
                "unlockedSinceBoot": true
            }
        });

        assert_eq!(device_is_locked_from_details(&details), Some(true));
    }

    #[test]
    fn recognizes_locked_device_errors_from_tool_output() {
        assert!(error_mentions_locked_device(
            "The operation failed since the device is locked. Unlock the device and try again."
        ));
        assert!(error_mentions_locked_device("Device needs to be unlocked."));
        assert!(!error_mentions_locked_device(
            "Failed to connect to remote service."
        ));
    }

    #[test]
    fn non_interactive_multiple_physical_devices_require_explicit_identifier() {
        let devices = vec![
            physical_device("DEVICE-1", "UDID-1", "First iPhone", "iOS"),
            physical_device("DEVICE-2", "UDID-2", "Second iPhone", "iOS"),
        ];

        let error =
            select_physical_device_from_candidates(devices, None, false, ApplePlatform::Ios)
                .unwrap_err()
                .to_string();

        assert!(error.contains("multiple connected ios devices were found"));
        assert!(error.contains("--device-id"));
        assert!(error.contains("First iPhone (UDID-1)"));
        assert!(error.contains("Second iPhone (UDID-2)"));
    }

    #[test]
    fn explicit_identifier_selects_matching_physical_device() {
        let devices = vec![
            physical_device("DEVICE-1", "UDID-1", "First iPhone", "iOS"),
            physical_device("DEVICE-2", "UDID-2", "Second iPhone", "iOS"),
        ];

        let selected = select_physical_device_from_candidates(
            devices,
            Some("Second iPhone"),
            false,
            ApplePlatform::Ios,
        )
        .unwrap();

        assert_eq!(selected.provisioning_udid(), "UDID-2");
    }

    #[test]
    fn finds_macos_executable_in_standard_bundle_layout_only() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("ExampleMacApp.app");
        let standard_binary = bundle_root
            .join("Contents")
            .join("MacOS")
            .join("ExampleMacApp");
        std::fs::create_dir_all(standard_binary.parent().unwrap()).unwrap();
        std::fs::write(&standard_binary, b"binary").unwrap();

        let receipt = BuildReceipt::new(BuildReceiptInput {
            target: "ExampleMacApp".to_owned(),
            platform: ApplePlatform::Macos,
            configuration: BuildConfiguration::Debug,
            distribution: DistributionKind::Development,
            destination: "local".to_owned(),
            bundle_id: "dev.orbit.examples.examplemacapp".to_owned(),
            bundle_path: bundle_root.clone(),
            artifact_path: bundle_root,
        });

        assert_eq!(macos_executable_path(&receipt).unwrap(), standard_binary);
    }

    #[test]
    fn finds_running_process_for_installation_url() {
        let processes = vec![
            DeviceRunningProcess {
                executable: Some(
                    "file:///private/var/containers/Bundle/Application/OTHER/Other.app/Other"
                        .to_owned(),
                ),
                process_identifier: 41,
            },
            DeviceRunningProcess {
                executable: Some(
                    "file:///private/var/containers/Bundle/Application/EXAMPLE/ExampleIOSApp.app/ExampleIOSApp"
                        .to_owned(),
                ),
                process_identifier: 99,
            },
        ];

        let process = find_running_process_for_installation(
            &processes,
            "file:///private/var/containers/Bundle/Application/EXAMPLE/ExampleIOSApp.app/",
        )
        .expect("expected matching process");

        assert_eq!(process.process_identifier, 99);
    }

    #[test]
    fn lldb_expect_script_waits_for_attach_before_continue() {
        let script =
            lldb_expect_attach_script(Some(Path::new("/tmp/iOS DeviceSupport/Symbols"))).unwrap();

        assert!(script.contains("device process attach --pid $pid"));
        assert!(script.contains("wait_for_log [format {Process %s stopped} $pid]"));
        assert!(script.contains("send -- \"process continue\\r\""));
        assert!(script.contains("wait_for_stop_or_prompt $pid"));
        assert!(script.contains("send -- \"\\003\""));
        assert!(script.contains("send -- \"process kill\\r\""));
        assert!(script.contains("send -- \"quit\\r\""));
        assert!(script.contains("send -- \"Y\\r\""));
        assert!(script.contains("settings append target.exec-search-paths"));
        assert!(script.contains("interact"));
    }

    #[test]
    fn lldb_expect_simulator_script_attaches_and_handles_ctrl_c() {
        let (_temp, selected_xcode) = fake_selected_xcode();
        let script = lldb_expect_simulator_attach_script(Some(&selected_xcode)).unwrap();
        let expected_lldb_path = format!(
            "set lldb_path \"{}\"",
            selected_xcode_lldb_path(Some(&selected_xcode))
                .unwrap()
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('[', "\\[")
                .replace(']', "\\]")
        );

        assert!(script.contains("log_user 0"));
        assert!(script.contains("log_user 1"));
        assert!(script.contains("send_user \"(lldb) \""));
        assert!(script.contains(&expected_lldb_path));
        assert!(script.contains("spawn -noecho $lldb_path $exe"));
        assert!(script.contains("settings set interpreter.echo-commands false"));
        assert!(script.contains("settings set show-progress false"));
        assert!(script.contains("settings set show-statusline false"));
        assert!(script.contains("settings set use-color false"));
        assert!(script.contains("process attach -i -w -n $process_name"));
        assert!(script.contains("wait_for_log {Process [0-9]+ stopped}"));
        assert!(script.contains("send -- \"process continue\\r\""));
        assert!(script.contains("send -- \"\\003\""));
        assert!(script.contains("send -- \"process kill\\r\""));
        assert!(script.contains("send -- \"quit\\r\""));
        assert!(script.contains("send -- \"Y\\r\""));
        assert!(script.contains("interact"));
    }

    #[test]
    fn simulator_run_wrapper_uses_simctl_launch_and_ctrl_c_cleanup() {
        let (_temp, selected_xcode) = fake_selected_xcode();
        let device = SimulatorDevice {
            udid: "SIM-UDID".to_owned(),
            name: "iPhone 17 Pro".to_owned(),
            state: "Booted".to_owned(),
        };
        let receipt = BuildReceipt {
            id: "receipt-1".to_owned(),
            target: "ExampleIOSApp".to_owned(),
            platform: ApplePlatform::Ios,
            configuration: crate::manifest::BuildConfiguration::Debug,
            distribution: crate::manifest::DistributionKind::Development,
            destination: "simulator".to_owned(),
            bundle_id: "dev.orbit.examples.exampleiosapp".to_owned(),
            bundle_path: PathBuf::from("/tmp/ExampleIOSApp.app"),
            artifact_path: PathBuf::from("/tmp/ExampleIOSApp.app"),
            created_at_unix: 1,
            submit_eligible: false,
        };

        let script =
            simulator_run_wrapper_script(Some(&selected_xcode), &device, &receipt).unwrap();

        assert!(script.contains("export DEVELOPER_DIR="));
        assert!(
            script
                .contains("/usr/bin/xcrun simctl launch --console-pty --terminate-running-process")
        );
        assert!(script.contains("trap 'cleanup 0' INT"));
        assert!(script.contains("trap 'cleanup 0' TERM HUP"));
        assert!(script.contains("/usr/bin/xcrun simctl terminate"));
        assert!(script.contains("launcher_pid=$!"));
        assert!(!script.contains("idb launch"));
    }

    #[test]
    fn lldb_expect_macos_launch_script_starts_stopped_before_continue() {
        let (_temp, selected_xcode) = fake_selected_xcode();
        let script = lldb_expect_macos_launch_script(Some(&selected_xcode)).unwrap();
        let expected_lldb_path = format!(
            "set lldb_path \"{}\"",
            selected_xcode_lldb_path(Some(&selected_xcode))
                .unwrap()
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('[', "\\[")
                .replace(']', "\\]")
        );

        assert!(script.contains("set log_pipe [lindex $argv 1]"));
        assert!(script.contains(&expected_lldb_path));
        assert!(script.contains("process launch -s -o $log_pipe -e $log_pipe"));
        assert!(script.contains("send -- \"continue\\r\""));
        assert!(script.contains("\\003"));
        assert!(script.contains("send -- \"quit\\r\""));
        assert!(script.contains("send -- \"Y\\r\""));
        assert!(script.contains("proc kill_target {exe}"));
        assert!(script.contains("kill_target $exe"));
        assert!(script.contains("wait_for_message {Process [0-9]+ launched}"));
        assert!(script.contains("wait_for_message {Process [0-9]+ resuming}"));
        assert!(script.contains(&macos_xcode_log_redirect_env(Some(&selected_xcode)).unwrap()));
        assert!(script.contains("interact"));
    }

    #[test]
    fn lldb_expect_macos_run_script_redirects_inferior_stdio() {
        let (_temp, selected_xcode) = fake_selected_xcode();
        let script = lldb_expect_macos_run_script(Some(&selected_xcode)).unwrap();
        let expected_lldb_path = format!(
            "set lldb_path \"{}\"",
            selected_xcode_lldb_path(Some(&selected_xcode))
                .unwrap()
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('[', "\\[")
                .replace(']', "\\]")
        );

        assert!(script.contains("log_user 0"));
        assert!(script.contains("set log_pipe [lindex $argv 1]"));
        assert!(script.contains(&expected_lldb_path));
        assert!(script.contains("process launch -s -o $log_pipe -e $log_pipe"));
        assert!(script.contains("continue\\r"));
        assert!(script.contains(&macos_xcode_log_redirect_env(Some(&selected_xcode)).unwrap()));
    }

    #[test]
    fn expect_macos_wrapper_script_forwards_ctrl_c_to_wrapper() {
        let script = expect_macos_wrapper_script().unwrap();

        assert!(script.contains("spawn -noecho /bin/zsh $wrapper"));
        assert!(script.contains("\\003"));
        assert!(script.contains("exec /bin/kill -TERM [exp_pid]"));
        assert!(script.contains("exec /usr/bin/pkill -TERM -P [exp_pid]"));
        assert!(script.contains("exec /usr/bin/pkill -KILL -P [exp_pid]"));
        assert!(script.contains("exec /bin/kill -KILL [exp_pid]"));
        assert!(script.contains("timeout {"));
    }

    #[test]
    fn macos_debug_wrapper_script_installs_guardian_cleanup() {
        let script = macos_debug_wrapper_script(
            "/tmp/ExampleMacApp",
            "/tmp/debug.expect",
            "/tmp/inferior.pipe",
            "dev.orbit.examples.macos",
        )
        .unwrap();

        assert!(script.contains("/usr/bin/setsid /bin/zsh -c"));
        assert!(script.contains("guardian_pid=$!"));
        assert!(script.contains("/usr/bin/pkill -TERM -P $$"));
        assert!(script.contains("/usr/bin/pkill -KILL -f '/tmp/ExampleMacApp'"));
    }
}
