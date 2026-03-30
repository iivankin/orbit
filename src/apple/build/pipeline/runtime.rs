use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tempfile::NamedTempFile;

use crate::apple::build::receipt::BuildReceipt;
use crate::context::ProjectContext;
use crate::manifest::ApplePlatform;
use crate::util::{
    CliSpinner, command_output, command_output_allow_failure, prompt_select, run_command,
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

pub(super) fn run_on_macos(receipt: &BuildReceipt) -> Result<()> {
    let executable = macos_executable_path(receipt)?;
    println!(
        "Launching {} on the local Mac. Orbit will hand control to the app until it exits; press Ctrl-C to stop.",
        receipt.bundle_id
    );

    let mut command = Command::new(&executable);
    if let Some(bundle_root) = receipt.bundle_path.parent() {
        command.current_dir(bundle_root);
    }
    let debug = crate::util::debug_command(&command);
    let error = command.exec();
    bail!("failed to execute `{debug}`: {error}")
}

pub(super) fn debug_on_macos(receipt: &BuildReceipt) -> Result<()> {
    let executable = macos_executable_path(receipt)?;
    println!(
        "Launching LLDB for {} on the local Mac. Orbit will stop at process entry so you can set breakpoints before continuing.",
        receipt.bundle_id
    );

    let mut command = Command::new("lldb");
    command.arg("--file").arg(&executable);
    command.arg("-o").arg("process launch --stop-at-entry");
    if let Some(bundle_root) = receipt.bundle_path.parent() {
        command.current_dir(bundle_root);
    }
    run_command(&mut command)
}

pub(super) fn run_on_simulator(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    let device = prepare_simulator_installation(project, receipt)?;

    println!(
        "Launching {} on {}. Orbit will stay attached to the simulator console; press Ctrl-C to stop.",
        receipt.bundle_id, device.name
    );

    let mut launch = Command::new("xcrun");
    launch.args([
        "simctl",
        "launch",
        "--console-pty",
        &device.udid,
        &receipt.bundle_id,
    ]);
    run_command(&mut launch)
}

pub(super) fn debug_on_simulator(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    let device = prepare_simulator_installation(project, receipt)?;
    let executable = bundle_debug_executable_path(receipt)?;

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB, attach, and continue the app.",
        receipt.bundle_id, device.name
    );

    let mut launch = Command::new("xcrun");
    launch.args([
        "simctl",
        "launch",
        "--wait-for-debugger",
        "--terminate-running-process",
        &device.udid,
        &receipt.bundle_id,
    ]);
    run_command(&mut launch)?;

    let mut command = Command::new("lldb");
    command.arg("--file").arg(&executable);
    command.arg("-o").arg(format!(
        "process attach -i -w -n {}",
        simulator_process_name(receipt)
    ));
    command.arg("-o").arg("process continue");
    if let Some(bundle_root) = receipt.bundle_path.parent() {
        command.current_dir(bundle_root);
    }
    run_command(&mut command)
}

pub(super) fn run_on_device(device: &PhysicalDevice, receipt: &BuildReceipt) -> Result<()> {
    let installed = install_on_device(device, receipt)?;
    if receipt.platform == ApplePlatform::Ios {
        launch_ios_app_by_bundle_id(device, &receipt.bundle_id)?;
    } else {
        let remote_bundle_path = remote_app_bundle_path(&installed.installation_url)?;
        launch_device_app(device, &remote_bundle_path, false)?;
    }
    Ok(())
}

pub(super) fn debug_on_device(
    project: &ProjectContext,
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
) -> Result<()> {
    if receipt.platform == ApplePlatform::Ios {
        return debug_on_ios_device(project, device, receipt);
    }

    let installed = install_on_device(device, receipt)?;
    let executable = bundle_debug_executable_path(receipt)?;
    ensure_device_is_unlocked_for_debugging(device, receipt.platform)?;
    let symbol_root = ensure_device_symbols_available(project, device, receipt.platform)?;
    ensure_device_is_unlocked_for_debugging(device, receipt.platform)?;

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB, attach, and continue the app. Use `quit` to end the session; Ctrl-C interrupts the target.",
        receipt.bundle_id,
        device.name()
    );

    let remote_bundle_path = remote_app_bundle_path(&installed.installation_url)?;
    let launch = launch_device_app(device, &remote_bundle_path, true)?;

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
    let mut devices = list_devicectl_devices(platform)?;

    if let Some(identifier) = requested_identifier {
        return devices
            .into_iter()
            .find(|device| device.matches(identifier))
            .with_context(|| format!("no connected {platform} device matched `{identifier}`"));
    }

    if devices.is_empty() {
        bail!("no connected {platform} devices were found through `devicectl`");
    }

    if !project.app.interactive || devices.len() == 1 {
        return Ok(devices.remove(0));
    }

    let labels = devices
        .iter()
        .map(PhysicalDevice::selection_label)
        .collect::<Vec<_>>();
    let index = prompt_select("Select a physical device", &labels)?;
    Ok(devices.remove(index))
}

