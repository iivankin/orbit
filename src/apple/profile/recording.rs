use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
#[cfg(unix)]
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::export::wait_for_exportable_trace;
use crate::apple::xcode::{SelectedXcode, xcrun_command};
use crate::cli::ProfileKind;
use crate::util::{command_output_allow_failure, debug_command, ensure_parent_dir, timestamp_slug};
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use signal_hook::consts::signal::SIGINT;
#[cfg(unix)]
use signal_hook::iterator::{Handle as SignalHandle, Signals};

pub(crate) const SIMULATOR_PROFILING_UNAVAILABLE_MESSAGE: &str = "simulator profiling is currently unavailable because Apple's xctrace/InstrumentsCLI simulator path is unstable and can hang or emit broken traces. Use a physical device or macOS target instead.";
const TRACE_RECORDING_FINALIZE_TIMEOUT: Duration = Duration::from_secs(90);
const TRACE_RECORDING_INTERRUPT_GRACE: Duration = Duration::from_millis(250);

pub(crate) struct TraceRecording {
    output_path: PathBuf,
    selected_xcode: Option<SelectedXcode>,
    child: Child,
    debug: String,
    backend: TraceRecordingBackend,
    interrupt_grace: Duration,
}

#[derive(Debug, Clone, Copy)]
enum TraceRecordingBackend {
    Xctrace,
    #[cfg(test)]
    PlainFile,
}

#[derive(Debug, Clone, Copy)]
enum TraceLaunchStdio {
    Inherit,
    Null,
}

struct LaunchedTraceRequest<'a> {
    root: &'a Path,
    selected_xcode: Option<&'a SelectedXcode>,
    interactive: bool,
    kind: ProfileKind,
    launch_command: &'a [String],
    launch_environment: &'a [(String, String)],
    device: Option<&'a str>,
    stdio: TraceLaunchStdio,
}

#[cfg(unix)]
struct SignalForwarder {
    handle: SignalHandle,
    thread: Option<JoinHandle<()>>,
}

#[cfg(not(unix))]
struct SignalForwarder;

pub(crate) fn start_optional_launched_process_trace(
    root: &Path,
    selected_xcode: Option<&SelectedXcode>,
    interactive: bool,
    kind: Option<ProfileKind>,
    launch_target: &str,
    device: Option<&str>,
) -> Result<Option<(ProfileKind, TraceRecording)>> {
    kind.map(|kind| {
        start_launched_process_trace(
            root,
            selected_xcode,
            interactive,
            kind,
            launch_target,
            device,
        )
        .map(|recording| (kind, recording))
    })
    .transpose()
}

pub(crate) fn start_optional_launched_command_trace(
    root: &Path,
    selected_xcode: Option<&SelectedXcode>,
    interactive: bool,
    kind: Option<ProfileKind>,
    launch_command: &[String],
    launch_environment: &[(String, String)],
    device: Option<&str>,
) -> Result<Option<(ProfileKind, TraceRecording)>> {
    kind.map(|kind| {
        start_launched_trace(LaunchedTraceRequest {
            root,
            selected_xcode,
            interactive,
            kind,
            launch_command,
            launch_environment,
            device,
            stdio: TraceLaunchStdio::Inherit,
        })
        .map(|recording| (kind, recording))
    })
    .transpose()
}

pub(crate) fn ensure_simulator_profiling_supported(kind: Option<ProfileKind>) -> Result<()> {
    if kind.is_some() {
        bail!("{SIMULATOR_PROFILING_UNAVAILABLE_MESSAGE}");
    }
    Ok(())
}

pub(crate) fn wait_for_launched_trace_exit(
    kind: ProfileKind,
    recording: TraceRecording,
) -> Result<()> {
    // `orbi run --trace` tells the user to press Ctrl-C to stop the recording. We
    // intercept that signal here, forward it to `xctrace`, and stay alive long
    // enough for the trace bundle to become exportable.
    let (interrupt_tx, interrupt_rx) = mpsc::channel();
    let signal_forwarder = SignalForwarder::install(interrupt_tx)?;
    let path = wait_for_trace_recording_exit(kind, recording, Some(&interrupt_rx))?;
    drop(signal_forwarder);
    println!("trace: {}", path.display());
    Ok(())
}

