use super::*;

pub(super) fn validate_requested_xcode_version(version: &str) -> Result<()> {
    parse_version_components(version)?;
    Ok(())
}

pub(super) fn resolve_requested_xcode(version: Option<&str>) -> Result<Option<SelectedXcode>> {
    resolve_requested_xcode_with_mode(version, false)
}

pub(super) fn resolve_requested_xcode_for_app(
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

pub(super) fn resolve_requested_xcode_with_mode(
    version: Option<&str>,
    interactive: bool,
) -> Result<Option<SelectedXcode>> {
    resolve_requested_xcode_in_roots(version, &installed_xcode_search_roots(), interactive)
}

pub(super) fn resolve_requested_xcode_in_roots(
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

pub(super) fn discover_xcodes_under(
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

pub(super) fn load_xcode_bundle(path: &Path) -> Result<Option<SelectedXcode>> {
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

pub(super) fn version_matches(requested: &str, candidate: &str) -> Result<bool> {
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

pub(super) fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
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

pub(super) fn preferred_xcode_install_root(roots: &[PathBuf]) -> Result<PathBuf> {
    if let Some(root) = configured_xcode_install_root(roots) {
        return Ok(root);
    }
    Ok(dirs::home_dir()
        .context("failed to resolve the user home directory for Xcode installs")?
        .join("Applications"))
}

pub(super) fn configured_xcode_install_root(roots: &[PathBuf]) -> Option<PathBuf> {
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
