use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

use crate::apple::xcode::SelectedXcode;

pub(crate) struct SimulatorAppLogStream {
    child: Child,
    stdout_forwarder: Option<JoinHandle<()>>,
    stderr_forwarder: Option<JoinHandle<()>>,
}

impl SimulatorAppLogStream {
    pub(crate) fn start(
        selected_xcode: Option<&SelectedXcode>,
        udid: &str,
        process_name: &str,
        bundle_id: &str,
        verbose: bool,
    ) -> Result<Self> {
        let mut command = simulator_log_command(selected_xcode);
        command.args([
            "simctl",
            "spawn",
            udid,
            "log",
            "stream",
            "--style",
            "compact",
            "--color",
            "none",
            "--level",
            "debug",
            "--process",
            process_name,
        ]);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command.spawn().with_context(|| {
            format!("failed to start simulator `log stream` for process `{process_name}`")
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("simulator `log stream` did not expose stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("simulator `log stream` did not expose stderr"))?;
        let process_name = process_name.to_owned();
        let bundle_id = bundle_id.to_owned();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stdout_ready_tx = ready_tx.clone();
        Ok(Self {
            child,
            stdout_forwarder: Some(thread::spawn({
                let process_name = process_name.clone();
                let bundle_id = bundle_id.clone();
                move || {
                    forward_simulator_log_stream(
                        stdout,
                        &process_name,
                        &bundle_id,
                        verbose,
                        false,
                        Some(stdout_ready_tx),
                    )
                }
            })),
            stderr_forwarder: Some(thread::spawn(move || {
                forward_simulator_log_stream(
                    stderr,
                    &process_name,
                    &bundle_id,
                    verbose,
                    true,
                    Some(ready_tx),
                )
            })),
        }
        .wait_until_ready(ready_rx))
    }

    fn wait_until_ready(mut self, ready_rx: mpsc::Receiver<()>) -> Self {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if ready_rx.recv_timeout(Duration::from_millis(50)).is_ok() {
                break;
            }

            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
        }
        self
    }

    fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_some() {
            self.join_forwarders();
            return Ok(());
        }

        let graceful_wait_started = Instant::now();
        while graceful_wait_started.elapsed() < Duration::from_millis(150) {
            if self.child.try_wait()?.is_some() {
                self.join_forwarders();
                return Ok(());
            }
            thread::sleep(Duration::from_millis(25));
        }

        let mut interrupt = Command::new("kill");
        interrupt.args(["-INT", &self.child.id().to_string()]);
        let _ = interrupt.status();

        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(1) {
            if self.child.try_wait()?.is_some() {
                self.join_forwarders();
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
        self.join_forwarders();
        Ok(())
    }

    fn join_forwarders(&mut self) {
        if let Some(handle) = self.stdout_forwarder.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_forwarder.take() {
            let _ = handle.join();
        }
    }
}

fn simulator_log_command(selected_xcode: Option<&SelectedXcode>) -> Command {
    let mut command = Command::new("xcrun");
    if let Some(selected_xcode) = selected_xcode {
        selected_xcode.configure_command(&mut command);
    }
    command
}

impl Drop for SimulatorAppLogStream {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub(crate) struct DeviceConsoleRelay {
    child: Child,
    stdout_forwarder: Option<JoinHandle<()>>,
    stderr_forwarder: Option<JoinHandle<()>>,
}

impl DeviceConsoleRelay {
    pub(crate) fn start(command: &mut Command, process_name: &str, verbose: bool) -> Result<Self> {
        Self::start_with_stdin(command, process_name, verbose, true)
    }

    pub(crate) fn start_without_stdin(
        command: &mut Command,
        process_name: &str,
        verbose: bool,
    ) -> Result<Self> {
        Self::start_with_stdin(command, process_name, verbose, false)
    }

    fn start_with_stdin(
        command: &mut Command,
        process_name: &str,
        verbose: bool,
        inherit_stdin: bool,
    ) -> Result<Self> {
        command.stdin(if inherit_stdin {
            Stdio::inherit()
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command.spawn().with_context(|| {
            format!("failed to start device console relay for process `{process_name}`")
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("device console command did not expose stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("device console command did not expose stderr"))?;
        let process_name = process_name.to_owned();
        Ok(Self {
            child,
            stdout_forwarder: Some(thread::spawn({
                let process_name = process_name.clone();
                move || forward_device_console_stream(stdout, &process_name, verbose, false)
            })),
            stderr_forwarder: Some(thread::spawn(move || {
                forward_device_console_stream(stderr, &process_name, verbose, true)
            })),
        })
    }

    pub(crate) fn wait(&mut self) -> Result<std::process::ExitStatus> {
        let status = self.child.wait()?;
        self.join_forwarders();
        Ok(status)
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    pub(crate) fn kill(&mut self) -> Result<()> {
        self.child.kill().map_err(Into::into)
    }

    fn join_forwarders(&mut self) {
        if let Some(handle) = self.stdout_forwarder.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_forwarder.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DeviceConsoleRelay {
    fn drop(&mut self) {
        let _ = self.child.try_wait();
        self.join_forwarders();
    }
}

pub(crate) struct MacosInferiorLogRelay {
    forwarder: Option<JoinHandle<()>>,
}

impl MacosInferiorLogRelay {
    pub(crate) fn start(pipe_path: &Path, bundle_id: &str, verbose: bool) -> Self {
        let pipe_path = pipe_path.to_path_buf();
        let bundle_id = bundle_id.to_owned();
        Self {
            forwarder: Some(thread::spawn(move || {
                forward_macos_inferior_pipe(&pipe_path, &bundle_id, verbose)
            })),
        }
    }
}

impl Drop for MacosInferiorLogRelay {
    fn drop(&mut self) {
        if let Some(handle) = self.forwarder.take() {
            let _ = handle.join();
        }
    }
}

fn forward_simulator_log_stream<R>(
    reader: R,
    process_name: &str,
    bundle_id: &str,
    verbose: bool,
    stderr: bool,
    mut ready_tx: Option<mpsc::Sender<()>>,
) where
    R: Read + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if simulator_log_stream_ready(&line)
                    && let Some(ready_tx) = ready_tx.take()
                {
                    let _ = ready_tx.send(());
                }

                if verbose {
                    write_terminal_stream_line(&line, stderr);
                    continue;
                }

                if let Some(rendered) = filter_simulator_log_line(&line, process_name, bundle_id) {
                    write_terminal_stream_line(rendered, stderr);
                }
            }
            Err(_) => break,
        }
    }
}

fn simulator_log_stream_ready(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("Filtering the log data using ") || trimmed == "Running log until ^C"
}

fn forward_macos_inferior_pipe(pipe_path: &Path, bundle_id: &str, verbose: bool) {
    let file = match std::fs::File::open(pipe_path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "warning: failed to open macOS run log pipe `{}`: {error}",
                pipe_path.display()
            );
            return;
        }
    };

    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if verbose {
                    write_terminal_stream_line(&line, false);
                    continue;
                }

                if let Some(rendered) = filter_macos_inferior_line(&line, bundle_id) {
                    write_terminal_stream_line(rendered, false);
                }
            }
            Err(_) => break,
        }
    }
}

fn forward_device_console_stream<R>(reader: R, process_name: &str, verbose: bool, stderr: bool)
where
    R: Read + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                for fragment in device_console_fragments(&line) {
                    if verbose {
                        write_terminal_stream_line(fragment, stderr);
                        continue;
                    }

                    if let Some(rendered) = filter_device_console_line(fragment, process_name) {
                        write_terminal_stream_line(rendered, stderr);
                    }
                }
            }
            Err(_) => break,
        }
    }
}

