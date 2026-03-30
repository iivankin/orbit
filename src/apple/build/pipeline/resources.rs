use tempfile::NamedTempFile;

use super::*;
use crate::util::command_output;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ResourceWorkSummary {
    asset_catalogs: usize,
    generated_default_app_icon: bool,
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
        if self.generated_default_app_icon {
            parts.push("generated default app icon".to_owned());
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
) -> Result<ResourceWorkSummary> {
    let resources_root = bundle_resources_root(toolchain, target.kind, bundle_root);
    ensure_dir(&resources_root)?;
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

    let compiled_asset_catalogs =
        compile_asset_catalogs(toolchain, target.kind, &asset_catalogs, bundle_root)?;

    let summary = ResourceWorkSummary {
        asset_catalogs: asset_catalogs.len(),
        generated_default_app_icon: compiled_asset_catalogs.generated_default_app_icon,
        interface_resources: interface_jobs.len(),
        strings_files: strings_jobs.len(),
        core_data_models: core_data_jobs.len(),
        copied_resources: copy_jobs.len(),
    };
    for (source, relative) in interface_jobs {
        compile_interface_resource(toolchain, &source, &resources_root.join(relative))?;
    }
    for (source, relative) in strings_jobs {
        compile_strings_resource(&source, &resources_root.join(relative))?;
    }
    for (source, relative) in core_data_jobs {
        compile_core_data_model(&source, &resources_root.join(relative))?;
    }

    for (source, relative) in copy_jobs {
        let destination = resources_root.join(relative);
        if source.is_dir() {
            copy_dir_recursive(&source, &destination)?;
        } else {
            copy_file(&source, &destination)?;
        }
    }

    Ok(summary)
}

pub(super) fn should_process_resources(platform: ApplePlatform, target: &TargetManifest) -> bool {
    !target.resources.is_empty()
        || default_icon::should_generate_default_app_icon(platform, target.kind)
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

struct CompiledAssetCatalogs {
    generated_default_app_icon: bool,
}

fn compile_asset_catalogs(
    toolchain: &Toolchain,
    target_kind: TargetKind,
    asset_catalogs: &[PathBuf],
    bundle_root: &Path,
) -> Result<CompiledAssetCatalogs> {
    let prepared_catalogs =
        default_icon::prepare_asset_catalogs(toolchain.platform, target_kind, asset_catalogs)?;
    if prepared_catalogs.catalogs().is_empty() {
        return Ok(CompiledAssetCatalogs {
            generated_default_app_icon: false,
        });
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
    if asset_catalog_contains_named_set(prepared_catalogs.catalogs(), "AccentColor.colorset") {
        command.arg("--accent-color").arg("AccentColor");
    }
    if prepared_catalogs.has_app_icon() {
        command.arg("--app-icon").arg("AppIcon");
    }
    for catalog in prepared_catalogs.catalogs() {
        command.arg(catalog);
    }
    command_output(&mut command)?;
    merge_partial_info_plist(
        bundle_metadata_root(toolchain, target_kind, bundle_root),
        partial_plist.path(),
    )?;
    default_icon::ensure_icon_metadata(
        toolchain.platform,
        target_kind,
        &bundle_metadata_root(toolchain, target_kind, bundle_root),
        prepared_catalogs.has_app_icon(),
    )?;
    Ok(CompiledAssetCatalogs {
        generated_default_app_icon: prepared_catalogs.generated_default_app_icon(),
    })
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
    let mut command = Command::new("xcrun");
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

fn compile_core_data_model(source: &Path, destination: &Path) -> Result<()> {
    ensure_parent_dir(destination)?;
    let mut command = Command::new("xcrun");
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
