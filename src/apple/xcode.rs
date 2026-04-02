use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use plist::Value as PlistValue;

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

    pub fn lldb_path(&self) -> PathBuf {
        self.developer_dir.join("usr").join("bin").join("lldb")
    }

    pub fn log_redirect_dylib_path(&self) -> PathBuf {
        self.developer_dir
            .join("usr")
            .join("lib")
            .join("libLogRedirect.dylib")
    }

    pub fn configure_command(&self, command: &mut Command) {
        command.env("DEVELOPER_DIR", &self.developer_dir);
    }

    pub fn display_name(&self) -> String {
        format!("Xcode {} ({})", self.version, self.build_version)
    }
}

pub fn validate_requested_xcode_version(version: &str) -> Result<()> {
    parse_version_components(version)?;
    Ok(())
}

pub fn resolve_requested_xcode(version: Option<&str>) -> Result<Option<SelectedXcode>> {
    resolve_requested_xcode_in_roots(version, &installed_xcode_search_roots())
}

fn resolve_requested_xcode_in_roots(
    version: Option<&str>,
    roots: &[PathBuf],
) -> Result<Option<SelectedXcode>> {
    let Some(version) = version.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    validate_requested_xcode_version(version)?;

    let installed = installed_xcodes_in_roots(roots)?;
    let exact_matches = installed
        .iter()
        .filter(|candidate| candidate.version == version)
        .cloned()
        .collect::<Vec<_>>();
    if exact_matches.len() == 1 {
        return Ok(exact_matches.into_iter().next());
    }
    if exact_matches.len() > 1 {
        return Err(ambiguous_xcode_error(version, &exact_matches));
    }

    let prefix_matches = installed
        .iter()
        .filter(|candidate| version_matches(version, &candidate.version).unwrap_or(false))
        .cloned()
        .collect::<Vec<_>>();
    match prefix_matches.len() {
        1 => Ok(prefix_matches.into_iter().next()),
        0 => Err(missing_xcode_error(version, &installed)),
        _ => Err(ambiguous_xcode_error(version, &prefix_matches)),
    }
}

pub fn xcrun_command(selected_xcode: Option<&SelectedXcode>) -> Command {
    let mut command = Command::new("xcrun");
    if let Some(selected_xcode) = selected_xcode {
        selected_xcode.configure_command(&mut command);
    }
    command
}

pub fn xcodebuild_command(selected_xcode: Option<&SelectedXcode>) -> Command {
    let mut command = Command::new("xcodebuild");
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
    let path = match selected_xcode {
        Some(selected_xcode) => selected_xcode.lldb_path(),
        None => developer_dir_path(None)?
            .join("usr")
            .join("bin")
            .join("lldb"),
    };
    if !path.exists() {
        bail!("Orbit could not find LLDB at {}", path.display());
    }
    Ok(path)
}

pub fn log_redirect_dylib_path(selected_xcode: Option<&SelectedXcode>) -> Result<PathBuf> {
    let path = match selected_xcode {
        Some(selected_xcode) => selected_xcode.log_redirect_dylib_path(),
        None => developer_dir_path(None)?
            .join("usr")
            .join("lib")
            .join("libLogRedirect.dylib"),
    };
    if !path.exists() {
        bail!(
            "Orbit could not find Xcode log redirect shim at {}",
            path.display()
        );
    }
    Ok(path)
}

fn installed_xcodes_in_roots(roots: &[PathBuf]) -> Result<Vec<SelectedXcode>> {
    let mut discovered = BTreeMap::new();
    for root in roots {
        discover_xcodes_under(root, &mut discovered)?;
    }

    let mut xcodes = discovered.into_values().collect::<Vec<_>>();
    xcodes.sort_by(|left, right| {
        compare_versions(&right.version, &left.version)
            .then_with(|| right.build_version.cmp(&left.build_version))
            .then_with(|| left.app_path.cmp(&right.app_path))
    });
    Ok(xcodes)
}

fn discover_xcodes_under(
    root: &Path,
    discovered: &mut BTreeMap<PathBuf, SelectedXcode>,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    if root
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("app"))
    {
        if let Some(xcode) = load_xcode_bundle(root)? {
            discovered.insert(xcode.app_path.clone(), xcode);
        }
        return Ok(());
    }

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(_) => continue,
        };
        if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("app"))
            && let Some(xcode) = load_xcode_bundle(&path)?
        {
            discovered.insert(xcode.app_path.clone(), xcode);
        }
    }
    Ok(())
}