fn device_console_fragments(line: &str) -> impl Iterator<Item = &str> {
    line.split('\r')
        .filter(|fragment| !fragment.trim().is_empty())
}

fn write_terminal_stream_line(line: &str, stderr: bool) {
    // `expect interact` switches the controlling TTY into a mode where `\n`
    // no longer implies a carriage return. Emit explicit CRLF for interactive
    // terminals so background app logs stay left-aligned instead of forming
    // a staircase.
    if stderr {
        let mut stream = io::stderr();
        if stream.is_terminal() {
            let _ = write!(stream, "\r{}\r\n", line.trim_end_matches(['\n', '\r']));
        } else {
            let _ = write!(stream, "{line}");
        }
        let _ = stream.flush();
        return;
    }

    let mut stream = io::stdout();
    if stream.is_terminal() {
        let _ = write!(stream, "\r{}\r\n", line.trim_end_matches(['\n', '\r']));
    } else {
        let _ = write!(stream, "{line}");
    }
    let _ = stream.flush();
}

fn filter_macos_inferior_line<'a>(line: &'a str, bundle_id: &str) -> Option<&'a str> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if is_macos_runtime_noise(trimmed) {
        return None;
    }

    if !trimmed.starts_with("libLogRedirect:") {
        return Some(trimmed);
    }

    if trimmed.contains("Initialization successful") {
        return None;
    }

    if trimmed.contains("subsystem:\"") && !trimmed.contains(&format!("subsystem:\"{bundle_id}\""))
    {
        return None;
    }

    let message = trimmed.rsplit('\t').next().unwrap_or(trimmed);
    if is_macos_runtime_noise(message) {
        return None;
    }

    Some(message)
}

