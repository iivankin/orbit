use std::fs;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use super::artifacts::remove_existing_path;
use super::*;
use crate::apple::build::app_icon;
use crate::util::{command_output, read_json_file_if_exists, write_json_file};

const RESOURCE_CACHE_VERSION: u32 = 2;

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CachedAssetCatalogInfo {
    version: u32,
    fingerprint: String,
    has_app_icon: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CachedResourceJobInfo {
    version: u32,
    fingerprint: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ResourceWorkSummary {
    asset_catalogs: usize,
    interface_resources: usize,
    strings_files: usize,
    core_data_models: usize,
    copied_resources: usize,
}

impl ResourceWorkSummary {
    pub(super) fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.asset_catalogs > 0 {
            parts.push(format!("{} asset catalog(s)", self.asset_catalogs));
        }
        if self.interface_resources > 0 {
            parts.push(format!("{} interface file(s)", self.interface_resources));
        }
        if self.strings_files > 0 {
            parts.push(format!("{} strings file(s)", self.strings_files));
        }
        if self.core_data_models > 0 {
            parts.push(format!("{} Core Data model(s)", self.core_data_models));
        }
        if self.copied_resources > 0 {
            parts.push(format!("{} copied resource(s)", self.copied_resources));
        }
        if parts.is_empty() {
            return "no resource work".to_owned();
        }
        parts.join(", ")
    }
}

pub(super) fn process_resources(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    bundle_root: &Path,
    target_dir: &Path,
) -> Result<ResourceWorkSummary> {
    let resources_root = bundle_resources_root(toolchain, target.kind, bundle_root);
    ensure_dir(&resources_root)?;
    let resource_cache_root = target_dir.join("resource-cache");
    ensure_dir(&resource_cache_root)?;
    let mut asset_catalogs = Vec::new();
    let mut interface_jobs = Vec::new();
    let mut strings_jobs = Vec::new();
    let mut core_data_jobs = Vec::new();
    let mut copy_jobs = Vec::new();

    for resource in &target.resources {
        let resource_path = resolve_path(&project.root, resource);
        if !resource_path.exists() {
            bail!(
                "resource path `{}` for target `{}` does not exist",
                resource_path.display(),
                target.name
            );
        }
        discover_resources(
            &resource_path,
            &resource_path,
            &mut asset_catalogs,
            &mut interface_jobs,
            &mut strings_jobs,
            &mut core_data_jobs,
            &mut copy_jobs,
        )?;
    }

    compile_asset_catalogs(
        toolchain,
        target.kind,
        &asset_catalogs,
        bundle_root,
        &resource_cache_root,
    )?;

    let summary = ResourceWorkSummary {
        asset_catalogs: asset_catalogs.len(),
        interface_resources: interface_jobs.len(),
        strings_files: strings_jobs.len(),
        core_data_models: core_data_jobs.len(),
        copied_resources: copy_jobs.len(),
    };
    for (source, relative) in interface_jobs {
        let destination = resources_root.join(&relative);
        let cache_dir = resource_job_cache_dir(&resource_cache_root, "ibtool", &relative);
        let fingerprint = resource_job_fingerprint(toolchain, "ibtool", &source, &relative)?;
        if !restore_cached_resource_job(&cache_dir, &destination, &fingerprint)? {
            compile_interface_resource(toolchain, &source, &destination)?;
            write_cached_resource_job(&cache_dir, &destination, &fingerprint)?;
        }
    }
    for (source, relative) in strings_jobs {
        let destination = resources_root.join(&relative);
        let cache_dir = resource_job_cache_dir(&resource_cache_root, "strings", &relative);
        let fingerprint = resource_job_fingerprint(toolchain, "strings", &source, &relative)?;
        if !restore_cached_resource_job(&cache_dir, &destination, &fingerprint)? {
            compile_strings_resource(&source, &destination)?;
            write_cached_resource_job(&cache_dir, &destination, &fingerprint)?;
        }
    }
    for (source, relative) in core_data_jobs {
        let destination = resources_root.join(&relative);
        let cache_dir = resource_job_cache_dir(&resource_cache_root, "momc", &relative);
        let fingerprint = resource_job_fingerprint(toolchain, "momc", &source, &relative)?;
        if !restore_cached_resource_job(&cache_dir, &destination, &fingerprint)? {
            compile_core_data_model(toolchain, &source, &destination)?;
            write_cached_resource_job(&cache_dir, &destination, &fingerprint)?;
        }
    }

    for (source, relative) in copy_jobs {
        let destination = resources_root.join(relative);
        let cache_dir = resource_job_cache_dir(
            &resource_cache_root,
            if source.is_dir() {
                "copy-dir"
            } else {
                "copy-file"
            },
            destination
                .strip_prefix(&resources_root)
                .unwrap_or(destination.as_path()),
        );
        let fingerprint = resource_job_fingerprint(
            toolchain,
            if source.is_dir() {
                "copy-dir"
            } else {
                "copy-file"
            },
            &source,
            destination
                .strip_prefix(&resources_root)
                .unwrap_or(destination.as_path()),
        )?;
        if !restore_cached_resource_job(&cache_dir, &destination, &fingerprint)? {
            if source.is_dir() {
                copy_dir_recursive(&source, &destination)?;
            } else {
                copy_file(&source, &destination)?;
            }
            write_cached_resource_job(&cache_dir, &destination, &fingerprint)?;
        }
    }

    Ok(summary)
}

pub(super) fn should_process_resources(target: &TargetManifest) -> bool {
    !target.resources.is_empty()
}

fn discover_resources(
    current: &Path,
    root: &Path,
    asset_catalogs: &mut Vec<PathBuf>,
    interface_jobs: &mut Vec<(PathBuf, PathBuf)>,
    strings_jobs: &mut Vec<(PathBuf, PathBuf)>,
    core_data_jobs: &mut Vec<(PathBuf, PathBuf)>,
    copy_jobs: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<()> {
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xcassets"))
    {
        asset_catalogs.push(current.to_path_buf());
        return Ok(());
    }
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("storyboard"))
    {
        interface_jobs.push((
            current.to_path_buf(),
            compiled_interface_relative(current, root, "storyboardc")?,
        ));
        return Ok(());
    }
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xib"))
    {
        interface_jobs.push((
            current.to_path_buf(),
            compiled_interface_relative(current, root, "nib")?,
        ));
        return Ok(());
    }
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("xcdatamodel")
                || extension.eq_ignore_ascii_case("xcdatamodeld")
        })
    {
        core_data_jobs.push((
            current.to_path_buf(),
            compiled_interface_relative(
                current,
                root,
                if current
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("xcdatamodeld"))
                {
                    "momd"
                } else {
                    "mom"
                },
            )?,
        ));
        return Ok(());
    }

    if current.is_file() {
        let relative = current
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| current.file_name().map(PathBuf::from).unwrap_or_default());
        if current
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("strings"))
        {
            strings_jobs.push((current.to_path_buf(), relative));
            return Ok(());
        }
        copy_jobs.push((current.to_path_buf(), relative));
        return Ok(());
    }

    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xcassets"))
        {
            asset_catalogs.push(path);
            continue;
        }
        if path.is_dir() {
            discover_resources(
                &path,
                root,
                asset_catalogs,
                interface_jobs,
                strings_jobs,
                core_data_jobs,
                copy_jobs,
            )?;
        } else {
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("failed to derive resource path for {}", path.display()))?
                .to_path_buf();
            if path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("strings"))
            {
                strings_jobs.push((path, relative));
                continue;
            }
            if path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("storyboard"))
            {
                interface_jobs.push((
                    path.clone(),
                    compiled_interface_relative(&path, root, "storyboardc")?,
                ));
                continue;
            }
            if path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("xib"))
            {
                interface_jobs.push((
                    path.clone(),
                    compiled_interface_relative(&path, root, "nib")?,
                ));
                continue;
            }
            copy_jobs.push((path, relative));
        }
    }
    Ok(())
}

