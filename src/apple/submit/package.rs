use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use plist::Value as PlistValue;
use serde::Deserialize;

use crate::apple::build::receipt::BuildReceipt;
use crate::manifest::ApplePlatform;
use crate::util::{command_output, command_output_allow_failure, ensure_dir, ensure_parent_dir};

const DEFAULT_TRANSPORTER_SWINFO: &str = "/Applications/Transporter.app/Contents/Frameworks/ContentDelivery.framework/Versions/A/Resources/swinfo";

#[derive(Debug, Clone)]
pub struct PreparedAsset {
    pub asset_type: AssetType,
    pub file_name: String,
    pub path: PathBuf,
    pub file_size: u64,
    pub md5_uppercase: String,
    pub uti: &'static str,
}

#[derive(Debug, Clone)]
pub struct PreparedUpload {
    pub cf_bundle_short_version_string: String,
    pub cf_bundle_version: String,
    pub build_platform: &'static str,
    pub assets: Vec<PreparedAsset>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AssetType {
    AssetDescription,
    AssetSpi,
    Bundle,
}

impl AssetType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AssetDescription => "ASSET_DESCRIPTION",
            Self::AssetSpi => "ASSET_SPI",
            Self::Bundle => "ASSET",
        }
    }
}

#[derive(Debug, Deserialize)]
struct HelperOutput {
    #[serde(rename = "reportedSuccess")]
    reported_success: bool,
    #[serde(rename = "assetDescriptionPath")]
    asset_description_path: String,
    #[serde(rename = "spiPath")]
    _spi_path: String,
}

pub fn prepare_upload(
    receipt: &BuildReceipt,
    provider_public_id: &str,
    workspace: &Path,
) -> Result<PreparedUpload> {
    ensure_dir(workspace)?;
    let bundle_info = read_bundle_info(receipt)?;

    let asset_description_path = generate_asset_description(receipt, provider_public_id, workspace)
        .context("failed to generate the ContentDelivery asset description")?;
    let spi_path = generate_spi(receipt, workspace)
        .context("failed to generate the Transporter SPI payload")?;

    let assets = vec![
        prepared_asset(
            AssetType::AssetDescription,
            &asset_description_path,
            "com.apple.binary-property-list",
        )?,
        prepared_asset(AssetType::AssetSpi, &spi_path, "com.pkware.zip-archive")?,
        prepared_asset(
            AssetType::Bundle,
            &receipt.artifact_path,
            bundle_uti(receipt.platform, &receipt.artifact_path)?,
        )?,
    ];

    Ok(PreparedUpload {
        cf_bundle_short_version_string: bundle_info.short_version,
        cf_bundle_version: bundle_info.build_version,
        build_platform: build_platform(receipt.platform)?,
        assets,
    })
}

pub fn software_type_for_receipt(receipt: &BuildReceipt) -> Result<&'static str> {
    software_type(receipt.platform, &receipt.artifact_path)
}

#[derive(Debug)]
struct BundleInfo {
    short_version: String,
    build_version: String,
}

fn read_bundle_info(receipt: &BuildReceipt) -> Result<BundleInfo> {
    let info_path = receipt.bundle_path.join("Info.plist");
    let plist = PlistValue::from_file(&info_path)
        .with_context(|| format!("failed to read {}", info_path.display()))?;
    let dict = plist
        .into_dictionary()
        .context("bundle Info.plist is not a dictionary")?;
    let short_version = dict
        .get("CFBundleShortVersionString")
        .and_then(PlistValue::as_string)
        .map(ToOwned::to_owned)
        .context("bundle Info.plist is missing CFBundleShortVersionString")?;
    let build_version = dict
        .get("CFBundleVersion")
        .and_then(PlistValue::as_string)
        .map(ToOwned::to_owned)
        .context("bundle Info.plist is missing CFBundleVersion")?;
    Ok(BundleInfo {
        short_version,
        build_version,
    })
}

