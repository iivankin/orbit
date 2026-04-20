use std::ffi::OsStr;
use std::process::Command;

use anyhow::{Context, Result, bail};

pub fn command_output(command: &mut Command) -> Result<String> {
    let debug = debug_command(command);
    let output = command
        .output()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    if !output.status.success() {
        bail!(
            "`{debug}` failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).context("command produced non UTF-8 output")
}

pub fn command_output_allow_failure(command: &mut Command) -> Result<(bool, String, String)> {
    let debug = debug_command(command);
    let output = command
        .output()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

pub fn run_command_capture(command: &mut Command) -> Result<(String, String)> {
    let debug = debug_command(command);
    let output = command
        .output()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        bail!(
            "`{debug}` failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            stdout,
            stderr
        );
    }
    Ok((stdout, stderr))
}

pub fn combine_command_output(stdout: &str, stderr: &str) -> String {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_owned(),
        (true, false) => stderr.to_owned(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

pub fn run_command(command: &mut Command) -> Result<()> {
    let debug = debug_command(command);
    if std::env::var_os("ORBI_PRINT_COMMANDS").is_some() {
        eprintln!("{debug}");
    }
    let status = command
        .status()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    if !status.success() {
        bail!("`{debug}` failed with {status}");
    }
    Ok(())
}

pub fn debug_command(command: &Command) -> String {
    let program = command.get_program().to_string_lossy();
    let args = command
        .get_args()
        .map(shell_escape)
        .collect::<Vec<_>>()
        .join(" ");
    if args.is_empty() {
        program.into_owned()
    } else {
        format!("{program} {args}")
    }
}

pub fn os_to_string(value: &OsStr) -> String {
    shell_escape(value)
}

pub fn shell_escape(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    if text.is_empty() {
        return "''".to_owned();
    }
    if text
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "/._-=".contains(character))
    {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}