fn compile_asset_catalogs(
    toolchain: &Toolchain,
    target_kind: TargetKind,
    asset_catalogs: &[PathBuf],
    bundle_root: &Path,
    resource_cache_root: &Path,
) -> Result<()> {
    if asset_catalogs.is_empty() {
        return Ok(());
    }
    let has_app_icon = app_icon::asset_catalogs_have_app_icon(asset_catalogs);

    let fingerprint = asset_catalog_fingerprint(toolchain, target_kind, asset_catalogs)?;
    let cache_dir = resource_cache_root.join("asset-catalogs");
    if restore_cached_asset_catalogs(
        &cache_dir,
        &fingerprint,
        toolchain,
        target_kind,
        bundle_root,
    )? {
        return Ok(());
    }

    let partial_plist = NamedTempFile::new()?;
    let resources_root = bundle_resources_root(toolchain, target_kind, bundle_root);
    let mut command = toolchain.actool_command();
    command.arg("actool");
    command.arg("--compile").arg(&resources_root);
    command
        .arg("--output-partial-info-plist")
        .arg(partial_plist.path());
    command
        .arg("--platform")
        .arg(toolchain.actool_platform_name());
    command
        .arg("--minimum-deployment-target")
        .arg(&toolchain.deployment_target);
    for device in toolchain.actool_target_device() {
        command.arg("--target-device").arg(device);
    }
    if asset_catalog_contains_named_set(asset_catalogs, "AccentColor.colorset") {
        command.arg("--accent-color").arg("AccentColor");
    }
    if has_app_icon {
        command.arg("--app-icon").arg("AppIcon");
    }
    for catalog in asset_catalogs {
        command.arg(catalog);
    }
    command_output(&mut command)?;
    merge_partial_info_plist(
        bundle_metadata_root(toolchain, target_kind, bundle_root),
        partial_plist.path(),
    )?;
    app_icon::ensure_icon_metadata(
        toolchain.platform,
        target_kind,
        &bundle_metadata_root(toolchain, target_kind, bundle_root),
        has_app_icon,
    )?;
    write_cached_asset_catalogs(
        &cache_dir,
        &fingerprint,
        &resources_root,
        partial_plist.path(),
        has_app_icon,
    )?;
    Ok(())
}

