use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use plist::Value as PlistValue;
use reqwest::blocking::Client as HttpClient;
use reqwest::header::USER_AGENT;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::apple::developer_services::DeveloperServicesClient;
use crate::context::AppContext;
use crate::util::{
    CliDownloadProgress, CliSpinner, ensure_dir, ensure_parent_dir, prompt_select, run_command,
};

const XCODE_RELEASES_INDEX_URL: &str = "https://xcodereleases.com/data.json";
const XCODE_RELEASES_USER_AGENT: &str = concat!("Orbit/", env!("CARGO_PKG_VERSION"));

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
    resolve_requested_xcode_with_mode(version, false)
}

pub fn resolve_requested_xcode_for_app(
    app: &AppContext,
    version: Option<&str>,
) -> Result<Option<SelectedXcode>> {
    resolve_requested_xcode_in_roots_with_app(
        version,
        &installed_xcode_search_roots(),
        app.interactive,
        Some(app),
    )
}

pub fn resolve_requested_xcode_with_mode(
    version: Option<&str>,
    interactive: bool,
) -> Result<Option<SelectedXcode>> {
    resolve_requested_xcode_in_roots(version, &installed_xcode_search_roots(), interactive)
}

fn resolve_requested_xcode_in_roots(
    version: Option<&str>,
    roots: &[PathBuf],
    interactive: bool,
) -> Result<Option<SelectedXcode>> {
    resolve_requested_xcode_in_roots_with_app(version, roots, interactive, None)
}