fn is_macos_runtime_noise(line: &str) -> bool {
    line.starts_with("os_unix.c:")
        || line.contains("__delegate_identifier__:Performance Diagnostics__")
            && line.contains(
                "This method should not be called on the main thread as it may lead to UI unresponsiveness.",
            )
}

fn filter_simulator_log_line<'a>(
    line: &'a str,
    process_name: &str,
    bundle_id: &str,
) -> Option<&'a str> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with("Filtering the log data using ")
        || trimmed.starts_with("Timestamp")
        || trimmed == "Running log until ^C"
        || trimmed == "Stopping log"
        || trimmed.starts_with("getpwuid_r did not find a match for uid ")
        || trimmed == "Traceback (most recent call last):"
        || trimmed.starts_with("asyncio.exceptions.CancelledError")
        || trimmed.starts_with("KeyboardInterrupt")
        || trimmed.starts_with("  File ")
        || trimmed.starts_with("    ")
    {
        return None;
    }

    if let Some(process_marker) = trimmed.find(&format!("{process_name}: ")) {
        let suffix = &trimmed[process_marker + process_name.len() + 2..];
        if suffix.starts_with(&format!("[{bundle_id}:")) {
            return Some(suffix.rsplit("] ").next().unwrap_or(suffix));
        }

        if suffix.starts_with('[') || suffix.starts_with('(') {
            return None;
        }

        return Some(suffix);
    }

    if let Some(process_marker) = trimmed.find(&format!("{process_name}[")) {
        let suffix = &trimmed[process_marker + process_name.len()..];
        let after_pid_marker = suffix.find("] ")?;
        let after_pid = &suffix[after_pid_marker + 2..];
        if after_pid.starts_with(&format!("[{bundle_id}:")) {
            return Some(after_pid.rsplit("] ").next().unwrap_or(after_pid));
        }

        if after_pid.starts_with('[') || after_pid.starts_with('(') {
            return None;
        }

        return Some(after_pid);
    }

    None
}