fn load_xcode_bundle(path: &Path) -> Result<Option<SelectedXcode>> {
    let info_path = path.join("Contents").join("Info.plist");
    if !info_path.exists() {
        return Ok(None);
    }

    let plist = PlistValue::from_file(&info_path)
        .with_context(|| format!("failed to read {}", info_path.display()))?;
    let dict = match plist.as_dictionary() {
        Some(dict) => dict,
        None => return Ok(None),
    };
    if dict
        .get("CFBundleIdentifier")
        .and_then(PlistValue::as_string)
        != Some("com.apple.dt.Xcode")
    {
        return Ok(None);
    }

    let version = dict
        .get("CFBundleShortVersionString")
        .and_then(PlistValue::as_string)
        .context("Xcode bundle was missing CFBundleShortVersionString")?
        .trim()
        .to_owned();
    validate_requested_xcode_version(&version)?;

    let build_version = dict
        .get("ProductBuildVersion")
        .or_else(|| dict.get("CFBundleVersion"))
        .and_then(PlistValue::as_string)
        .context("Xcode bundle was missing ProductBuildVersion")?
        .trim()
        .to_owned();
    let developer_dir = path.join("Contents").join("Developer");
    if !developer_dir.exists() {
        bail!(
            "Xcode bundle at {} did not contain Contents/Developer",
            path.display()
        );
    }

    Ok(Some(SelectedXcode {
        version,
        build_version,
        app_path: path.to_path_buf(),
        developer_dir,
    }))
}

fn installed_xcode_search_roots() -> Vec<PathBuf> {
    if let Some(override_paths) = std::env::var_os("ORBIT_XCODE_SEARCH_ROOTS") {
        let paths = std::env::split_paths(&override_paths).collect::<Vec<_>>();
        if !paths.is_empty() {
            return paths;
        }
    }

    let mut roots = Vec::new();
    roots.push(PathBuf::from("/Applications"));
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }

    if let Some(current) = std::env::var_os("DEVELOPER_DIR")
        .map(PathBuf::from)
        .and_then(|path| path.parent().and_then(Path::parent).map(Path::to_path_buf))
    {
        roots.push(current);
    }

    dedupe_paths(roots)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut ordered = Vec::new();
    for path in paths {
        if !ordered.contains(&path) {
            ordered.push(path);
        }
    }
    ordered
}

fn version_matches(requested: &str, candidate: &str) -> Result<bool> {
    let requested = parse_version_components(requested)?;
    let candidate = parse_version_components(candidate)?;
    Ok(requested.len() <= candidate.len()
        && requested
            .iter()
            .zip(candidate.iter())
            .all(|(left, right)| left == right))
}

fn parse_version_components(version: &str) -> Result<Vec<u64>> {
    let components = version.split('.').map(str::trim).collect::<Vec<_>>();
    if components.is_empty() || components.len() > 3 {
        bail!("`xcode` must use a dotted numeric version like `26`, `26.4`, or `26.4.1`");
    }

    let mut parsed = Vec::with_capacity(components.len());
    for component in components {
        if component.is_empty()
            || !component
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            bail!("`xcode` must use a dotted numeric version like `26`, `26.4`, or `26.4.1`");
        }
        parsed.push(component.parse()?);
    }
    Ok(parsed)
}

fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let left = parse_version_components(left).unwrap_or_default();
    let right = parse_version_components(right).unwrap_or_default();
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left = left.get(index).copied().unwrap_or(0);
        let right = right.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn missing_xcode_error(version: &str, installed: &[SelectedXcode]) -> anyhow::Error {
    if installed.is_empty() {
        return anyhow::anyhow!(
            "manifest requests Xcode `{version}`, but Orbit could not find any installed Xcode.app bundles"
        );
    }

    anyhow::anyhow!(
        "manifest requests Xcode `{version}`, but no installed Xcode matched it. Installed: {}",
        installed
            .iter()
            .map(|candidate| format!(
                "{} at {}",
                candidate.display_name(),
                candidate.app_path.display()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn ambiguous_xcode_error(version: &str, matches: &[SelectedXcode]) -> anyhow::Error {
    anyhow::anyhow!(
        "manifest requests Xcode `{version}`, but that matched multiple installed Xcodes: {}",
        matches
            .iter()
            .map(|candidate| format!(
                "{} at {}",
                candidate.display_name(),
                candidate.app_path.display()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
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

        let selected = resolve_requested_xcode_in_roots(Some("26.4"), &[temp.path().to_path_buf()])
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
    fn selected_xcode_exposes_runtime_tool_paths() {
        let selected = SelectedXcode {
            version: "26.4".to_owned(),
            build_version: "17E192".to_owned(),
            app_path: PathBuf::from("/Applications/Xcode-26.4.app"),
            developer_dir: PathBuf::from("/Applications/Xcode-26.4.app/Contents/Developer"),
        };

        assert_eq!(
            selected.lldb_path(),
            PathBuf::from("/Applications/Xcode-26.4.app/Contents/Developer/usr/bin/lldb")
        );
        assert_eq!(
            selected.log_redirect_dylib_path(),
            PathBuf::from(
                "/Applications/Xcode-26.4.app/Contents/Developer/usr/lib/libLogRedirect.dylib"
            )
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