fn restore_cached_asset_catalogs(
    cache_dir: &Path,
    fingerprint: &str,
    toolchain: &Toolchain,
    target_kind: TargetKind,
    bundle_root: &Path,
) -> Result<bool> {
    let Some(cache_info) =
        read_json_file_if_exists::<CachedAssetCatalogInfo>(&cache_dir.join("cache.json"))?
    else {
        return Ok(false);
    };
    if cache_info.version != RESOURCE_CACHE_VERSION || cache_info.fingerprint != fingerprint {
        return Ok(false);
    }
    let cached_output = cache_dir.join("output");
    if !cached_output.exists() {
        return Ok(false);
    }

    let resources_root = bundle_resources_root(toolchain, target_kind, bundle_root);
    restore_cached_output(&cached_output, &resources_root)?;
    merge_partial_info_plist(
        bundle_metadata_root(toolchain, target_kind, bundle_root),
        &cache_dir.join("partial-info.plist"),
    )?;
    app_icon::ensure_icon_metadata(
        toolchain.platform,
        target_kind,
        &bundle_metadata_root(toolchain, target_kind, bundle_root),
        cache_info.has_app_icon,
    )?;
    Ok(true)
}

fn write_cached_asset_catalogs(
    cache_dir: &Path,
    fingerprint: &str,
    resources_root: &Path,
    partial_plist_path: &Path,
    has_app_icon: bool,
) -> Result<()> {
    ensure_dir(cache_dir)?;
    let cached_output = cache_dir.join("output");
    copy_output_to_cache(resources_root, &cached_output)?;
    if partial_plist_path.exists() {
        copy_output_to_cache(partial_plist_path, &cache_dir.join("partial-info.plist"))?;
    }
    write_json_file(
        &cache_dir.join("cache.json"),
        &CachedAssetCatalogInfo {
            version: RESOURCE_CACHE_VERSION,
            fingerprint: fingerprint.to_owned(),
            has_app_icon,
        },
    )
}

