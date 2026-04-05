use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::manifest::ApplePlatform;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum RunStatus {
    Passed,
    Failed,
}

#[derive(Debug, Serialize)]
pub(super) struct UiTestRunReport {
    pub(super) id: String,
    pub(super) platform: ApplePlatform,
    pub(super) backend: String,
    pub(super) bundle_id: String,
    pub(super) bundle_path: PathBuf,
    pub(super) receipt_path: PathBuf,
    pub(super) target_name: String,
    pub(super) target_id: String,
    pub(super) report_path: PathBuf,
    pub(super) artifacts_dir: PathBuf,
    pub(super) started_at_unix: u64,
    pub(super) finished_at_unix: u64,
    pub(super) duration_ms: u64,
    pub(super) status: RunStatus,
    pub(super) flows: Vec<FlowRunReport>,
}

#[derive(Debug, Serialize)]
pub(super) struct FlowRunReport {
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) invoked_by: Option<PathBuf>,
    pub(super) started_at_unix: u64,
    pub(super) finished_at_unix: u64,
    pub(super) duration_ms: u64,
    pub(super) status: RunStatus,
    pub(super) error: Option<String>,
    pub(super) failure_screenshot: Option<PathBuf>,
    pub(super) failure_hierarchy: Option<PathBuf>,
    pub(super) video: Option<PathBuf>,
    pub(super) steps: Vec<StepRunReport>,
}

#[derive(Debug, Serialize)]
pub(super) struct StepRunReport {
    pub(super) index: usize,
    pub(super) command: String,
    pub(super) duration_ms: u64,
    pub(super) status: RunStatus,
    pub(super) error: Option<String>,
    pub(super) artifact: Option<PathBuf>,
}

pub(super) fn flow_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("flow")
        .to_owned()
}

pub(super) fn append_report_error(report: &mut FlowRunReport, message: String) {
    match report.error.as_mut() {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&message);
        }
        None => report.error = Some(message),
    }
}

pub(super) fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn sanitize_artifact_name(value: &str) -> String {
    sanitize_path_component(value, "artifact")
}

pub(super) fn sanitize_extension_component(value: &str) -> String {
    sanitize_path_component(value, "artifact")
}

fn sanitize_path_component(value: &str, fallback: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        fallback.to_owned()
    } else {
        sanitized
    }
}
