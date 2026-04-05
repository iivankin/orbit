use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use plist::Value;

use crate::manifest::{ApplePlatform, TargetKind};

const APP_ICON_SET_NAME: &str = "AppIcon";
const APP_ICON_SET_DIRECTORY: &str = "AppIcon.appiconset";

pub fn asset_catalogs_have_app_icon(asset_catalogs: &[PathBuf]) -> bool {
    asset_catalogs
        .iter()
        .any(|catalog| catalog.join(APP_ICON_SET_DIRECTORY).exists())
}

pub fn ensure_icon_metadata(
    platform: ApplePlatform,
    target_kind: TargetKind,
    info_plist_root: &Path,
    has_app_icon: bool,
) -> Result<()> {
    if !has_app_icon || !requires_top_level_icon_name(platform, target_kind) {
        return Ok(());
    }

    let info_plist_path = info_plist_root.join("Info.plist");
    if !info_plist_path.exists() {
        return Ok(());
    }

    let mut info_plist = Value::from_file(&info_plist_path)
        .with_context(|| format!("failed to read {}", info_plist_path.display()))?;
    let info_dict = info_plist
        .as_dictionary_mut()
        .context("Info.plist must be a dictionary")?;
    if !info_dict.contains_key("CFBundleIconName") {
        info_dict.insert(
            "CFBundleIconName".to_owned(),
            Value::String(APP_ICON_SET_NAME.to_owned()),
        );
    }
    info_plist
        .to_file_xml(&info_plist_path)
        .with_context(|| format!("failed to write {}", info_plist_path.display()))
}

fn requires_top_level_icon_name(platform: ApplePlatform, target_kind: TargetKind) -> bool {
    matches!(
        (platform, target_kind),
        (ApplePlatform::Ios, TargetKind::App)
    )
}

#[cfg(test)]
mod tests {
    use plist::Dictionary;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn inserts_top_level_icon_name_for_ios_apps() {
        let temp = tempdir().unwrap();
        let info_plist_path = temp.path().join("Info.plist");
        Value::Dictionary(Dictionary::from_iter([(
            "CFBundleIcons".to_owned(),
            Value::Dictionary(Dictionary::from_iter([(
                "CFBundlePrimaryIcon".to_owned(),
                Value::Dictionary(Dictionary::new()),
            )])),
        )]))
        .to_file_xml(&info_plist_path)
        .unwrap();

        ensure_icon_metadata(ApplePlatform::Ios, TargetKind::App, temp.path(), true).unwrap();

        let updated = Value::from_file(&info_plist_path).unwrap();
        assert_eq!(
            updated
                .as_dictionary()
                .and_then(|dict| dict.get("CFBundleIconName"))
                .and_then(Value::as_string),
            Some(APP_ICON_SET_NAME)
        );
    }
}