fn restore_cached_resource_job(
    cache_dir: &Path,
    destination: &Path,
    fingerprint: &str,
) -> Result<bool> {
    let Some(cache_info) =
        read_json_file_if_exists::<CachedResourceJobInfo>(&cache_dir.join("cache.json"))?
    else {
        return Ok(false);
    };
    if cache_info.version != RESOURCE_CACHE_VERSION || cache_info.fingerprint != fingerprint {
        return Ok(false);
    }
    let cached_output = cache_dir.join("output");
    if !cached_output.exists() {
        return Ok(false);
    }
    restore_cached_output(&cached_output, destination)?;
    Ok(true)
}

fn write_cached_resource_job(cache_dir: &Path, output: &Path, fingerprint: &str) -> Result<()> {
    ensure_dir(cache_dir)?;
    let cached_output = cache_dir.join("output");
    copy_output_to_cache(output, &cached_output)?;
    write_json_file(
        &cache_dir.join("cache.json"),
        &CachedResourceJobInfo {
            version: RESOURCE_CACHE_VERSION,
            fingerprint: fingerprint.to_owned(),
        },
    )
}

fn copy_output_to_cache(source: &Path, cache_output: &Path) -> Result<()> {
    remove_existing_path(cache_output)?;
    if source.is_dir() {
        copy_dir_recursive(source, cache_output)
    } else {
        copy_file(source, cache_output)
    }
}

fn restore_cached_output(cache_output: &Path, destination: &Path) -> Result<()> {
    remove_existing_path(destination)?;
    if cache_output.is_dir() {
        copy_dir_recursive(cache_output, destination)
    } else {
        copy_file(cache_output, destination)
    }
}

fn resource_job_cache_dir(resource_cache_root: &Path, kind: &str, relative: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(relative.to_string_lossy().as_bytes());
    resource_cache_root
        .join("jobs")
        .join(hex_digest(hasher.finalize()))
}

fn resource_job_fingerprint(
    toolchain: &Toolchain,
    kind: &str,
    source: &Path,
    relative: &Path,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(RESOURCE_CACHE_VERSION.to_le_bytes());
    hasher.update(kind.as_bytes());
    hasher.update(relative.to_string_lossy().as_bytes());
    hasher.update(toolchain.platform.to_string().as_bytes());
    hasher.update(toolchain.destination.as_str().as_bytes());
    hasher.update(toolchain.sdk_name.as_bytes());
    hasher.update(toolchain.deployment_target.as_bytes());
    hasher.update(toolchain.target_triple.as_bytes());
    hash_resource_toolchain(&mut hasher, toolchain);
    hash_resource_input(&mut hasher, source)?;
    Ok(hex_digest(hasher.finalize()))
}

fn asset_catalog_fingerprint(
    toolchain: &Toolchain,
    target_kind: TargetKind,
    asset_catalogs: &[PathBuf],
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(RESOURCE_CACHE_VERSION.to_le_bytes());
    hasher.update(toolchain.platform.to_string().as_bytes());
    hasher.update(toolchain.destination.as_str().as_bytes());
    hasher.update(toolchain.sdk_name.as_bytes());
    hasher.update(toolchain.deployment_target.as_bytes());
    hasher.update(toolchain.target_triple.as_bytes());
    hash_resource_toolchain(&mut hasher, toolchain);
    hasher.update(format!("{:?}", target_kind).as_bytes());
    for catalog in asset_catalogs {
        hash_resource_input(&mut hasher, catalog)?;
    }
    Ok(hex_digest(hasher.finalize()))
}

fn hash_resource_toolchain(hasher: &mut Sha256, toolchain: &Toolchain) {
    hasher.update(toolchain.actool_platform_name().as_bytes());
    for device in toolchain.actool_target_device() {
        hasher.update([0]);
        hasher.update(device.as_bytes());
    }
    if let Some(selected_xcode) = &toolchain.selected_xcode {
        hasher.update(selected_xcode.version.as_bytes());
        hasher.update(selected_xcode.build_version.as_bytes());
        hasher.update(selected_xcode.developer_dir.to_string_lossy().as_bytes());
    } else {
        hasher.update(b"system-xcode");
    }
}