fn generate_asset_description(
    receipt: &BuildReceipt,
    provider_public_id: &str,
    workspace: &Path,
) -> Result<PathBuf> {
    if use_swinfo_asset_description() {
        return generate_asset_description_with_swinfo(receipt, workspace);
    }

    generate_asset_description_with_helper(receipt, provider_public_id, workspace)
}

fn generate_asset_description_with_helper(
    receipt: &BuildReceipt,
    provider_public_id: &str,
    workspace: &Path,
) -> Result<PathBuf> {
    let helper_app = ensure_asset_helper_app(workspace)?;
    let output_dir = workspace.join("asset-description");
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir)
            .with_context(|| format!("failed to clear {}", output_dir.display()))?;
    }
    ensure_dir(&output_dir)?;

    let mut command = Command::new(helper_app.join("Contents/MacOS/orbit-asset-helper"));
    command
        .arg(&receipt.artifact_path)
        .arg(helper_platform(receipt.platform)?)
        .arg(provider_public_id)
        .arg(&output_dir);
    let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
    if !stderr.trim().is_empty() {
        eprintln!("{stderr}");
    }
    let output: HelperOutput =
        serde_json::from_str(stdout.trim()).context("failed to parse asset helper output")?;
    let asset_path = PathBuf::from(output.asset_description_path);
    if asset_path.exists() {
        return Ok(asset_path);
    }
    if success || output.reported_success {
        bail!("asset helper did not emit an asset description path");
    }
    bail!("asset helper failed and no asset description was produced");
}

fn generate_asset_description_with_swinfo(
    receipt: &BuildReceipt,
    workspace: &Path,
) -> Result<PathBuf> {
    let output_dir = workspace.join("asset-description");
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir)
            .with_context(|| format!("failed to clear {}", output_dir.display()))?;
    }
    ensure_dir(&output_dir)?;

    let asset_path = output_dir.join("asset-description.plist");
    let mut command = Command::new(transporter_swinfo_path());
    command
        .arg("-f")
        .arg(&receipt.artifact_path)
        .arg("-o")
        .arg(&asset_path)
        .arg("-temporary")
        .arg(&output_dir)
        .arg("--plistFormat")
        .arg("binary")
        .arg("-platform")
        .arg(helper_platform(receipt.platform)?);
    let _ = command_output(&mut command)?;
    if asset_path.exists() {
        return Ok(asset_path);
    }
    bail!(
        "swinfo did not produce an asset description at {}",
        asset_path.display()
    )
}

fn generate_spi(receipt: &BuildReceipt, workspace: &Path) -> Result<PathBuf> {
    let output_dir = workspace.join("spi");
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir)
            .with_context(|| format!("failed to clear {}", output_dir.display()))?;
    }
    ensure_dir(&output_dir)?;

    let asset_path = output_dir.join("asset-description.plist");
    let mut command = Command::new(transporter_swinfo_path());
    command
        .arg("-f")
        .arg(&receipt.artifact_path)
        .arg("-o")
        .arg(&asset_path)
        .arg("-temporary")
        .arg(&output_dir)
        .arg("--plistFormat")
        .arg("binary")
        .arg("-platform")
        .arg(helper_platform(receipt.platform)?)
        .arg("--output-spi")
        .arg(output_dir.join("placeholder.zip"));
    let _ = command_output(&mut command)?;

    let mut matches = fs::read_dir(&output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    value.starts_with("DTAppAnalyzerExtractorOutput-") && value.ends_with(".zip")
                })
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
        .pop()
        .context("swinfo did not produce a DTAppAnalyzerExtractorOutput zip")
}

