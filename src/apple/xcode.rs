use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use plist::Value as PlistValue;

use crate::context::AppContext;
use crate::util::prompt_select;

#[path = "xcode/selection.rs"]
mod selection;

#[cfg(test)]
use self::selection::{
    compare_versions, discover_xcodes_under, resolve_requested_xcode_in_roots, version_matches,
};

#[derive(Debug, Clone)]
pub struct SelectedXcode {
    pub version: String,
    pub build_version: String,
    pub app_path: PathBuf,
    pub developer_dir: PathBuf,
}

impl SelectedXcode {
    pub fn simulator_app_path(&self) -> PathBuf {
        self.developer_dir
            .join("Applications")
            .join("Simulator.app")
    }

    pub fn configure_command(&self, command: &mut Command) {
        command.env("DEVELOPER_DIR", &self.developer_dir);
    }

    pub fn display_name(&self) -> String {
        format!("Xcode {} ({})", self.version, self.build_version)
    }
}

pub fn validate_requested_xcode_version(version: &str) -> Result<()> {
    selection::validate_requested_xcode_version(version)
}

pub fn resolve_requested_xcode(version: Option<&str>) -> Result<Option<SelectedXcode>> {
    selection::resolve_requested_xcode(version)
}

pub fn resolve_requested_xcode_for_app(
    app: &AppContext,
    version: Option<&str>,
) -> Result<Option<SelectedXcode>> {
    selection::resolve_requested_xcode_for_app(app, version)
}

pub fn resolve_requested_xcode_with_mode(
    version: Option<&str>,
    interactive: bool,
) -> Result<Option<SelectedXcode>> {
    selection::resolve_requested_xcode_with_mode(version, interactive)
}

pub fn xcrun_command(selected_xcode: Option<&SelectedXcode>) -> Command {
    configured_xcode_command("xcrun", selected_xcode)
}

pub fn xcodebuild_command(selected_xcode: Option<&SelectedXcode>) -> Command {
    configured_xcode_command("xcodebuild", selected_xcode)
}

fn configured_xcode_command(program: &str, selected_xcode: Option<&SelectedXcode>) -> Command {
    let mut command = Command::new(program);
    if let Some(selected_xcode) = selected_xcode {
        selected_xcode.configure_command(&mut command);
    }
    command
}

pub fn open_simulator_command(selected_xcode: Option<&SelectedXcode>, udid: &str) -> Command {
    let mut command = Command::new("open");
    command.arg("-a");
    if let Some(selected_xcode) = selected_xcode {
        command.arg(selected_xcode.simulator_app_path());
    } else {
        command.arg("Simulator");
    }
    command.args(["--args", "-CurrentDeviceUDID", udid]);
    command
}

