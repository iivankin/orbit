use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn orbi_bin() -> &'static str {
    env!("CARGO_BIN_EXE_orbi")
}

pub fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

pub fn create_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    home
}

pub fn orbi_data_dir(home: &Path) -> PathBuf {
    home.join(".orbi-data")
}

pub fn orbi_cache_dir(home: &Path) -> PathBuf {
    home.join(".orbi-cache")
}

pub fn base_command(workspace: &Path, home: &Path, mock_bin: &Path, log_path: &Path) -> Command {
    let mut command = Command::new(orbi_bin());
    apply_test_environment(&mut command, home, mock_bin, log_path);
    command.current_dir(workspace);
    command
}

pub fn sourcekit_lsp_command(
    workspace: &Path,
    home: &Path,
    mock_bin: &Path,
    log_path: &Path,
) -> Command {
    let mut command = Command::new("sourcekit-lsp");
    apply_test_environment(&mut command, home, mock_bin, log_path);
    command.current_dir(workspace);
    command
}

pub fn run_and_capture(command: &mut Command) -> Output {
    command.output().unwrap()
}

pub fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

pub fn clear_log(path: &Path) {
    fs::write(path, b"").unwrap();
}

pub fn latest_receipt_path(workspace: &Path) -> PathBuf {
    let receipt_dir = workspace.join(".orbi/receipts");
    let mut receipts = fs::read_dir(&receipt_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    receipts.sort();
    receipts.pop().unwrap()
}

fn apply_test_environment(command: &mut Command, home: &Path, mock_bin: &Path, log_path: &Path) {
    command.env("HOME", home);
    command.env("ORBI_DATA_DIR", orbi_data_dir(home));
    command.env("ORBI_CACHE_DIR", orbi_cache_dir(home));
    command.env(
        "PATH",
        format!(
            "{}:{}",
            mock_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    command.env("MOCK_LOG", log_path);
}