fn ensure_asset_helper_app(workspace: &Path) -> Result<PathBuf> {
    let helper_root = workspace.join("asset-helper/OrbitAssetHelper.app");
    let executable_path = helper_root.join("Contents/MacOS/orbit-asset-helper");
    if executable_path.exists() {
        return Ok(helper_root);
    }

    ensure_parent_dir(&executable_path)?;
    ensure_dir(&helper_root.join("Contents/MacOS"))?;

    let info_plist = helper_root.join("Contents/Info.plist");
    fs::write(
        &info_plist,
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"https://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\"><dict>\n",
            "  <key>CFBundleIdentifier</key><string>com.apple.TransporterApp</string>\n",
            "  <key>CFBundleName</key><string>OrbitAssetHelper</string>\n",
            "  <key>CFBundleExecutable</key><string>orbit-asset-helper</string>\n",
            "  <key>CFBundleVersion</key><string>1</string>\n",
            "  <key>CFBundleShortVersionString</key><string>1.0</string>\n",
            "</dict></plist>\n"
        ),
    )
    .with_context(|| format!("failed to write {}", info_plist.display()))?;

    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/apple/submit/asset_description_helper.m");
    let mut command = Command::new("clang");
    command
        .arg("-fobjc-arc")
        .arg("-framework")
        .arg("Foundation")
        .arg(&source)
        .arg("-o")
        .arg(&executable_path);
    let _ = command_output(&mut command)?;
    Ok(helper_root)
}

fn prepared_asset(asset_type: AssetType, path: &Path, uti: &'static str) -> Result<PreparedAsset> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .with_context(|| format!("{} is missing a file name", path.display()))?;
    let file_size = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len();
    let md5_uppercase = file_md5_uppercase(path)?;
    Ok(PreparedAsset {
        asset_type,
        file_name,
        path: path.to_path_buf(),
        file_size,
        md5_uppercase,
        uti,
    })
}

fn file_md5_uppercase(path: &Path) -> Result<String> {
    let mut md5_command = Command::new("md5");
    md5_command.arg("-q").arg(path);
    let md5 = command_output(&mut md5_command);
    if let Ok(value) = md5 {
        return Ok(value.trim().to_ascii_uppercase());
    }

    let mut md5sum_command = Command::new("md5sum");
    md5sum_command.arg(path);
    let md5sum = command_output(&mut md5sum_command)?;
    Ok(md5sum
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase())
}

fn helper_platform(platform: ApplePlatform) -> Result<&'static str> {
    match platform {
        ApplePlatform::Ios => Ok("ios"),
        ApplePlatform::Macos => Ok("osx"),
        ApplePlatform::Tvos => Ok("appletvos"),
        ApplePlatform::Visionos => Ok("xros"),
        ApplePlatform::Watchos => Ok("watchos"),
    }
}

fn build_platform(platform: ApplePlatform) -> Result<&'static str> {
    match platform {
        ApplePlatform::Ios => Ok("IOS"),
        ApplePlatform::Macos => Ok("MAC_OS"),
        ApplePlatform::Tvos => Ok("TV_OS"),
        ApplePlatform::Visionos => Ok("VISION_OS"),
        ApplePlatform::Watchos => bail!("watchOS App Store submit is not implemented yet"),
    }
}

fn software_type(platform: ApplePlatform, artifact_path: &Path) -> Result<&'static str> {
    match platform {
        ApplePlatform::Ios
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => Ok("Purple"),
        ApplePlatform::Macos => {
            if artifact_path.extension().and_then(|value| value.to_str()) == Some("pkg") {
                Ok("Firenze")
            } else {
                bail!("macOS content delivery submit expects a .pkg artifact")
            }
        }
    }
}

fn bundle_uti(platform: ApplePlatform, artifact_path: &Path) -> Result<&'static str> {
    match platform {
        ApplePlatform::Ios
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => Ok("com.apple.ipa"),
        ApplePlatform::Macos => {
            if artifact_path.extension().and_then(|value| value.to_str()) == Some("pkg") {
                Ok("com.apple.pkg")
            } else {
                bail!("macOS content delivery submit expects a .pkg artifact")
            }
        }
    }
}

fn transporter_swinfo_path() -> PathBuf {
    std::env::var_os("ORBIT_TRANSPORTER_SWINFO_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TRANSPORTER_SWINFO))
}

fn use_swinfo_asset_description() -> bool {
    std::env::var("ORBIT_TRANSPORTER_USE_SWINFO_ASSET_DESCRIPTION")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}
