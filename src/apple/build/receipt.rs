use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::manifest::{ApplePlatform, BuildConfiguration, DistributionKind};
use crate::util::{read_json_file, timestamp_slug, write_json_file};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildReceipt {
    pub id: String,
    pub target: String,
    pub platform: ApplePlatform,
    pub configuration: BuildConfiguration,
    pub distribution: DistributionKind,
    pub destination: String,
    pub bundle_id: String,
    pub bundle_path: PathBuf,
    pub artifact_path: PathBuf,
    pub created_at_unix: u64,
    pub submit_eligible: bool,
}

impl BuildReceipt {
    pub fn new(
        target: impl Into<String>,
        platform: ApplePlatform,
        configuration: BuildConfiguration,
        distribution: DistributionKind,
        destination: impl Into<String>,
        bundle_id: impl Into<String>,
        bundle_path: PathBuf,
        artifact_path: PathBuf,
    ) -> Self {
        Self {
            id: timestamp_slug(),
            target: target.into(),
            platform,
            configuration,
            distribution,
            destination: destination.into(),
            bundle_id: bundle_id.into(),
            bundle_path,
            artifact_path,
            created_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            submit_eligible: distribution.supports_submit(),
        }
    }
}

pub fn write_receipt(receipts_dir: &Path, receipt: &BuildReceipt) -> Result<PathBuf> {
    let path = receipts_dir.join(format!(
        "{}-{}-{}-{}-{}.json",
        receipt.id,
        receipt.target,
        receipt.platform,
        receipt.distribution.as_str(),
        receipt.configuration.as_str()
    ));
    write_json_file(&path, receipt)?;
    Ok(path)
}

pub fn load_receipt(path: &Path) -> Result<BuildReceipt> {
    read_json_file(path)
}

pub fn list_receipts(
    receipts_dir: &Path,
    platform: Option<ApplePlatform>,
    distribution: Option<DistributionKind>,
) -> Result<Vec<BuildReceipt>> {
    if !receipts_dir.exists() {
        return Ok(Vec::new());
    }

    let mut receipts = Vec::new();
    for entry in fs::read_dir(receipts_dir)
        .with_context(|| format!("failed to read {}", receipts_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let receipt = load_receipt(&entry.path())?;
        if platform.is_some_and(|candidate| receipt.platform != candidate) {
            continue;
        }
        if distribution.is_some_and(|candidate| receipt.distribution != candidate) {
            continue;
        }
        receipts.push(receipt);
    }
    receipts.sort_by_key(|receipt| receipt.created_at_unix);
    Ok(receipts)
}

pub fn find_latest_receipt(
    receipts_dir: &Path,
    platform: Option<ApplePlatform>,
    distribution: Option<DistributionKind>,
) -> Result<Option<BuildReceipt>> {
    let mut receipts = list_receipts(receipts_dir, platform, distribution)?;
    Ok(receipts.pop())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{BuildReceipt, find_latest_receipt, list_receipts, write_receipt};
    use crate::manifest::{ApplePlatform, BuildConfiguration, DistributionKind};

    #[test]
    fn finds_latest_matching_receipt() {
        let temp = tempdir().unwrap();
        let first = BuildReceipt {
            id: "1".to_owned(),
            target: "App".to_owned(),
            platform: ApplePlatform::Ios,
            configuration: BuildConfiguration::Debug,
            distribution: DistributionKind::Development,
            destination: "simulator".to_owned(),
            bundle_id: "dev.example.app".to_owned(),
            bundle_path: temp.path().join("one.app"),
            artifact_path: temp.path().join("one.app"),
            created_at_unix: 1,
            submit_eligible: false,
        };
        let second = BuildReceipt {
            id: "2".to_owned(),
            created_at_unix: 2,
            ..first.clone()
        };
        write_receipt(temp.path(), &first).unwrap();
        write_receipt(temp.path(), &second).unwrap();

        let latest = find_latest_receipt(
            temp.path(),
            Some(ApplePlatform::Ios),
            Some(DistributionKind::Development),
        )
        .unwrap()
        .unwrap();
        assert_eq!(latest.id, "2");
    }

    #[test]
    fn lists_receipts_sorted_oldest_to_newest() {
        let temp = tempdir().unwrap();
        let first = BuildReceipt {
            id: "1".to_owned(),
            target: "App".to_owned(),
            platform: ApplePlatform::Ios,
            configuration: BuildConfiguration::Debug,
            distribution: DistributionKind::Development,
            destination: "simulator".to_owned(),
            bundle_id: "dev.example.app".to_owned(),
            bundle_path: temp.path().join("one.app"),
            artifact_path: temp.path().join("one.app"),
            created_at_unix: 1,
            submit_eligible: false,
        };
        let second = BuildReceipt {
            id: "2".to_owned(),
            created_at_unix: 2,
            ..first.clone()
        };
        write_receipt(temp.path(), &second).unwrap();
        write_receipt(temp.path(), &first).unwrap();

        let receipts = list_receipts(
            temp.path(),
            Some(ApplePlatform::Ios),
            Some(DistributionKind::Development),
        )
        .unwrap();
        assert_eq!(
            receipts
                .into_iter()
                .map(|receipt| receipt.id)
                .collect::<Vec<_>>(),
            vec!["1".to_owned(), "2".to_owned()]
        );
    }
}