fn hash_resource_input(hasher: &mut Sha256, path: &Path) -> Result<()> {
    hasher.update(path.to_string_lossy().as_bytes());
    if !path.exists() {
        hasher.update(b"missing");
        return Ok(());
    }

    let mut entries = Vec::new();
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = entry.with_context(|| format!("failed to walk {}", path.display()))?;
        entries.push(entry.into_path());
    }
    entries.sort();

    for entry in entries {
        hasher.update(entry.to_string_lossy().as_bytes());
        let metadata = fs::symlink_metadata(&entry)
            .with_context(|| format!("failed to stat {}", entry.display()))?;
        if metadata.file_type().is_symlink() {
            hasher.update(b"symlink");
            hasher.update(
                fs::read_link(&entry)
                    .with_context(|| format!("failed to read symlink {}", entry.display()))?
                    .to_string_lossy()
                    .as_bytes(),
            );
            continue;
        }
        if metadata.is_dir() {
            hasher.update(b"dir");
            continue;
        }
        hasher.update(b"file");
        hasher.update(metadata.len().to_le_bytes());
        let modified = metadata
            .modified()
            .with_context(|| format!("failed to read mtime for {}", entry.display()))?
            .duration_since(std::time::UNIX_EPOCH)
            .with_context(|| format!("mtime for {} was before UNIX_EPOCH", entry.display()))?;
        hasher.update(modified.as_nanos().to_le_bytes());
    }
    Ok(())
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn asset_catalog_contains_named_set(asset_catalogs: &[PathBuf], expected_name: &str) -> bool {
    asset_catalogs
        .iter()
        .any(|catalog| catalog.join(expected_name).exists())
}

pub(super) fn merge_partial_info_plist(
    info_plist_root: impl AsRef<Path>,
    partial_plist_path: &Path,
) -> Result<()> {
    let info_plist_root = info_plist_root.as_ref();
    if !partial_plist_path.exists() {
        return Ok(());
    }

    let info_plist_path = info_plist_root.join("Info.plist");
    if !info_plist_path.exists() {
        return Ok(());
    }

    let mut info_plist = Value::from_file(&info_plist_path)
        .with_context(|| format!("failed to read {}", info_plist_path.display()))?;
    let partial = Value::from_file(partial_plist_path)
        .with_context(|| format!("failed to read {}", partial_plist_path.display()))?;
    let info_dict = info_plist
        .as_dictionary_mut()
        .context("Info.plist must be a dictionary")?;
    let partial_dict = partial
        .as_dictionary()
        .context("actool partial Info.plist must be a dictionary")?;
    for (key, value) in partial_dict {
        info_dict.insert(key.clone(), value.clone());
    }
    info_plist
        .to_file_xml(&info_plist_path)
        .with_context(|| format!("failed to write {}", info_plist_path.display()))
}

fn compile_interface_resource(
    toolchain: &Toolchain,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    ensure_parent_dir(destination)?;
    let mut command = toolchain.actool_command();
    command.args(["--sdk", toolchain.sdk_name.as_str(), "ibtool"]);
    command.arg("--compile").arg(destination);
    command
        .arg("--platform")
        .arg(toolchain.actool_platform_name());
    command
        .arg("--minimum-deployment-target")
        .arg(&toolchain.deployment_target);
    for device in toolchain.actool_target_device() {
        command.arg("--target-device").arg(device);
    }
    command.arg(source);
    command_output(&mut command).map(|_| ())
}

fn compile_strings_resource(source: &Path, destination: &Path) -> Result<()> {
    ensure_parent_dir(destination)?;
    let mut command = Command::new("plutil");
    command.args(["-convert", "binary1", "-o"]);
    command.arg(destination);
    command.arg(source);
    run_command(&mut command)
}