pub fn developer_dir_path(selected_xcode: Option<&SelectedXcode>) -> Result<PathBuf> {
    if let Some(selected_xcode) = selected_xcode {
        return Ok(selected_xcode.developer_dir.clone());
    }

    if let Some(path) = std::env::var_os("DEVELOPER_DIR") {
        return Ok(PathBuf::from(path));
    }

    let output = Command::new("xcode-select")
        .args(["-p"])
        .output()
        .context("failed to execute `xcode-select -p`")?;
    if !output.status.success() {
        bail!(
            "`xcode-select -p` failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let developer_dir = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if developer_dir.is_empty() {
        bail!("`xcode-select -p` returned an empty developer directory");
    }
    Ok(PathBuf::from(developer_dir))
}

pub fn lldb_path(selected_xcode: Option<&SelectedXcode>) -> Result<PathBuf> {
    required_developer_dir_entry(selected_xcode, &["usr", "bin", "lldb"], "LLDB")
}

pub fn log_redirect_dylib_path(selected_xcode: Option<&SelectedXcode>) -> Result<PathBuf> {
    required_developer_dir_entry(
        selected_xcode,
        &["usr", "lib", "libLogRedirect.dylib"],
        "Xcode log redirect shim",
    )
}

fn required_developer_dir_entry(
    selected_xcode: Option<&SelectedXcode>,
    relative_components: &[&str],
    description: &str,
) -> Result<PathBuf> {
    let mut path = developer_dir_path(selected_xcode)?;
    for component in relative_components {
        path.push(component);
    }
    if !path.exists() {
        bail!("Orbi could not find {description} at {}", path.display());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use plist::Value as PlistValue;

    use super::{
        SelectedXcode, compare_versions, developer_dir_path, discover_xcodes_under, lldb_path,
        log_redirect_dylib_path, open_simulator_command, resolve_requested_xcode_in_roots,
        version_matches,
    };

    fn write_fake_xcode(root: &Path, name: &str, version: &str, build: &str) -> PathBuf {
        let app_root = root.join(name);
        let contents = app_root.join("Contents");
        let developer = contents.join("Developer");
        fs::create_dir_all(&developer).unwrap();

        let mut info = plist::Dictionary::new();
        info.insert(
            "CFBundleIdentifier".to_owned(),
            PlistValue::String("com.apple.dt.Xcode".to_owned()),
        );
        info.insert(
            "CFBundleShortVersionString".to_owned(),
            PlistValue::String(version.to_owned()),
        );
        info.insert(
            "ProductBuildVersion".to_owned(),
            PlistValue::String(build.to_owned()),
        );
        PlistValue::Dictionary(info)
            .to_file_xml(contents.join("Info.plist"))
            .unwrap();
        app_root
    }

    #[test]
    fn version_prefix_matching_is_supported() {
        assert!(version_matches("26", "26.4").unwrap());
        assert!(version_matches("26.4", "26.4.1").unwrap());
        assert!(!version_matches("26.3", "26.4").unwrap());
    }

    #[test]
    fn version_sorting_prefers_newer_xcodes() {
        assert!(compare_versions("26.4", "26.3").is_gt());
        assert!(compare_versions("26.4.1", "26.4").is_gt());
        assert!(compare_versions("26.4", "26.4.0").is_eq());
    }

    #[test]
    fn resolve_requested_xcode_uses_explicit_search_roots() {
        let temp = tempfile::tempdir().unwrap();
        write_fake_xcode(temp.path(), "Xcode-26.4.app", "26.4", "17E192");

        let selected =
            resolve_requested_xcode_in_roots(Some("26.4"), &[temp.path().to_path_buf()], false)
                .unwrap()
                .unwrap();

        assert_eq!(selected.version, "26.4");
        assert_eq!(selected.build_version, "17E192");
        assert!(selected.developer_dir.ends_with("Contents/Developer"));
    }

    #[test]
    fn discover_xcodes_ignores_non_xcode_bundles() {
        let temp = tempfile::tempdir().unwrap();
        write_fake_xcode(temp.path(), "Xcode-26.4.app", "26.4", "17E192");
        let safari = temp.path().join("Safari.app");
        fs::create_dir_all(safari.join("Contents")).unwrap();
        let mut info = plist::Dictionary::new();
        info.insert(
            "CFBundleIdentifier".to_owned(),
            PlistValue::String("com.apple.Safari".to_owned()),
        );
        PlistValue::Dictionary(info)
            .to_file_xml(safari.join("Contents/Info.plist"))
            .unwrap();

        let mut discovered = BTreeMap::new();
        discover_xcodes_under(temp.path(), &mut discovered).unwrap();

        assert_eq!(discovered.len(), 1);
    }

    #[test]
    fn missing_xcode_error_mentions_manual_install_when_not_installed() {
        let error = resolve_requested_xcode_in_roots(
            Some("26.4"),
            &[PathBuf::from("/definitely-missing")],
            false,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("Install the requested Xcode.app manually"));
    }

    #[test]
    fn open_simulator_command_targets_selected_xcode_bundle() {
        let selected = SelectedXcode {
            version: "26.4".to_owned(),
            build_version: "17E192".to_owned(),
            app_path: PathBuf::from("/Applications/Xcode-26.4.app"),
            developer_dir: PathBuf::from("/Applications/Xcode-26.4.app/Contents/Developer"),
        };

        let command = open_simulator_command(Some(&selected), "SIM-UDID");
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            arguments,
            vec![
                "-a".to_owned(),
                "/Applications/Xcode-26.4.app/Contents/Developer/Applications/Simulator.app"
                    .to_owned(),
                "--args".to_owned(),
                "-CurrentDeviceUDID".to_owned(),
                "SIM-UDID".to_owned(),
            ]
        );
    }

    #[test]
    fn developer_dir_path_prefers_environment_override() {
        let temp = tempfile::tempdir().unwrap();
        let developer_dir = temp
            .path()
            .join("FakeXcode.app")
            .join("Contents")
            .join("Developer");
        fs::create_dir_all(&developer_dir).unwrap();

        unsafe {
            std::env::set_var("DEVELOPER_DIR", &developer_dir);
        }
        let resolved = developer_dir_path(None).unwrap();
        unsafe {
            std::env::remove_var("DEVELOPER_DIR");
        }

        assert_eq!(resolved, developer_dir);
    }

    #[test]
    fn lldb_and_log_redirect_paths_use_selected_xcode_without_touching_host_state() {
        let temp = tempfile::tempdir().unwrap();
        let developer_dir = temp
            .path()
            .join("Xcode.app")
            .join("Contents")
            .join("Developer");
        let lldb = developer_dir.join("usr").join("bin").join("lldb");
        let log_redirect = developer_dir
            .join("usr")
            .join("lib")
            .join("libLogRedirect.dylib");
        fs::create_dir_all(lldb.parent().unwrap()).unwrap();
        fs::create_dir_all(log_redirect.parent().unwrap()).unwrap();
        fs::write(&lldb, b"").unwrap();
        fs::write(&log_redirect, b"").unwrap();

        let selected = SelectedXcode {
            version: "26.4".to_owned(),
            build_version: "17E192".to_owned(),
            app_path: temp.path().join("Xcode.app"),
            developer_dir,
        };

        assert_eq!(lldb_path(Some(&selected)).unwrap(), lldb);
        assert_eq!(
            log_redirect_dylib_path(Some(&selected)).unwrap(),
            log_redirect
        );
    }
}
