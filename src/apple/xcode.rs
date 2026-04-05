use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use plist::Value as PlistValue;
use reqwest::blocking::Client as HttpClient;
use reqwest::header::{HeaderMap, USER_AGENT};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::apple::developer_services::DeveloperServicesClient;
use crate::apple::xar_stream;
use crate::context::AppContext;
use crate::util::{
    CliDownloadProgress, CliSpinner, ensure_dir, ensure_parent_dir, prompt_select, run_command,
};

#[path = "xcode/install.rs"]
mod install;
#[path = "xcode/selection.rs"]
mod selection;

#[cfg(test)]
use self::install::matching_downloadable_xcodes;
use self::install::{fetch_downloadable_xcodes, install_requested_xcode};
use self::selection::{
    compare_versions, load_xcode_bundle, preferred_xcode_install_root, version_matches,
};
#[cfg(test)]
use self::selection::{
    configured_xcode_install_root, discover_xcodes_under, resolve_requested_xcode_in_roots,
};

const XCODE_RELEASES_INDEX_URL: &str = "https://xcodereleases.com/data.json";
const XCODE_RELEASES_USER_AGENT: &str = concat!("Orbit/", env!("CARGO_PKG_VERSION"));
const XCODE_DOWNLOAD_RETRY_ATTEMPTS: usize = 3;
const XCODE_DOWNLOAD_RETRY_DELAY: Duration = Duration::from_millis(750);

#[derive(Debug, Clone)]
struct DownloadableXcode {
    version: String,
    build_version: String,
    variant_label: String,
    variant_rank: u8,
    archive_url: String,
    archive_filename: String,
    remote_path: String,
}

impl DownloadableXcode {
    fn display_name(&self) -> String {
        format!("Xcode {} ({})", self.version, self.variant_label)
    }

    fn install_bundle_name(&self) -> String {
        format!("Xcode-{}.app", self.version)
    }
}

#[derive(Debug, Deserialize)]
struct XcodeReleasesEntry {
    name: String,
    version: XcodeReleasesVersion,
    #[serde(default)]
    links: XcodeReleasesLinks,
}

#[derive(Debug, Deserialize)]
struct XcodeReleasesVersion {
    number: String,
    build: Option<String>,
    #[serde(default)]
    release: XcodeReleasesState,
}

#[derive(Debug, Default, Deserialize)]
struct XcodeReleasesState {
    #[serde(default)]
    release: bool,
    #[serde(default)]
    gm: bool,
}

#[derive(Debug, Default, Deserialize)]
struct XcodeReleasesLinks {
    download: Option<XcodeReleasesDownload>,
}

#[derive(Debug, Deserialize)]
struct XcodeReleasesDownload {
    url: String,
    #[serde(default)]
    architectures: Vec<String>,
}

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
        bail!("Orbit could not find {description} at {}", path.display());
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
        SelectedXcode, XcodeReleasesEntry, compare_versions, configured_xcode_install_root,
        developer_dir_path, discover_xcodes_under, lldb_path, log_redirect_dylib_path,
        matching_downloadable_xcodes, open_simulator_command, resolve_requested_xcode_in_roots,
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
    fn missing_xcode_error_mentions_native_install_when_not_installed() {
        let error = resolve_requested_xcode_in_roots(
            Some("26.4"),
            &[PathBuf::from("/definitely-missing")],
            false,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("Orbit can download and install"));
    }

    #[test]
    fn matching_downloadable_xcodes_prefers_exact_stable_release_variants() {
        let entries: Vec<XcodeReleasesEntry> = serde_json::from_str(
            r#"
            [
              {
                "name": "Xcode (Apple Silicon)",
                "version": {
                  "number": "26.4",
                  "build": "17E192",
                  "release": {"release": true}
                },
                "links": {
                  "download": {
                    "url": "https://download.developer.apple.com/Developer_Tools/Xcode_26.4/Xcode_26.4_Apple_silicon.xip",
                    "architectures": ["arm64"]
                  }
                }
              },
              {
                "name": "Xcode",
                "version": {
                  "number": "26.4",
                  "build": "17E192",
                  "release": {"release": true}
                },
                "links": {
                  "download": {
                    "url": "https://download.developer.apple.com/Developer_Tools/Xcode_26.4/Xcode_26.4_Universal.xip",
                    "architectures": ["arm64", "x86_64"]
                  }
                }
              },
              {
                "name": "Xcode",
                "version": {
                  "number": "26.4",
                  "build": "17E5179g",
                  "release": {"beta": 3}
                },
                "links": {
                  "download": {
                    "url": "https://download.developer.apple.com/Developer_Tools/Xcode_26.4_beta_3/Xcode_26.4_beta_3_Universal.xip",
                    "architectures": ["arm64", "x86_64"]
                  }
                }
              }
            ]
            "#,
        )
        .unwrap();

        let candidates = matching_downloadable_xcodes("26.4", &entries).unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.build_version == "17E192")
        );
    }

    #[test]
    fn matching_downloadable_xcodes_uses_latest_stable_prefix_match() {
        let entries: Vec<XcodeReleasesEntry> = serde_json::from_str(
            r#"
            [
              {
                "name": "Xcode",
                "version": {
                  "number": "26.3",
                  "build": "17D5044a",
                  "release": {"release": true}
                },
                "links": {
                  "download": {
                    "url": "https://download.developer.apple.com/Developer_Tools/Xcode_26.3/Xcode_26.3_Universal.xip",
                    "architectures": ["arm64", "x86_64"]
                  }
                }
              },
              {
                "name": "Xcode",
                "version": {
                  "number": "26.4",
                  "build": "17E192",
                  "release": {"release": true}
                },
                "links": {
                  "download": {
                    "url": "https://download.developer.apple.com/Developer_Tools/Xcode_26.4/Xcode_26.4_Universal.xip",
                    "architectures": ["arm64", "x86_64"]
                  }
                }
              }
            ]
            "#,
        )
        .unwrap();

        let candidates = matching_downloadable_xcodes("26", &entries).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].version, "26.4");
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

    #[test]
    fn configured_install_root_uses_single_override_search_root() {
        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("Xcodes");
        fs::create_dir_all(&install_root).unwrap();
        unsafe {
            std::env::set_var("ORBIT_XCODE_SEARCH_ROOTS", &install_root);
        }
        let resolved = configured_xcode_install_root(std::slice::from_ref(&install_root));
        unsafe {
            std::env::remove_var("ORBIT_XCODE_SEARCH_ROOTS");
        }

        assert_eq!(resolved, Some(install_root));
    }
}