fn filter_device_console_line<'a>(line: &'a str, process_name: &str) -> Option<&'a str> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with("Failed to load provisioning paramter list due to error:")
        || trimmed.starts_with("`devicectl manage create` may support a reduced set of arguments.")
    {
        return None;
    }

    if trimmed.contains("Acquired tunnel connection to device.")
        || trimmed.contains("Enabling developer disk image services.")
        || trimmed.contains("Acquired usage assertion.")
        || trimmed.starts_with("Launched application with ")
        || trimmed.starts_with("Waiting for the application to terminate")
    {
        return None;
    }

    if trimmed.starts_with("os_unix.c:") {
        return None;
    }

    if trimmed.starts_with("PID") && trimmed.contains("Executable Path") {
        return None;
    }

    if is_devicectl_process_table_row(trimmed) {
        return None;
    }

    if let Some(process_marker) = trimmed.find(&format!("{process_name}[")) {
        let suffix = &trimmed[process_marker..];
        if let Some(message_index) = suffix.rfind("] ") {
            return Some(&suffix[message_index + 2..]);
        }
    }

    Some(trimmed)
}

fn is_devicectl_process_table_row(line: &str) -> bool {
    let mut parts = line.split_whitespace();
    let Some(pid) = parts.next() else {
        return false;
    };
    let Some(path) = parts.next() else {
        return false;
    };
    pid.bytes().all(|byte| byte.is_ascii_digit()) && path.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::{
        device_console_fragments, filter_device_console_line, filter_macos_inferior_line,
        filter_simulator_log_line,
    };

    #[test]
    fn macos_inferior_filter_keeps_print_lines() {
        assert_eq!(
            filter_macos_inferior_line("FixtureView print appeared\n", "dev.orbit.examples.macos"),
            Some("FixtureView print appeared")
        );
    }

    #[test]
    fn macos_inferior_filter_keeps_bundle_logger_lines() {
        let line = "libLogRedirect: 7 80 L 0 {subsystem:\"dev.orbit.examples.macos\",category:\"app\"}\tExampleMacApp launched\n";
        assert_eq!(
            filter_macos_inferior_line(line, "dev.orbit.examples.macos"),
            Some("ExampleMacApp launched")
        );
    }

    #[test]
    fn macos_inferior_filter_keeps_followup_logger_lines_without_subsystem() {
        let line = "libLogRedirect: 7 80 L 1 {category:\"fixture\",offset:0x114f4}\tFixtureView appeared\n";
        assert_eq!(
            filter_macos_inferior_line(line, "dev.orbit.examples.macos"),
            Some("FixtureView appeared")
        );
    }

    #[test]
    fn macos_inferior_filter_drops_other_subsystems() {
        let line = "libLogRedirect: 7 80 L 2 {subsystem:\"com.apple.BaseBoard\",category:\"Common\"}\tUnable to obtain a task name port right\n";
        assert_eq!(
            filter_macos_inferior_line(line, "dev.orbit.examples.macos"),
            None
        );
    }

    #[test]
    fn macos_inferior_filter_drops_detached_signature_noise_from_log_redirect() {
        let line = "libLogRedirect: 7 80 L 3 {t:1775597903.760651}\tos_unix.c:51044: (2) open(/private/var/db/DetachedSignatures) - No such file or directory\n";
        assert_eq!(
            filter_macos_inferior_line(line, "dev.orbit.examples.macos"),
            None
        );
    }

    #[test]
    fn macos_inferior_filter_drops_performance_diagnostics_noise_without_subsystem() {
        let line = "libLogRedirect: 7 80 L 5 {t:1775597903.762038}\t__delegate_identifier__:Performance Diagnostics__:::____message__:This method should not be called on the main thread as it may lead to UI unresponsiveness.\n";
        assert_eq!(
            filter_macos_inferior_line(line, "dev.orbit.examples.macos"),
            None
        );
    }

    #[test]
    fn device_console_filter_drops_devicectl_preamble() {
        assert_eq!(
            filter_device_console_line(
                "22:00:20  Acquired tunnel connection to device.\n",
                "AccordCampanion"
            ),
            None
        );
    }

    #[test]
    fn device_console_filter_extracts_message_from_logger_line() {
        let line = "2026-04-01 22:00:21.343104+0300 AccordCampanion[9122:5021858] [General] Failed to send CA Event\n";
        assert_eq!(
            filter_device_console_line(line, "AccordCampanion"),
            Some("Failed to send CA Event")
        );
    }

    #[test]
    fn device_console_filter_keeps_print_lines() {
        assert_eq!(
            filter_device_console_line("ExampleIOSApp print launched\n", "ExampleIOSApp"),
            Some("ExampleIOSApp print launched")
        );
    }

    #[test]
    fn device_console_filter_drops_process_table_header() {
        assert_eq!(
            filter_device_console_line("PID    Executable Path\n", "ExampleIOSApp"),
            None
        );
    }

    #[test]
    fn device_console_filter_drops_process_table_rows() {
        assert_eq!(
            filter_device_console_line(
                "9180   /private/var/containers/Bundle/Application/XYZ/ExampleIOSApp.app/ExampleIOSApp\n",
                "ExampleIOSApp"
            ),
            None
        );
    }

    #[test]
    fn device_console_fragments_split_carriage_return_delimited_output() {
        let fragments =
            device_console_fragments("ExampleIOSApp launched\rExampleLandingView appeared\r\n")
                .collect::<Vec<_>>();

        assert_eq!(
            fragments,
            vec!["ExampleIOSApp launched", "ExampleLandingView appeared"]
        );
    }

    #[test]
    fn simulator_log_filter_keeps_bundle_logger_lines() {
        let line = "2026-04-01 22:50:58.211904+0300 0x1cc1509  Default     0x0                  87108  0    ExampleIOSApp: [dev.orbit.examples.exampleiosapp:App] ExampleIOSApp launched\n";
        assert_eq!(
            filter_simulator_log_line(line, "ExampleIOSApp", "dev.orbit.examples.exampleiosapp"),
            Some("ExampleIOSApp launched")
        );
    }

    #[test]
    fn simulator_log_filter_drops_other_subsystems() {
        let line = "2026-04-01 22:50:58.223142+0300 0x1cc1515  Default     0x0                  87108  0    ExampleIOSApp: (UIKitCore) [com.apple.UIKit:BackgroundTask] Creating new assertion\n";
        assert_eq!(
            filter_simulator_log_line(line, "ExampleIOSApp", "dev.orbit.examples.exampleiosapp"),
            None
        );
    }

    #[test]
    fn simulator_log_filter_keeps_plain_print_lines() {
        let line = "ExampleIOSApp: ExampleIOSApp print launched\n";
        assert_eq!(
            filter_simulator_log_line(line, "ExampleIOSApp", "dev.orbit.examples.exampleiosapp"),
            Some("ExampleIOSApp print launched")
        );
    }

    #[test]
    fn simulator_log_filter_drops_preamble_and_traceback_noise() {
        assert_eq!(
            filter_simulator_log_line(
                "Filtering the log data using \"process BEGINSWITH[cd] \\\"ExampleIOSApp\\\"\"\n",
                "ExampleIOSApp",
                "dev.orbit.examples.exampleiosapp"
            ),
            None
        );
        assert_eq!(
            filter_simulator_log_line(
                "Traceback (most recent call last):\n",
                "ExampleIOSApp",
                "dev.orbit.examples.exampleiosapp"
            ),
            None
        );
    }

    #[test]
    fn simulator_log_stream_ready_matches_idb_preamble() {
        assert!(super::simulator_log_stream_ready(
            "Filtering the log data using \"process BEGINSWITH[cd] \\\"ExampleIOSApp\\\"\"\n"
        ));
        assert!(super::simulator_log_stream_ready("Running log until ^C\n"));
        assert!(!super::simulator_log_stream_ready(
            "ExampleIOSApp launched\n"
        ));
    }
}