#[derive(Debug, Clone, Deserialize)]
struct SimctlList {
    devices: BTreeMap<String, Vec<SimulatorDevice>>,
}

#[derive(Debug, Clone, Deserialize)]
struct SimulatorDevice {
    udid: String,
    name: String,
    state: String,
}

impl SimulatorDevice {
    fn is_booted(&self) -> bool {
        self.state.eq_ignore_ascii_case("Booted")
    }
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

fn macos_executable_path(receipt: &BuildReceipt) -> Result<PathBuf> {
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
    let installed = install_on_device(device, receipt)?;
    let executable = bundle_debug_executable_path(receipt)?;
    ensure_device_is_unlocked_for_debugging(device, receipt.platform)?;
    let symbol_root = ensure_device_symbols_available(project, device, receipt.platform)?;
    ensure_device_is_unlocked_for_debugging(device, receipt.platform)?;

    println!(
        "Launching {} on {} in debug mode. Orbit will open LLDB and attach to the launched app. Use `quit` to end the session; Ctrl-C interrupts the target.",
        receipt.bundle_id,
        device.name()
    );

    let mut launch = spawn_ios_debug_launch_session(device, &receipt.bundle_id)?;
    let process = wait_for_device_process_for_installation(
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

fn launch_ios_app_by_bundle_id(
    device: &PhysicalDevice,
    bundle_id: &str,
) -> Result<DeviceLaunchedProcess> {
    let output_path = NamedTempFile::new()?;
    let mut launch = Command::new("xcrun");
    launch.args([
        "devicectl",
        "device",
        "process",
        "launch",
        "--device",
        device.provisioning_udid(),
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
        bundle_id,
    ]);
    run_command(&mut launch)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let launched: DeviceCtlEnvelope<LaunchedProcessResult> = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device process launch` output")?;
    Ok(launched.result.process)
}

fn spawn_ios_debug_launch_session(device: &PhysicalDevice, bundle_id: &str) -> Result<Child> {
    let mut launch = Command::new("xcrun");
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
        bundle_id,
    ]);
    launch.stdin(Stdio::inherit());
    launch.stdout(Stdio::inherit());
    launch.stderr(Stdio::inherit());
    launch.spawn().with_context(|| {
        format!(
            "failed to execute `{}`",
            crate::util::debug_command(&launch)
        )
    })
}

fn prepare_simulator_installation(
    project: &ProjectContext,
    receipt: &BuildReceipt,
) -> Result<SimulatorDevice> {
    let device = select_simulator_device(project, receipt.platform)?;
    if !device.is_booted() {
        let mut boot = Command::new("xcrun");
        boot.args(["simctl", "boot", &device.udid]);
        run_command(&mut boot)?;
    }

    let mut bootstatus = Command::new("xcrun");
    bootstatus.args(["simctl", "bootstatus", &device.udid, "-b"]);
    run_command(&mut bootstatus)?;

    let mut open_simulator = Command::new("open");
    open_simulator.args([
        "-a",
        "Simulator",
        "--args",
        "-CurrentDeviceUDID",
        &device.udid,
    ]);
    run_command(&mut open_simulator)?;

    let mut install = Command::new("xcrun");
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
    device: &PhysicalDevice,
    receipt: &BuildReceipt,
) -> Result<InstalledApplication> {
    let output_path = NamedTempFile::new()?;
    let mut install = Command::new("xcrun");
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
    device: &PhysicalDevice,
    remote_bundle_path: &str,
    start_stopped: bool,
) -> Result<DeviceLaunchedProcess> {
    let output_path = NamedTempFile::new()?;
    let mut launch = Command::new("xcrun");
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
        remote_bundle_path,
    ]);
    run_command(&mut launch)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let launched: DeviceCtlEnvelope<LaunchedProcessResult> = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device process launch` output")?;
    Ok(launched.result.process)
}

fn list_device_processes(device: &PhysicalDevice) -> Result<Vec<DeviceRunningProcess>> {
    let output_path = NamedTempFile::new()?;
    let mut command = Command::new("xcrun");
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
    run_command(&mut command)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let processes: DeviceCtlEnvelope<RunningProcessesResult> = serde_json::from_str(&contents)
        .context("failed to parse `devicectl device info processes` output")?;
    Ok(processes.result.running_processes)
}

fn wait_for_device_process_for_installation(
    device: &PhysicalDevice,
    installation_url: &str,
    timeout: Duration,
    mut launch_child: Option<&mut Child>,
) -> Result<DeviceRunningProcess> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(process) =
            find_running_process_for_installation(&list_device_processes(device)?, installation_url)
        {
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
    run_command(&mut command)
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
interact
"#,
        symbol_setup = symbol_setup
    ))
}

fn tcl_quote_arg(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('[', "\\[")
        .replace(']', "\\]")
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

fn select_simulator_device(
    project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<SimulatorDevice> {
    let output = command_output(Command::new("xcrun").args([
        "simctl",
        "list",
        "devices",
        "available",
        "--json",
    ]))?;
    let devices: SimctlList = serde_json::from_str(&output)?;
    let mut flattened = devices
        .devices
        .into_iter()
        .filter(|(runtime, _)| simulator_runtime_matches_platform(runtime, platform))
        .flat_map(|(_, devices)| devices)
        .collect::<Vec<_>>();
    flattened.sort_by(|left, right| {
        right
            .is_booted()
            .cmp(&left.is_booted())
            .then_with(|| left.name.cmp(&right.name))
    });

    if flattened.is_empty() {
        bail!("no available {platform} simulators were found");
    }

    let display = flattened
        .iter()
        .map(|device| format!("{} ({})", device.name, device.state))
        .collect::<Vec<_>>();
    let index = if project.app.interactive {
        prompt_select("Select a simulator", &display)?
    } else {
        0
    };
    Ok(flattened.remove(index))
}

fn list_devicectl_devices(platform: ApplePlatform) -> Result<Vec<PhysicalDevice>> {
    let output_path = NamedTempFile::new()?;
    let mut list = Command::new("xcrun");
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

    ensure_device_is_unlocked_for_symbol_download(device, platform)?;

    let spinner = CliSpinner::new(format!("Caching device symbols for {platform}"));
    match prepare_device_support_symbols(device, platform) {
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

fn prepare_device_support_symbols(device: &PhysicalDevice, platform: ApplePlatform) -> Result<()> {
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

    let mut command = Command::new("xcodebuild");
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
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<()> {
    ensure_device_is_unlocked(
        device,
        platform,
        locked_device_symbol_download_message(device),
    )
}

fn ensure_device_is_unlocked_for_debugging(
    device: &PhysicalDevice,
    platform: ApplePlatform,
) -> Result<()> {
    ensure_device_is_unlocked(device, platform, locked_device_debug_message(device))
}

fn ensure_device_is_unlocked(
    device: &PhysicalDevice,
    platform: ApplePlatform,
    failure_message: String,
) -> Result<()> {
    if platform == ApplePlatform::Macos {
        return Ok(());
    }

    let output_path = NamedTempFile::new()?;
    let mut command = Command::new("xcrun");
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

fn combine_command_output(stdout: &str, stderr: &str) -> String {
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_owned(),
        (true, false) => stderr.to_owned(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
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

fn lldb_quote_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
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

fn simulator_runtime_matches_platform(runtime_identifier: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios => runtime_identifier.contains(".SimRuntime.iOS-"),
        ApplePlatform::Tvos => runtime_identifier.contains(".SimRuntime.tvOS-"),
        ApplePlatform::Visionos => {
            runtime_identifier.contains(".SimRuntime.xrOS-")
                || runtime_identifier.contains(".SimRuntime.visionOS-")
        }
        ApplePlatform::Watchos => runtime_identifier.contains(".SimRuntime.watchOS-"),
        ApplePlatform::Macos => runtime_identifier.contains(".SimRuntime.macOS-"),
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
    use std::path::Path;

    use serde_json::json;

    use super::{
        ApplePlatform, BuildReceipt, DeviceRunningProcess, device_is_locked_from_details,
        error_mentions_locked_device, find_running_process_for_installation,
        lldb_expect_attach_script, macos_executable_path,
    };
    use crate::apple::build::receipt::BuildReceiptInput;
    use crate::manifest::{BuildConfiguration, DistributionKind};

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
        assert!(script.contains("settings append target.exec-search-paths"));
        assert!(script.contains("interact"));
    }
}