fn resolve_requested_xcode_in_roots_with_app(
    version: Option<&str>,
    roots: &[PathBuf],
    interactive: bool,
    app: Option<&AppContext>,
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
        if interactive {
            return select_installed_xcode(
                &format!(
                    "Manifest requests Xcode `{version}`. Select one of the matching installs"
                ),
                &exact_matches,
            )
            .map(Some);
        }
        return Err(ambiguous_xcode_error(version, &exact_matches));
    }

    let prefix_matches = installed
        .iter()
        .filter(|candidate| version_matches(version, &candidate.version).unwrap_or(false))
        .cloned()
        .collect::<Vec<_>>();
    match prefix_matches.len() {
        1 => Ok(prefix_matches.into_iter().next()),
        0 => resolve_missing_requested_xcode(version, &installed, roots, interactive, app),
        _ => {
            if interactive {
                select_installed_xcode(
                    &format!(
                        "Manifest requests Xcode `{version}`. Multiple installed Xcodes match that version prefix"
                    ),
                    &prefix_matches,
                )
                .map(Some)
            } else {
                Err(ambiguous_xcode_error(version, &prefix_matches))
            }
        }
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

fn resolve_missing_requested_xcode(
    version: &str,
    installed: &[SelectedXcode],
    roots: &[PathBuf],
    interactive: bool,
    app: Option<&AppContext>,
) -> Result<Option<SelectedXcode>> {
    if !interactive {
        return Err(missing_xcode_error(version, installed, None));
    }

    let mut install_lookup_error = None;
    let downloadable = if let Some(_app) = app {
        match fetch_downloadable_xcodes(version) {
            Ok(candidates) => {
                if candidates.is_empty() {
                    install_lookup_error = Some(format!(
                        "Orbit could not find a stable downloadable Xcode release matching `{version}`."
                    ));
                }
                candidates
            }
            Err(error) => {
                install_lookup_error = Some(error.to_string());
                Vec::new()
            }
        }
    } else {
        install_lookup_error = Some(
            "Orbit can only install missing Xcodes while loading a project context.".to_owned(),
        );
        Vec::new()
    };

    if downloadable.is_empty() && installed.is_empty() {
        return Err(missing_xcode_error(
            version,
            installed,
            install_lookup_error.as_deref(),
        ));
    }

    let mut actions = Vec::new();
    let mut labels = Vec::new();
    if !downloadable.is_empty() {
        let install_root = preferred_xcode_install_root(roots)?;
        for (index, candidate) in downloadable.iter().enumerate() {
            actions.push(MissingXcodeAction::Install(index));
            labels.push(format!(
                "Download and install {} into {}",
                candidate.display_name(),
                install_root.display()
            ));
        }
    }
    if !installed.is_empty() {
        actions.push(MissingXcodeAction::SelectInstalled);
        labels.push("Use a different installed Xcode for this run".to_owned());
    }
    actions.push(MissingXcodeAction::Abort);
    labels.push("Abort".to_owned());

    let index = prompt_select(
        &format!(
            "Manifest requests Xcode `{version}`, but it is not installed. What should Orbit do?"
        ),
        &labels,
    )?;

    match actions[index] {
        MissingXcodeAction::Install(candidate_index) => {
            let app = app.context("interactive Xcode installation requires an app context")?;
            install_requested_xcode(app, &downloadable[candidate_index], roots)?;
            resolve_requested_xcode_in_roots(Some(version), roots, false)
        }
        MissingXcodeAction::SelectInstalled => select_installed_xcode(
            &format!(
                "Manifest requests Xcode `{version}`. Select an installed Xcode to use for this run"
            ),
            installed,
        )
        .map(Some),
        MissingXcodeAction::Abort => Err(missing_xcode_error(
            version,
            installed,
            install_lookup_error.as_deref(),
        )),
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum MissingXcodeAction {
    Install(usize),
    SelectInstalled,
    Abort,
}

fn select_installed_xcode(prompt: &str, installed: &[SelectedXcode]) -> Result<SelectedXcode> {
    let labels = installed
        .iter()
        .map(|candidate| {
            format!(
                "{} at {}",
                candidate.display_name(),
                candidate.app_path.display()
            )
        })
        .collect::<Vec<_>>();
    let index = prompt_select(prompt, &labels)?;
    Ok(installed[index].clone())
}

fn fetch_downloadable_xcodes(version: &str) -> Result<Vec<DownloadableXcode>> {
    let response = reqwest::blocking::ClientBuilder::new()
        .brotli(true)
        .deflate(true)
        .gzip(true)
        .build()
        .context("failed to build Xcode releases HTTP client")?
        .get(XCODE_RELEASES_INDEX_URL)
        .header(USER_AGENT, XCODE_RELEASES_USER_AGENT)
        .send()
        .context("failed to fetch Xcode release metadata from xcodereleases.com")?;
    let status = response.status();
    let body = response
        .bytes()
        .context("failed to read Xcode release metadata response body")?;
    if !status.is_success() {
        bail!(
            "failed to fetch Xcode release metadata from xcodereleases.com with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }

    let entries: Vec<XcodeReleasesEntry> = serde_json::from_slice(&body)
        .context("failed to parse Xcode release metadata from xcodereleases.com")?;
    matching_downloadable_xcodes(version, &entries)
}

fn matching_downloadable_xcodes(
    requested_version: &str,
    entries: &[XcodeReleasesEntry],
) -> Result<Vec<DownloadableXcode>> {
    let stable = entries
        .iter()
        .filter(|entry| entry.version.release.release || entry.version.release.gm)
        .filter(|entry| version_matches(requested_version, &entry.version.number).unwrap_or(false))
        .collect::<Vec<_>>();
    if stable.is_empty() {
        return Ok(Vec::new());
    }

    let selected_version = if stable
        .iter()
        .any(|entry| entry.version.number == requested_version)
    {
        requested_version.to_owned()
    } else {
        stable
            .iter()
            .map(|entry| entry.version.number.as_str())
            .max_by(|left, right| compare_versions(left, right))
            .unwrap_or(requested_version)
            .to_owned()
    };

    let mut candidates = stable
        .into_iter()
        .filter(|entry| entry.version.number == selected_version)
        .filter_map(|entry| {
            let build_version = entry.version.build.as_ref()?.trim();
            if build_version.is_empty() {
                return None;
            }
            let download = entry.links.download.as_ref()?;
            let archive_url = download.url.trim();
            if archive_url.is_empty() {
                return None;
            }
            let remote_path = reqwest::Url::parse(archive_url).ok()?.path().to_owned();
            let archive_filename = Path::new(&remote_path)
                .file_name()?
                .to_string_lossy()
                .into_owned();
            let (variant_label, variant_rank) =
                download_variant(entry.name.as_str(), &download.architectures);
            Some(DownloadableXcode {
                version: entry.version.number.clone(),
                build_version: build_version.to_owned(),
                variant_label,
                variant_rank,
                archive_url: archive_url.to_owned(),
                archive_filename,
                remote_path,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.variant_rank
            .cmp(&right.variant_rank)
            .then_with(|| left.variant_label.cmp(&right.variant_label))
            .then_with(|| left.archive_filename.cmp(&right.archive_filename))
    });
    Ok(candidates)
}

fn download_variant(name: &str, architectures: &[String]) -> (String, u8) {
    let architectures = architectures
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let is_apple_silicon =
        architectures.len() == 1 && architectures.first().is_some_and(|value| value == "arm64");
    let is_universal = architectures.iter().any(|value| value == "arm64")
        && architectures.iter().any(|value| value == "x86_64");
    let prefers_apple_silicon = host_prefers_apple_silicon();

    if is_apple_silicon {
        return (
            "Apple Silicon".to_owned(),
            if prefers_apple_silicon { 0 } else { 1 },
        );
    }
    if is_universal {
        return (
            "Universal".to_owned(),
            if prefers_apple_silicon { 1 } else { 0 },
        );
    }
    if name.contains("Apple Silicon") {
        return (
            "Apple Silicon".to_owned(),
            if prefers_apple_silicon { 0 } else { 1 },
        );
    }
    if !architectures.is_empty() {
        return (architectures.join("/"), 2);
    }
    ("Default".to_owned(), 2)
}

fn host_prefers_apple_silicon() -> bool {
    matches!(std::env::consts::ARCH, "aarch64" | "arm64")
}

fn install_requested_xcode(
    app: &AppContext,
    candidate: &DownloadableXcode,
    roots: &[PathBuf],
) -> Result<()> {
    let spinner = CliSpinner::new(format!("Installing {}", candidate.display_name()));
    let result = (|| {
        let install_root = preferred_xcode_install_root(roots)?;
        ensure_dir(&install_root)?;
        let archive_path = download_xcode_archive(app, candidate, &spinner)?;
        install_downloaded_xcode(&archive_path, candidate, &install_root, &spinner)
    })();
    match result {
        Ok(install_path) => {
            spinner.finish_success(format!(
                "Installed {} at {}.",
                candidate.display_name(),
                install_path.display()
            ));
            Ok(())
        }
        Err(error) => {
            spinner.finish_clear();
            Err(error)
        }
    }
}

fn download_xcode_archive(
    app: &AppContext,
    candidate: &DownloadableXcode,
    spinner: &CliSpinner,
) -> Result<PathBuf> {
    let archive_path = app
        .global_paths
        .cache_dir
        .join("xcodes")
        .join("archives")
        .join(format!("{}-{}", candidate.version, candidate.build_version))
        .join(&candidate.archive_filename);
    if archive_path.exists() {
        spinner.set_message(format!("Using cached archive {}", archive_path.display()));
        return Ok(archive_path);
    }

    ensure_parent_dir(&archive_path)?;
    let partial_path = partial_download_path(&archive_path)?;
    if partial_path.exists() {
        fs::remove_file(&partial_path)
            .with_context(|| format!("failed to clear {}", partial_path.display()))?;
    }

    spinner.set_message(format!(
        "Authorizing Apple Developer download for {}",
        candidate.display_name()
    ));
    let mut developer_services = DeveloperServicesClient::authenticate_for_xcode_download(
        app,
        &candidate.version,
        &candidate.build_version,
    )?;
    developer_services.authorize_download_path(&candidate.remote_path)?;

    spinner.set_message(format!("Downloading {}", candidate.archive_filename));
    spinner.suspend(|| {
        stream_download_to_path(
            &developer_services.clone_http_client(),
            &candidate.archive_url,
            &candidate.archive_filename,
            &archive_path,
            &partial_path,
        )
    })?;
    Ok(archive_path)
}

fn partial_download_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("download path was missing a file name")?;
    Ok(path.with_file_name(format!("{file_name}.part")))
}

fn stream_download_to_path(
    client: &HttpClient,
    url: &str,
    label: &str,
    destination: &Path,
    partial_path: &Path,
) -> Result<()> {
    let mut response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to download Xcode archive from {url}"))?;
    if response.url().path().contains("/unauthorized") {
        bail!(
            "Apple redirected the Xcode archive download to an unauthorized page. Re-authenticate your Apple ID and make sure developer downloads are allowed for that account."
        );
    }

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .unwrap_or_else(|_| "<unreadable response body>".to_owned());
        bail!("Xcode archive download failed with {status}: {body}");
    }

    let total = response.content_length();
    let mut progress = CliDownloadProgress::new(label, total);
    let mut file = fs::File::create(partial_path)
        .with_context(|| format!("failed to create {}", partial_path.display()))?;
    let mut buffer = [0u8; 1024 * 1024];
    let mut downloaded = 0u64;
    loop {
        let read = response
            .read(&mut buffer)
            .context("failed while reading the Xcode archive download stream")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .with_context(|| format!("failed to write {}", partial_path.display()))?;
        downloaded += read as u64;
        progress.advance(downloaded);
    }
    file.flush()
        .with_context(|| format!("failed to flush {}", partial_path.display()))?;
    fs::rename(partial_path, destination).with_context(|| {
        format!(
            "failed to move downloaded Xcode archive to {}",
            destination.display()
        )
    })?;
    progress.finish(downloaded, destination);
    Ok(())
}

fn install_downloaded_xcode(
    archive_path: &Path,
    candidate: &DownloadableXcode,
    install_root: &Path,
    spinner: &CliSpinner,
) -> Result<PathBuf> {
    let expansion_root = archive_path
        .parent()
        .context("downloaded Xcode archive did not have a parent directory")?
        .join(format!("expand-{}", candidate.build_version));
    recreate_dir(&expansion_root)?;

    spinner.set_message(format!("Expanding {}", archive_path.display()));
    let mut expand = Command::new("xip");
    expand
        .arg("--expand")
        .arg(archive_path)
        .current_dir(&expansion_root);
    spinner
        .suspend(|| run_command(&mut expand))
        .with_context(|| format!("failed to expand {}", archive_path.display()))?;

    let extracted_app = find_expanded_xcode_app(&expansion_root)?;
    let install_path = install_root.join(candidate.install_bundle_name());
    if install_path.exists() {
        if let Some(existing) = load_xcode_bundle(&install_path)?
            && existing.version == candidate.version
            && existing.build_version == candidate.build_version
        {
            return Ok(install_path);
        }
        bail!(
            "Orbit refused to overwrite the existing Xcode install at {}",
            install_path.display()
        );
    }

    spinner.set_message(format!("Installing {}", candidate.display_name()));
    let mut move_app = Command::new("mv");
    move_app.arg(&extracted_app).arg(&install_path);
    run_command(&mut move_app).with_context(|| {
        format!(
            "failed to move {} into {}",
            extracted_app.display(),
            install_path.display()
        )
    })?;
    let _ = fs::remove_dir_all(&expansion_root);

    let installed = load_xcode_bundle(&install_path)?.with_context(|| {
        format!(
            "expected a valid Xcode bundle at {} after installation",
            install_path.display()
        )
    })?;
    if installed.version != candidate.version || installed.build_version != candidate.build_version
    {
        bail!(
            "installed Xcode metadata at {} did not match the requested release {} ({})",
            install_path.display(),
            candidate.version,
            candidate.build_version
        );
    }
    Ok(install_path)
}

fn find_expanded_xcode_app(root: &Path) -> Result<PathBuf> {
    WalkDir::new(root)
        .max_depth(2)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .find(|path| load_xcode_bundle(path).ok().flatten().is_some())
        .with_context(|| {
            format!(
                "`xip --expand` did not produce a valid Xcode.app bundle under {}",
                root.display()
            )
        })
}

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to clear {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

fn preferred_xcode_install_root(roots: &[PathBuf]) -> Result<PathBuf> {
    if let Some(root) = configured_xcode_install_root(roots) {
        return Ok(root);
    }
    Ok(dirs::home_dir()
        .context("failed to resolve the user home directory for Xcode installs")?
        .join("Applications"))
}

fn configured_xcode_install_root(roots: &[PathBuf]) -> Option<PathBuf> {
    let override_paths = std::env::var_os("ORBIT_XCODE_SEARCH_ROOTS")?;
    let paths = std::env::split_paths(&override_paths).collect::<Vec<_>>();
    if paths.len() != 1 {
        return None;
    }
    let root = paths.into_iter().next()?;
    if root
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("app"))
    {
        return None;
    }
    if roots.contains(&root) {
        Some(root)
    } else {
        None
    }
}

fn missing_xcode_error(
    version: &str,
    installed: &[SelectedXcode],
    install_unavailable_reason: Option<&str>,
) -> anyhow::Error {
    let install_hint = install_unavailable_reason
        .map(|reason| format!(" Orbit could not install it automatically: {reason}"))
        .unwrap_or_else(|| {
            " In interactive runs, Orbit can download and install the requested Xcode for you."
                .to_owned()
        });

    if installed.is_empty() {
        return anyhow::anyhow!(
            "manifest requests Xcode `{version}`, but Orbit could not find any installed Xcode.app bundles.{install_hint}"
        );
    }

    anyhow::anyhow!(
        "manifest requests Xcode `{version}`, but no installed Xcode matched it. Installed: {}.{}",
        installed
            .iter()
            .map(|candidate| format!(
                "{} at {}",
                candidate.display_name(),
                candidate.app_path.display()
            ))
            .collect::<Vec<_>>()
            .join(", "),
        install_hint
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