fn compile_core_data_model(toolchain: &Toolchain, source: &Path, destination: &Path) -> Result<()> {
    ensure_parent_dir(destination)?;
    let mut command = toolchain.actool_command();
    command.arg("momc");
    command.arg(source);
    command.arg(destination);
    run_command(&mut command)
}

fn compiled_interface_relative(
    source: &Path,
    root: &Path,
    output_extension: &str,
) -> Result<PathBuf> {
    let relative = source
        .strip_prefix(root)
        .with_context(|| format!("failed to derive resource path for {}", source.display()))?;
    let mut destination = relative.to_path_buf();
    destination.set_extension(output_extension);
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{asset_catalog_fingerprint, resource_job_fingerprint};
    use crate::apple::build::toolchain::{DestinationKind, Toolchain};
    use crate::apple::xcode::SelectedXcode;
    use crate::manifest::{ApplePlatform, TargetKind};

    fn fixture_toolchain(selected_xcode: Option<SelectedXcode>) -> Toolchain {
        Toolchain {
            platform: ApplePlatform::Ios,
            destination: DestinationKind::Simulator,
            sdk_name: "iphonesimulator".to_owned(),
            sdk_path: PathBuf::from("/Applications/Xcode.app/SDKs/iPhoneSimulator.sdk"),
            deployment_target: "18.0".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "arm64-apple-ios18.0-simulator".to_owned(),
            selected_xcode,
        }
    }

    #[test]
    fn asset_catalog_fingerprint_changes_when_selected_xcode_changes() {
        let temp = tempdir().unwrap();
        let asset_catalog = temp.path().join("Assets.xcassets");
        fs::create_dir_all(asset_catalog.join("AppIcon.appiconset")).unwrap();
        fs::write(asset_catalog.join("Contents.json"), "{}\n").unwrap();
        fs::write(
            asset_catalog
                .join("AppIcon.appiconset")
                .join("Contents.json"),
            "{}\n",
        )
        .unwrap();

        let first = asset_catalog_fingerprint(
            &fixture_toolchain(Some(SelectedXcode {
                version: "16.0".to_owned(),
                build_version: "16A242d".to_owned(),
                app_path: PathBuf::from("/Applications/Xcode-16.0.app"),
                developer_dir: PathBuf::from("/Applications/Xcode-16.0.app/Contents/Developer"),
            })),
            TargetKind::App,
            std::slice::from_ref(&asset_catalog),
        )
        .unwrap();
        let second = asset_catalog_fingerprint(
            &fixture_toolchain(Some(SelectedXcode {
                version: "16.1".to_owned(),
                build_version: "16B40".to_owned(),
                app_path: PathBuf::from("/Applications/Xcode-16.1.app"),
                developer_dir: PathBuf::from("/Applications/Xcode-16.1.app/Contents/Developer"),
            })),
            TargetKind::App,
            std::slice::from_ref(&asset_catalog),
        )
        .unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn resource_job_fingerprint_changes_when_developer_dir_changes() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("Main.storyboard");
        let relative = PathBuf::from("Main.storyboardc");
        fs::write(&source, "<document />\n").unwrap();

        let first = resource_job_fingerprint(
            &fixture_toolchain(Some(SelectedXcode {
                version: "16.0".to_owned(),
                build_version: "16A242d".to_owned(),
                app_path: PathBuf::from("/Applications/Xcode-16.0.app"),
                developer_dir: PathBuf::from("/Applications/Xcode-16.0.app/Contents/Developer"),
            })),
            "ibtool",
            &source,
            &relative,
        )
        .unwrap();
        let second = resource_job_fingerprint(
            &fixture_toolchain(Some(SelectedXcode {
                version: "16.0".to_owned(),
                build_version: "16A242d".to_owned(),
                app_path: PathBuf::from("/Applications/Xcode-16.0-copy.app"),
                developer_dir: PathBuf::from(
                    "/Applications/Xcode-16.0-copy.app/Contents/Developer",
                ),
            })),
            "ibtool",
            &source,
            &relative,
        )
        .unwrap();

        assert_ne!(first, second);
    }
}