pub(crate) fn finish_started_trace(
    kind: ProfileKind,
    recording: TraceRecording,
) -> Result<PathBuf> {
    finish_trace_recording(recording)
        .with_context(|| format!("failed to finalize {} trace", kind.trace_label()))
}

fn start_launched_process_trace(
    root: &Path,
    selected_xcode: Option<&SelectedXcode>,
    interactive: bool,
    kind: ProfileKind,
    launch_target: &str,
    device: Option<&str>,
) -> Result<TraceRecording> {
    start_launched_trace(LaunchedTraceRequest {
        root,
        selected_xcode,
        interactive,
        kind,
        launch_command: &[launch_target.to_owned()],
        launch_environment: &[],
        device,
        stdio: TraceLaunchStdio::Null,
    })
}

fn start_launched_trace(request: LaunchedTraceRequest<'_>) -> Result<TraceRecording> {
    if request.launch_command.is_empty() {
        bail!("xctrace launched trace requires at least one launch argument");
    }

    let output_path = default_trace_output(request.root, request.kind)?;
    let mut command = build_xctrace_record_command(&request, &output_path);
    let debug = debug_command(&command);
    let child = command
        .spawn()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    Ok(TraceRecording {
        output_path,
        selected_xcode: request.selected_xcode.cloned(),
        child,
        debug,
        backend: TraceRecordingBackend::Xctrace,
        interrupt_grace: TRACE_RECORDING_INTERRUPT_GRACE,
    })
}

fn build_xctrace_record_command(request: &LaunchedTraceRequest<'_>, output_path: &Path) -> Command {
    let mut command = xcrun_command(request.selected_xcode);
    command.arg("xctrace");
    command.arg("record");
    command.arg("--template");
    command.arg(request.kind.trace_template());
    command.arg("--output");
    command.arg(output_path);
    if let Some(device) = request.device {
        command.arg("--device").arg(device);
    }
    if !request.interactive {
        command.arg("--no-prompt");
    }
    command.arg("--env");
    command.arg("OS_ACTIVITY_DT_MODE=1");
    command.arg("--env");
    command.arg("IDEPreferLogStreaming=YES");
    for (key, value) in request.launch_environment {
        command.arg("--env");
        command.arg(format!("{key}={value}"));
    }
    command.arg("--launch");
    command.arg("--");
    command.args(request.launch_command);
    if matches!(request.stdio, TraceLaunchStdio::Null) {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
    }
    command
}

