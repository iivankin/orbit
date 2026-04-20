use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::util::{combine_command_output, command_output_allow_failure, run_command};

// PyPI still publishes 1.1.7 as the latest `fb-idb` release, so keep the
// auto-bootstrap pinned to an exact version for reproducibility.
const FB_IDB_VERSION: &str = "1.1.7";

pub(crate) fn requirement_message() -> String {
    format!(
        "Orbi UI tooling for iOS simulators requires `idb` and `idb_companion`.\n\n\
Orbi first looks in PATH, Homebrew-managed `idb-companion`, and common user Python bin directories.\n\
If a tool is still missing, Orbi will try to install it automatically with:\n  \
brew tap facebook/fb\n  \
brew install idb-companion\n  \
python3 -m pip install --user fb-idb=={FB_IDB_VERSION}\n\n\
If setup still fails, make sure Homebrew and `python3` are installed and available on PATH."
    )
}

pub(crate) fn ensure_tooling_available() -> Result<()> {
    if let Some(locations) = locate_tooling()? {
        prepend_tool_paths(&locations)?;
        return Ok(());
    }

    let mut install_errors = Vec::new();

    if locate_idb_client()?.is_none()
        && let Err(error) = install_idb_client()
    {
        install_errors.push(format!("`idb`: {error:#}"));
    }

    if locate_idb_companion()?.is_none()
        && let Err(error) = install_idb_companion()
    {
        install_errors.push(format!("`idb_companion`: {error:#}"));
    }

    if let Some(locations) = locate_tooling()? {
        prepend_tool_paths(&locations)?;
        return Ok(());
    }

    let missing = missing_tool_names()?;
    let mut message = format!(
        "{}\nMissing: {}.",
        requirement_message(),
        missing
            .into_iter()
            .map(|entry| format!("`{entry}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !install_errors.is_empty() {
        message.push_str("\nAutomatic setup errors:\n");
        for error in install_errors {
            message.push_str(&format!("  - {error}\n"));
        }
    }
    bail!(message.trim_end().to_owned())
}

struct ToolLocations {
    idb: PathBuf,
    idb_companion: PathBuf,
}

fn locate_tooling() -> Result<Option<ToolLocations>> {
    let idb = locate_idb_client()?;
    let idb_companion = locate_idb_companion()?;
    Ok(match (idb, idb_companion) {
        (Some(idb), Some(idb_companion)) => Some(ToolLocations { idb, idb_companion }),
        _ => None,
    })
}

fn missing_tool_names() -> Result<Vec<&'static str>> {
    let mut missing = Vec::new();
    if locate_idb_client()?.is_none() {
        missing.push("idb");
    }
    if locate_idb_companion()?.is_none() {
        missing.push("idb_companion");
    }
    Ok(missing)
}

fn locate_idb_client() -> Result<Option<PathBuf>> {
    if let Some(path) = find_executable_on_path("idb") {
        return Ok(Some(path));
    }
    Ok(find_executable_in_dirs("idb", user_tool_bin_dirs()?))
}

fn locate_idb_companion() -> Result<Option<PathBuf>> {
    if let Some(path) = find_executable_on_path("idb_companion") {
        return Ok(Some(path));
    }

    let Some(brew) = find_executable_on_path("brew") else {
        return Ok(None);
    };
    let mut command = Command::new(brew);
    command.args(["--prefix", "idb-companion"]);
    let (success, stdout, _stderr) = command_output_allow_failure(&mut command)?;
    if !success {
        return Ok(None);
    }

    let prefix = stdout.trim();
    if prefix.is_empty() {
        return Ok(None);
    }

    let path = PathBuf::from(prefix).join("bin").join("idb_companion");
    if path.is_file() {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

fn install_idb_client() -> Result<()> {
    let python3 = find_executable_on_path("python3")
        .context("`python3` is required to install `fb-idb` automatically")?;
    eprintln!("installing missing `idb` via python3 -m pip --user");
    run_pip_install_user(&python3)
        .with_context(|| format!("failed to install `fb-idb=={FB_IDB_VERSION}`"))?;
    Ok(())
}

fn run_pip_install_user(python3: &Path) -> Result<()> {
    let spec = format!("fb-idb=={FB_IDB_VERSION}");
    let attempt_install = |python3: &Path| -> Result<(bool, String)> {
        let mut command = Command::new(python3);
        command.args([
            "-m",
            "pip",
            "install",
            "--user",
            "--disable-pip-version-check",
            "--no-input",
            &spec,
        ]);
        let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
        Ok((success, combine_command_output(&stdout, &stderr)))
    };

    let (success, output) = attempt_install(python3)?;
    if success {
        return Ok(());
    }
    if !pip_missing(&output) {
        bail!("{output}");
    }

    let mut ensurepip = Command::new(python3);
    ensurepip.args(["-m", "ensurepip", "--upgrade", "--user"]);
    run_command(&mut ensurepip).context("`python3 -m ensurepip --upgrade --user` failed")?;

    let (success, output) = attempt_install(python3)?;
    if success { Ok(()) } else { bail!("{output}") }
}

fn pip_missing(output: &str) -> bool {
    output.contains("No module named pip") || output.contains("No module named 'pip'")
}

fn install_idb_companion() -> Result<()> {
    let brew = find_executable_on_path("brew")
        .context("`brew` is required to install `idb_companion` automatically")?;
    eprintln!("installing missing `idb_companion` via Homebrew");

    let mut tap = Command::new(&brew);
    tap.args(["tap", "facebook/fb"]);
    run_command(&mut tap).context("`brew tap facebook/fb` failed")?;

    let mut install = Command::new(&brew);
    install.args(["install", "idb-companion"]);
    run_command(&mut install).context("`brew install idb-companion` failed")?;
    Ok(())
}

fn prepend_tool_paths(locations: &ToolLocations) -> Result<()> {
    let mut entries = Vec::new();
    for path in [&locations.idb, &locations.idb_companion] {
        let parent = path
            .parent()
            .with_context(|| format!("tool path `{}` had no parent directory", path.display()))?;
        entries.push(parent.to_path_buf());
    }
    prepend_path_entries(&entries)
}

fn prepend_path_entries(entries: &[PathBuf]) -> Result<()> {
    let mut merged = Vec::new();
    merged.extend(entries.iter().cloned());
    if let Some(current) = env::var_os("PATH") {
        merged.extend(env::split_paths(&current));
    }

    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for entry in merged {
        if seen.insert(entry.clone()) {
            unique.push(entry);
        }
    }

    let joined = env::join_paths(unique).context("failed to update PATH for idb tooling")?;
    // SAFETY: Orbi updates PATH before it spawns any `idb` subprocesses in this process.
    unsafe {
        env::set_var("PATH", joined);
    }
    Ok(())
}

fn find_executable_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| find_executable_in_dirs(name, env::split_paths(&paths)))
}

fn find_executable_in_dirs(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn user_tool_bin_dirs() -> Result<Vec<PathBuf>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(Vec::new());
    };

    let mut entries = Vec::new();
    let local_bin = home.join(".local").join("bin");
    if local_bin.is_dir() {
        entries.push(local_bin);
    }

    let library_python = home.join("Library").join("Python");
    if library_python.is_dir() {
        let mut version_dirs = fs::read_dir(&library_python)
            .with_context(|| format!("failed to read {}", library_python.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect::<Vec<_>>();
        version_dirs.sort();
        for version_dir in version_dirs {
            let bin_dir = version_dir.join("bin");
            if bin_dir.is_dir() {
                entries.push(bin_dir);
            }
        }
    }

    Ok(entries)
}
