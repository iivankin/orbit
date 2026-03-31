use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

pub(crate) struct SimulatorAppLogStream {
    child: Child,
    stdout_forwarder: Option<JoinHandle<()>>,
    stderr_forwarder: Option<JoinHandle<()>>,
}

impl SimulatorAppLogStream {
    pub(crate) fn start(udid: &str, process_name: &str) -> Result<Self> {
        let mut command = Command::new("idb");
        command.args(["log", "--udid", udid, "--", "--process", process_name]);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start `idb log` for process `{process_name}`"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("`idb log` did not expose stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("`idb log` did not expose stderr"))?;
        Ok(Self {
            child,
            stdout_forwarder: Some(thread::spawn(move || forward_log_stream(stdout, false))),
            stderr_forwarder: Some(thread::spawn(move || forward_log_stream(stderr, true))),
        })
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

impl Drop for SimulatorAppLogStream {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn forward_log_stream<R>(reader: R, stderr: bool)
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
                if stderr {
                    eprint!("{line}");
                    let _ = std::io::stderr().flush();
                } else {
                    print!("{line}");
                    let _ = std::io::stdout().flush();
                }
            }
            Err(_) => break,
        }
    }
}