fn wait_for_trace_recording_exit(
    kind: ProfileKind,
    mut recording: TraceRecording,
    interrupt_rx: Option<&Receiver<()>>,
) -> Result<PathBuf> {
    let mut interrupted = false;

    loop {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() {
                return finish_trace_recording(recording)
                    .with_context(|| format!("failed to finalize {} trace", kind.trace_label()));
            }

            if interrupted {
                verify_recording_output(&recording).with_context(|| {
                    format!(
                        "failed to finalize {} trace after interruption",
                        kind.trace_label()
                    )
                })?;
                return Ok(recording.output_path);
            }

            bail!("`{}` failed with {status}", recording.debug);
        }

        if received_interrupt(interrupt_rx)? {
            interrupted = true;
            send_interrupt_to_child(&recording.child)?;
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn finish_trace_recording(recording: TraceRecording) -> Result<PathBuf> {
    match recording.backend {
        TraceRecordingBackend::Xctrace => finish_xctrace_recording(recording),
        #[cfg(test)]
        TraceRecordingBackend::PlainFile => finish_plain_recording(recording),
    }
}

fn accept_finished_xctrace_output(recording: &TraceRecording) -> Result<Option<PathBuf>> {
    if !recording.output_path.exists() {
        return Ok(None);
    }
    wait_for_exportable_trace(
        &recording.output_path,
        recording.selected_xcode.as_ref(),
        &recording.debug,
    )?;
    Ok(Some(recording.output_path.clone()))
}

fn verify_recording_output(recording: &TraceRecording) -> Result<()> {
    match recording.backend {
        TraceRecordingBackend::Xctrace => {
            wait_for_recording_output_path(recording, Duration::from_secs(5))?;
            wait_for_exportable_trace(
                &recording.output_path,
                recording.selected_xcode.as_ref(),
                &recording.debug,
            )
        }
        #[cfg(test)]
        TraceRecordingBackend::PlainFile => {
            wait_for_recording_output_path(recording, Duration::from_secs(2))
        }
    }
}

fn wait_for_recording_output_path(recording: &TraceRecording, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if recording.output_path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }

    bail!(
        "`{}` exited before writing {}",
        recording.debug,
        recording.output_path.display()
    )
}

fn finish_xctrace_recording(mut recording: TraceRecording) -> Result<PathBuf> {
    let graceful_wait_started = Instant::now();
    while graceful_wait_started.elapsed() < recording.interrupt_grace {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() {
                if let Some(path) = accept_finished_xctrace_output(&recording)? {
                    return Ok(path);
                }
            } else if let Some(path) = accept_finished_xctrace_output(&recording)? {
                return Ok(path);
            }
            bail!(
                "`{}` exited with {status} before writing {}",
                recording.debug,
                recording.output_path.display()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }

    if recording.child.try_wait()?.is_none() {
        let _ = send_interrupt_to_child(&recording.child);
    }

    let started = Instant::now();
    while started.elapsed() < TRACE_RECORDING_FINALIZE_TIMEOUT {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() {
                if let Some(path) = accept_finished_xctrace_output(&recording)? {
                    return Ok(path);
                }
            } else if let Some(path) = accept_finished_xctrace_output(&recording)? {
                return Ok(path);
            }
            bail!(
                "`{}` exited with {status} before writing {}",
                recording.debug,
                recording.output_path.display()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }

    let _ = recording.child.kill();
    let _ = recording.child.wait();
    if let Some(path) = accept_finished_xctrace_output(&recording)? {
        return Ok(path);
    }

    bail!(
        "timed out waiting for `{}` to finish writing an exportable trace at {}",
        recording.debug,
        recording.output_path.display()
    )
}

#[cfg(test)]
fn finish_plain_recording(mut recording: TraceRecording) -> Result<PathBuf> {
    let graceful_wait_started = Instant::now();
    while graceful_wait_started.elapsed() < TRACE_RECORDING_INTERRUPT_GRACE {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() && recording.output_path.exists() {
                return Ok(recording.output_path);
            }
            bail!(
                "`{}` exited with {status} before writing {}",
                recording.debug,
                recording.output_path.display()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }

    if recording.child.try_wait()?.is_none() {
        let mut interrupt = std::process::Command::new("kill");
        interrupt.args(["-INT", &recording.child.id().to_string()]);
        let _ = command_output_allow_failure(&mut interrupt)?;
    }

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() && recording.output_path.exists() {
                return Ok(recording.output_path);
            }
            bail!(
                "`{}` exited with {status} before writing {}",
                recording.debug,
                recording.output_path.display()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }

    let _ = recording.child.kill();
    let _ = recording.child.wait();
    if recording.output_path.exists() {
        return Ok(recording.output_path);
    }

    bail!(
        "timed out waiting for `{}` to finish writing {}",
        recording.debug,
        recording.output_path.display()
    )
}

fn received_interrupt(interrupt_rx: Option<&Receiver<()>>) -> Result<bool> {
    let Some(interrupt_rx) = interrupt_rx else {
        return Ok(false);
    };

    let mut received = false;
    loop {
        match interrupt_rx.try_recv() {
            Ok(()) => received = true,
            Err(TryRecvError::Empty) => return Ok(received),
            Err(TryRecvError::Disconnected) => return Ok(received),
        }
    }
}

fn send_interrupt_to_child(child: &Child) -> Result<()> {
    let mut interrupt = std::process::Command::new("kill");
    interrupt.args(["-INT", &child.id().to_string()]);
    let _ = command_output_allow_failure(&mut interrupt)?;
    Ok(())
}

impl SignalForwarder {
    #[cfg(unix)]
    fn install(interrupt_tx: mpsc::Sender<()>) -> Result<Self> {
        let mut signals = Signals::new([SIGINT])
            .context("failed to install Ctrl-C handler for trace recording")?;
        let handle = signals.handle();
        let thread = thread::spawn(move || {
            for _signal in &mut signals {
                let _ = interrupt_tx.send(());
            }
        });
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }

    #[cfg(not(unix))]
    fn install(_interrupt_tx: mpsc::Sender<()>) -> Result<Self> {
        Ok(Self)
    }
}

#[cfg(unix)]
impl Drop for SignalForwarder {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn default_trace_output(root: &Path, kind: ProfileKind) -> Result<PathBuf> {
    let output_path = root
        .join(".orbi")
        .join("artifacts")
        .join("profiles")
        .join(format!("{}-{}.trace", timestamp_slug(), kind.trace_slug()));
    validate_trace_output_path(&output_path)?;
    Ok(output_path)
}

fn validate_trace_output_path(output_path: &Path) -> Result<()> {
    if output_path.exists() && output_path.is_dir() {
        bail!(
            "trace output must be a `.trace` path, not a directory: {}",
            output_path.display()
        );
    }
    if output_path.extension().and_then(|value| value.to_str()) != Some("trace") {
        bail!(
            "trace output must end with `.trace`: {}",
            output_path.display()
        );
    }
    if output_path.exists() {
        bail!(
            "trace output already exists; choose a new path: {}",
            output_path.display()
        );
    }

    ensure_parent_dir(output_path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::{
        LaunchedTraceRequest, TraceLaunchStdio, TraceRecording, TraceRecordingBackend,
        build_xctrace_record_command, wait_for_trace_recording_exit,
    };
    use crate::cli::ProfileKind;

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn launched_trace_command_keeps_prompts_when_interactive() {
        let root = PathBuf::from("/tmp");
        let launch_command = ["/tmp/App.app/Contents/MacOS/App".to_owned()];
        let request = LaunchedTraceRequest {
            root: root.as_path(),
            selected_xcode: None,
            interactive: true,
            kind: ProfileKind::Memory,
            launch_command: &launch_command,
            launch_environment: &[],
            device: None,
            stdio: TraceLaunchStdio::Inherit,
        };
        let command = build_xctrace_record_command(
            &request,
            PathBuf::from("/tmp/interactive.trace").as_path(),
        );
        let args = command_args(&command);
        assert!(!args.iter().any(|value| value == "--no-prompt"));
    }

    #[test]
    fn launched_trace_command_suppresses_prompts_when_noninteractive() {
        let root = PathBuf::from("/tmp");
        let launch_command = ["/tmp/App.app/Contents/MacOS/App".to_owned()];
        let request = LaunchedTraceRequest {
            root: root.as_path(),
            selected_xcode: None,
            interactive: false,
            kind: ProfileKind::Memory,
            launch_command: &launch_command,
            launch_environment: &[],
            device: None,
            stdio: TraceLaunchStdio::Inherit,
        };
        let command = build_xctrace_record_command(
            &request,
            PathBuf::from("/tmp/noninteractive.trace").as_path(),
        );
        let args = command_args(&command);
        assert!(args.iter().any(|value| value == "--no-prompt"));
    }

    #[test]
    fn interrupted_trace_wait_returns_written_output_even_if_child_exits_non_zero() {
        let temp = tempdir().unwrap();
        let output_path = temp.path().join("capture.sample.txt");
        let ready_path = temp.path().join("writer.ready");
        let script_path = temp.path().join("writer.py");
        fs::write(
            &script_path,
            format!(
                r#"import pathlib, signal, time

def handler(signum, frame):
    return None

signal.signal(signal.SIGINT, handler)
signal.signal(signal.SIGTERM, handler)
pathlib.Path(r"{}").write_text("ready")
end = time.time() + 0.4
while time.time() < end:
    try:
        time.sleep(0.05)
    except InterruptedError:
        pass
pathlib.Path(r"{}").write_text("sample")
raise SystemExit(130)
"#,
                ready_path.display(),
                output_path.display()
            ),
        )
        .unwrap();

        let child = Command::new("python3")
            .arg("-S")
            .arg(&script_path)
            .spawn()
            .unwrap();
        let recording = TraceRecording {
            output_path: output_path.clone(),
            selected_xcode: None,
            child,
            debug: "writer".to_owned(),
            backend: TraceRecordingBackend::PlainFile,
            interrupt_grace: super::TRACE_RECORDING_INTERRUPT_GRACE,
        };

        let (interrupt_tx, interrupt_rx) = std::sync::mpsc::channel();
        let interrupt_ready_path = ready_path.clone();
        thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(2) {
                if interrupt_ready_path.exists() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            thread::sleep(Duration::from_millis(50));
            let _ = interrupt_tx.send(());
        });

        let path = wait_for_trace_recording_exit(ProfileKind::Cpu, recording, Some(&interrupt_rx))
            .unwrap();

        assert_eq!(path, output_path);
        assert_eq!(fs::read_to_string(&output_path).unwrap(), "sample");
    }
}
