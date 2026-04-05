use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    PayloadChecksumKind, PercentEncodingMode, SessionTokenMode, SignableBody, SignableRequest,
    SignatureLocation, SigningSettings, UriPathNormalizationMode, sign,
};
use aws_sigv4::sign::v4;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Url;
use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::apple::authkit::{AuthKitIdentity, bootstrap_authkit, build_cookie_client, header_map};
use crate::apple::build::receipt::BuildReceipt;
use crate::apple::build::verify::{
    should_verify_developer_id_artifact, verify_post_build, verify_post_notarization,
};
use crate::apple::grand_slam::XcodeNotaryAuth;
use crate::context::ProjectContext;
use crate::util::{
    CliSpinner, combine_command_output, command_output, command_output_allow_failure, ensure_dir,
    format_elapsed, run_command,
};

use super::endpoints;

const NOTARY_UPLOAD_USER_AGENT: &str = "Soto/5.0";
const NOTARY_POLL_TIMEOUT: Duration = Duration::from_secs(300);
const NOTARY_POLL_INTERVAL: Duration = Duration::from_secs(5);
const AWS_REGION: &str = "us-west-2";
const AWS_SERVICE: &str = "s3";

pub(crate) fn submit_with_xcode_notary(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    wait: bool,
) -> Result<()> {
    let auth = notary_progress_step(
        "Notary: Refreshing Xcode-like auth",
        |_| "Notary: Refreshed Xcode-like auth.".to_owned(),
        || resolve_xcode_notary_auth(project),
    )?;
    let team_id = resolve_team_id(project)?;
    let client = notary_progress_step(
        "Notary: Connecting to App Store Connect".to_owned(),
        |_| "Notary: Connected to App Store Connect.".to_owned(),
        || NotaryClient::new(auth, team_id),
    )?;
    notary_progress_step(
        "Notary: Starting notarization session".to_owned(),
        |_| "Notary: Started notarization session.".to_owned(),
        || {
            client.authenticate_with_authkit()?;
            Ok(())
        },
    )?;

    let archive = notary_progress_step(
        format!(
            "Notary: Preparing upload archive for {}",
            receipt.artifact_path.display()
        ),
        |archive: &PreparedNotaryArchive| format!("Notary: Prepared {}.", archive.path.display()),
        || prepare_submission_archive(project, receipt),
    )?;
    notary_progress_step(
        format!("Notary: Preflighting {}", archive.path.display()),
        String::clone,
        || preflight_submission_archive(receipt, &archive),
    )?;
    let submission_name = archive
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .with_context(|| {
            format!(
                "submit archive path `{}` does not have a valid file name",
                archive.path.display()
            )
        })?;
    let digests = notary_progress_step(
        format!(
            "Notary: Preparing archive digests for {}",
            archive.path.display()
        ),
        |_| "Notary: Prepared archive digests.".to_owned(),
        || Ok(archive.digests.clone()),
    )?;
    let created = notary_progress_step(
        format!("Notary: Creating submission for {submission_name}"),
        |created: &NotarySubmissionDocument| {
            format!("Notary: Created submission `{}`.", created.data.id)
        },
        || client.create_submission(&submission_name, &digests),
    )?;
    notary_progress_step(
        format!("Notary: Uploading {}", archive.path.display()),
        |_| "Notary: Uploaded archive to Apple.".to_owned(),
        || client.upload_submission_archive(&created, &archive.digests),
    )?;

    if wait {
        let status = client.wait_for_completion(&created.data.id)?;
        if !status
            .data
            .attributes
            .status
            .eq_ignore_ascii_case("accepted")
        {
            let developer_log = client
                .fetch_developer_log(&created.data.id)
                .unwrap_or_else(|error| format!("failed to fetch developer log: {error:#}"));
            bail!(
                "notary submission {} completed with status `{}`\n{}",
                created.data.id,
                status.data.attributes.status,
                developer_log
            );
        }

        notary_progress_step(
            format!("Notary: Stapling {}", receipt.artifact_path.display()),
            |_| format!("Notary: Stapled {}.", receipt.artifact_path.display()),
            || {
                let mut staple = std::process::Command::new("xcrun");
                staple.arg("stapler");
                staple.arg("staple");
                staple.arg(&receipt.artifact_path);
                run_command(&mut staple)?;
                Ok(())
            },
        )?;
        if should_verify_developer_id_artifact(receipt) {
            notary_progress_step(
                format!(
                    "Notary: Verifying notarized package {}",
                    receipt.artifact_path.display()
                ),
                String::clone,
                || verify_post_notarization(receipt),
            )?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct PreparedNotaryArchive {
    path: std::path::PathBuf,
    digests: ArchiveDigests,
}

fn prepare_submission_archive(
    project: &ProjectContext,
    receipt: &BuildReceipt,
) -> Result<PreparedNotaryArchive> {
    let file_name = receipt
        .artifact_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("notarization artifact is missing a valid file name")?;
    let archive_dir = project
        .project_paths
        .orbit_dir
        .join("submit")
        .join(&receipt.id)
        .join("notary");
    ensure_dir(&archive_dir)?;

    let archive_path = archive_dir.join(format!("{file_name}.zip"));
    if archive_path.exists() {
        fs::remove_file(&archive_path).with_context(|| {
            format!(
                "failed to remove existing notarization archive {}",
                archive_path.display()
            )
        })?;
    }

    create_submission_archive(&receipt.artifact_path, &archive_path)?;
    Ok(PreparedNotaryArchive {
        digests: ArchiveDigests::read(&archive_path)?,
        path: archive_path,
    })
}

fn preflight_submission_archive(
    receipt: &BuildReceipt,
    archive: &PreparedNotaryArchive,
) -> Result<String> {
    if should_verify_developer_id_artifact(receipt) {
        // Fail before upload if the source package or app signing is already invalid.
        let _ = verify_post_build(receipt)?;
    }

    let archive_name = archive
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .context("notarization archive is missing a valid file name")?;
    if archive.path.extension().and_then(|value| value.to_str()) != Some("zip") {
        bail!(
            "notarization archive `{}` must be a .zip file",
            archive.path.display()
        );
    }
    let expected_archive_name = format!(
        "{}.zip",
        receipt
            .artifact_path
            .file_name()
            .and_then(|value| value.to_str())
            .context("notarization artifact is missing a valid file name")?
    );
    if archive_name != expected_archive_name {
        bail!(
            "notarization archive `{}` should be named `{expected_archive_name}`",
            archive.path.display()
        );
    }

    let metadata = fs::metadata(&archive.path)
        .with_context(|| format!("failed to read {}", archive.path.display()))?;
    if !metadata.is_file() {
        bail!(
            "notarization archive `{}` is not a regular file",
            archive.path.display()
        );
    }
    if metadata.len() == 0 {
        bail!("notarization archive `{}` is empty", archive.path.display());
    }

    validate_zip_archive(&archive.path, &receipt.artifact_path)?;
    Ok(format!(
        "Notary: Preflight validated {}.",
        archive.path.display()
    ))
}

fn create_submission_archive(source_path: &Path, archive_path: &Path) -> Result<()> {
    let mut command = Command::new("ditto");
    command.arg("-c");
    command.arg("-k");
    command.arg("--keepParent");
    command.arg(source_path);
    command.arg(archive_path);
    run_command(&mut command)
}

fn validate_zip_archive(archive_path: &Path, expected_payload_path: &Path) -> Result<()> {
    let mut test_command = Command::new("unzip");
    test_command.arg("-tqq");
    test_command.arg(archive_path);
    let (success, stdout, stderr) = command_output_allow_failure(&mut test_command)?;
    if !success {
        let output = combine_command_output(&stdout, &stderr);
        bail!(
            "notarization archive `{}` failed zip integrity validation\n{}",
            archive_path.display(),
            output
        );
    }

    let mut list_command = Command::new("unzip");
    list_command.arg("-Z1");
    list_command.arg(archive_path);
    let listing = command_output(&mut list_command)?;
    let entries = listing
        .lines()
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    if entries.is_empty() {
        bail!(
            "notarization archive `{}` does not contain any entries",
            archive_path.display()
        );
    }

    let expected_name = expected_payload_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("notarization payload is missing a valid file name")?;
    if !entries.iter().any(|entry| {
        *entry == expected_name
            || entry
                .strip_suffix('/')
                .is_some_and(|entry| entry == expected_name)
            || entry.starts_with(&format!("{expected_name}/"))
    }) {
        bail!(
            "notarization archive `{}` does not contain expected payload `{expected_name}`",
            archive_path.display()
        );
    }

    Ok(())
}

fn notary_progress_step<T, F, G>(
    message: impl Into<String>,
    success_message: G,
    action: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
    G: FnOnce(&T) -> String,
{
    let spinner = CliSpinner::new(message.into());
    match action() {
        Ok(value) => {
            spinner.finish_success(success_message(&value));
            Ok(value)
        }
        Err(error) => {
            spinner.finish_clear();
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct NotarySubmissionDocument {
    data: NotarySubmissionData,
}

#[derive(Debug, Clone, Deserialize)]
struct NotarySubmissionData {
    id: String,
    attributes: NotarySubmissionAttributes,
}

#[derive(Debug, Clone, Deserialize)]
struct NotarySubmissionAttributes {
    #[serde(rename = "awsAccessKeyId")]
    aws_access_key_id: String,
    #[serde(rename = "awsSecretAccessKey")]
    aws_secret_access_key: String,
    #[serde(rename = "awsSessionToken")]
    aws_session_token: String,
    bucket: String,
    object: String,
}

#[derive(Debug, Clone, Deserialize)]
struct NotaryStatusDocument {
    data: NotaryStatusData,
}

#[derive(Debug, Clone, Deserialize)]
struct NotaryStatusData {
    attributes: NotaryStatusAttributes,
}

#[derive(Debug, Clone, Deserialize)]
struct NotaryStatusAttributes {
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct NotaryLogDocument {
    data: NotaryLogData,
}

#[derive(Debug, Clone, Deserialize)]
struct NotaryLogData {
    attributes: NotaryLogAttributes,
}

#[derive(Debug, Clone, Deserialize)]
struct NotaryLogAttributes {
    #[serde(rename = "developerLogUrl")]
    developer_log_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct NotaryAuthFixture {
    team_id: Option<String>,
    #[serde(flatten)]
    auth: XcodeNotaryAuth,
}

struct NotaryClient {
    client: Client,
    auth: XcodeNotaryAuth,
    team_id: String,
}

impl NotaryClient {
    fn new(auth: XcodeNotaryAuth, team_id: String) -> Result<Self> {
        let client = build_cookie_client("Xcode-like notary")?;
        Ok(Self {
            client,
            auth,
            team_id,
        })
    }

    fn authenticate_with_authkit(&self) -> Result<()> {
        let _: serde_json::Value =
            bootstrap_authkit(&self.client, &self.auth, AuthKitIdentity::Xcode, "Xcode")?;
        Ok(())
    }

    fn create_submission(
        &self,
        submission_name: &str,
        digests: &ArchiveDigests,
    ) -> Result<NotarySubmissionDocument> {
        let response = self
            .client
            .post(endpoints::notary_submissions_url())
            .headers(self.notary_headers()?)
            .json(&json!({
                "md5": digests.md5_hex_lowercase,
                "submissionName": submission_name,
                "sha256": digests.sha256_hex_lowercase,
            }))
            .send()
            .with_context(|| format!("failed to create notary submission `{submission_name}`"))?;
        parse_json_response(response, "notary submission create")
    }

    fn upload_submission_archive(
        &self,
        submission: &NotarySubmissionDocument,
        digests: &ArchiveDigests,
    ) -> Result<()> {
        let upload_url = endpoints::notary_upload_url(
            &submission.data.attributes.bucket,
            &submission.data.attributes.object,
        );
        let request = SignedS3UploadRequest::new(
            &upload_url,
            &submission.data.attributes.aws_access_key_id,
            &submission.data.attributes.aws_secret_access_key,
            &submission.data.attributes.aws_session_token,
            digests,
        )?;
        let response = self
            .client
            .put(request.url.clone())
            .headers(request.headers)
            .body(digests.bytes.clone())
            .send()
            .with_context(|| {
                format!(
                    "failed to upload notary archive to `{}`",
                    submission.data.attributes.object
                )
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response_text(response);
            bail!(
                "notary archive upload failed with {} {}\n{}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("upload error"),
                body
            );
        }
        Ok(())
    }

    fn fetch_status(&self, submission_id: &str) -> Result<NotaryStatusDocument> {
        let response = self
            .client
            .get(endpoints::notary_submission_url(submission_id))
            .headers(self.notary_headers()?)
            .send()
            .with_context(|| format!("failed to fetch notary status for `{submission_id}`"))?;
        parse_json_response(response, "notary status")
    }

    fn wait_for_completion(&self, submission_id: &str) -> Result<NotaryStatusDocument> {
        let spinner = CliSpinner::new(format!(
            "Notary: Waiting for Apple to process submission `{submission_id}`"
        ));
        let started_at = Instant::now();
        let deadline = started_at + NOTARY_POLL_TIMEOUT;
        while Instant::now() < deadline {
            let status = self.fetch_status(submission_id)?;
            let state = status.data.attributes.status.as_str();
            spinner.set_message(format!(
                "Notary: Waiting for submission `{submission_id}` ({state}, elapsed {})",
                format_elapsed(started_at.elapsed())
            ));
            if state.eq_ignore_ascii_case("accepted") {
                spinner.finish_success(format!(
                    "Notary: Submission `{submission_id}` was accepted in {}.",
                    format_elapsed(started_at.elapsed())
                ));
                return Ok(status);
            }
            if !state.eq_ignore_ascii_case("in progress") {
                spinner.finish_success(format!(
                    "Notary: Submission `{submission_id}` finished with `{state}` in {}.",
                    format_elapsed(started_at.elapsed())
                ));
                return Ok(status);
            }
            thread::sleep(NOTARY_POLL_INTERVAL);
        }
        spinner.finish_clear();
        bail!(
            "notary submission {} did not finish within {} seconds",
            submission_id,
            NOTARY_POLL_TIMEOUT.as_secs()
        )
    }

    fn fetch_developer_log(&self, submission_id: &str) -> Result<String> {
        let response = self
            .client
            .get(endpoints::notary_submission_logs_url(submission_id))
            .headers(self.notary_headers()?)
            .send()
            .with_context(|| format!("failed to fetch log metadata for `{submission_id}`"))?;
        let log = parse_json_response::<NotaryLogDocument>(response, "notary logs")?;
        let log_response = self
            .client
            .get(&log.data.attributes.developer_log_url)
            .send()
            .with_context(|| {
                format!("failed to download developer log for notary submission `{submission_id}`")
            })?;
        if !log_response.status().is_success() {
            let status = log_response.status();
            let body = response_text(log_response);
            bail!(
                "developer log download failed with {} {}\n{}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("download error"),
                body
            );
        }
        Ok(response_text(log_response))
    }

    fn notary_headers(&self) -> Result<HeaderMap> {
        let headers = self.auth.notary_headers(&self.team_id);
        header_map(&headers)
    }
}

#[derive(Debug, Clone)]
struct ArchiveDigests {
    bytes: Vec<u8>,
    md5_hex_lowercase: String,
    md5_base64: String,
    sha256_hex_lowercase: String,
}

impl ArchiveDigests {
    fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read notarization artifact {}", path.display()))?;
        let md5 = md5::compute(&bytes);
        let sha256 = Sha256::digest(&bytes);
        Ok(Self {
            bytes,
            md5_hex_lowercase: format!("{:x}", md5),
            md5_base64: STANDARD.encode(md5.0),
            sha256_hex_lowercase: bytes_to_hex_lower(sha256),
        })
    }
}

struct SignedS3UploadRequest {
    url: String,
    headers: HeaderMap,
}

impl SignedS3UploadRequest {
    fn new(
        upload_url: &str,
        access_key_id: &str,
        secret_access_key: &str,
        session_token: &str,
        digests: &ArchiveDigests,
    ) -> Result<Self> {
        Self::new_with_time(
            upload_url,
            access_key_id,
            secret_access_key,
            session_token,
            digests,
            std::time::SystemTime::now(),
        )
    }

    fn new_with_time(
        upload_url: &str,
        access_key_id: &str,
        secret_access_key: &str,
        session_token: &str,
        digests: &ArchiveDigests,
        signing_time: std::time::SystemTime,
    ) -> Result<Self> {
        let url = Url::parse(upload_url)
            .with_context(|| format!("failed to parse notary upload URL `{upload_url}`"))?;
        let mut base_headers = vec![
            ("content-md5".to_owned(), digests.md5_base64.clone()),
            (
                CONTENT_TYPE.as_str().to_owned(),
                "application/octet-stream".to_owned(),
            ),
            (
                USER_AGENT.as_str().to_owned(),
                NOTARY_UPLOAD_USER_AGENT.to_owned(),
            ),
        ];
        let signable_request = SignableRequest::new(
            "PUT",
            upload_url,
            base_headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Precomputed(digests.sha256_hex_lowercase.clone()),
        )
        .context("failed to build signable notary S3 upload request")?;
        let mut settings = SigningSettings::default();
        settings.percent_encoding_mode = PercentEncodingMode::Double;
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        settings.signature_location = SignatureLocation::Headers;
        settings.excluded_headers = Some(
            ["authorization", "x-amzn-trace-id", "transfer-encoding"]
                .into_iter()
                .map(std::borrow::Cow::Borrowed)
                .collect(),
        );
        settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
        settings.session_token_mode = SessionTokenMode::Include;
        let identity = Credentials::new(
            access_key_id,
            secret_access_key,
            Some(session_token.to_owned()),
            None,
            "orbit-notary-upload",
        )
        .into();
        let signing_params = v4::SigningParams::builder()
            .identity(&identity)
            .region(AWS_REGION)
            .name(AWS_SERVICE)
            .time(signing_time)
            .settings(settings)
            .build()
            .context("failed to build AWS SigV4 signing params for notary upload")?;
        let signing_output = sign(signable_request, &signing_params.into())
            .context("failed to sign notary S3 upload request")?;
        let (signing_instructions, _) = signing_output.into_parts();

        let mut headers = HeaderMap::new();
        for (name, value) in base_headers.drain(..) {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(&value)?,
            );
        }
        for (name, value) in signing_instructions.headers() {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(value)?,
            );
        }

        Ok(Self {
            url: url.to_string(),
            headers,
        })
    }
}

fn resolve_xcode_notary_auth(project: &ProjectContext) -> Result<XcodeNotaryAuth> {
    if let Some(path) = std::env::var_os("ORBIT_XCODE_NOTARY_AUTH_PATH") {
        let payload = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read Xcode notary auth fixture at {}",
                Path::new(&path).display()
            )
        })?;
        let fixture: NotaryAuthFixture = serde_json::from_str(&payload)
            .context("failed to parse Xcode notary auth fixture JSON")?;
        return Ok(fixture.auth);
    }

    crate::apple::grand_slam::establish_xcode_notary_auth(&project.app)
}

fn resolve_team_id(project: &ProjectContext) -> Result<String> {
    if let Some(path) = std::env::var_os("ORBIT_XCODE_NOTARY_AUTH_PATH") {
        let payload = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read Xcode notary auth fixture at {}",
                Path::new(&path).display()
            )
        })?;
        let fixture: NotaryAuthFixture = serde_json::from_str(&payload)
            .context("failed to parse Xcode notary auth fixture JSON")?;
        if let Some(team_id) = fixture.team_id {
            return Ok(team_id);
        }
    }

    project
        .resolved_manifest
        .team_id
        .clone()
        .or_else(|| std::env::var("ORBIT_APPLE_TEAM_ID").ok())
        .context("Xcode-like notarization requires team_id in orbit.json or ORBIT_APPLE_TEAM_ID")
}

fn parse_json_response<T: for<'de> Deserialize<'de>>(
    response: Response,
    context_label: &str,
) -> Result<T> {
    let status = response.status();
    let body = response
        .bytes()
        .context("failed to read HTTP response body")?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&body);
        bail!(
            "{} failed with {} {}\n{}",
            context_label,
            status.as_u16(),
            status.canonical_reason().unwrap_or("HTTP error"),
            text
        );
    }
    serde_json::from_slice(&body)
        .with_context(|| format!("failed to decode {} response body as JSON", context_label))
}

fn response_text(response: Response) -> String {
    match response.bytes() {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => format!("failed to read response body: {error:#}"),
    }
}

fn bytes_to_hex_lower(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::{ArchiveDigests, SignedS3UploadRequest};

    #[test]
    fn s3_upload_signing_matches_xcode_capture() {
        let request = SignedS3UploadRequest::new_with_time(
            "https://notary-submissions-prod.s3-accelerate.amazonaws.com/prod/AROARQRX7CZS3PRF6ZA5L:18b3b1f5-a58d-4e29-b762-95b14ad9c14f",
            "ASIARQRX7CZS4FJ2TVQ6",
            "tlydEczCTpel/la2nMoNURXE87zWnGym3UAc7FXT",
            "IQoJb3JpZ2luX2VjEEwaCXVzLXdlc3QtMiJGMEQCIBpTCVujbZV69Y2xeVJLHwpcYZRn1tnBWcfe/57R7mcdAiArjsyaea70BL4EqwmtgfSXnAagJJbuR4SXogCIMZDxpyqyAggVEAQaDDEwNDI3MDMzNzYzNyIMXntxrLGYfuTZz0F9Ko8ClSSEZWdlN30PVeudyCJ2zXBNx/SCj1R/MTjkTBgNuYrh7MeqolJX0RGGeQjTiLR4hYNxanHQuLAcIhY7Fynlwb/3WZVvMPnqvWg9VhjOqRtyr2mDo2EV2qCfdCahwZncaqDGa9zGZ5+yj0GHqsa9f8p9scbfukBR6oulbJUJcZul9egCukujk/WRWnPGbHOT0VgMx/wSZPKC73VKTHEDvaCtTmfHnTW6/QShHQDztKKQbWrg3wL5JpJItbU5u1EQ76XuXBbNc5rtbh47xKC1EVwhgaVq95LdoubneVSWW+vEpIn6Dp39x3ppIa6Mt80lHtKJMcB8ikhXI6wSVZaXlykJycVVTyOPJ3gmdI5gXjDIiqbOBjqeAUD6FxsXJTyaBCuDaoAHtjPb8HjFww8QVwtAjtWYeg7o7DzPqwRwGCLQRpacgvp9fa63lZ2abmu1DYtjAJWwXmpkQjBznMzAJdgWixT8W/dsDmwRSdpUjWIJmW1ZNpb5BHej9/E65KpU1y4p4mOgfsxaRbuEoxlWLeCyC1MsU0abI8ZQAX/ZQjNh32om6GuuiscFSNgxmgtRVq9xFsJ8",
            &ArchiveDigests {
                bytes: Vec::new(),
                md5_hex_lowercase: "94b6fc36f99c28c5ad52425acf260045".to_owned(),
                md5_base64: "lLb8NvmcKMWtUkJazyYARQ==".to_owned(),
                sha256_hex_lowercase: "82aeecfa628a4f615baa8c9076cd5193aaa49babd62fcd01a8a2b5b609175c3b".to_owned(),
            },
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_774_814_536),
        )
        .expect("captured Xcode request should sign");

        let authorization = request
            .headers
            .get("authorization")
            .expect("authorization header")
            .to_str()
            .expect("authorization header should be UTF-8");
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=ASIARQRX7CZS4FJ2TVQ6/20260329/us-west-2/s3/aws4_request, SignedHeaders=content-md5;content-type;host;user-agent;x-amz-content-sha256;x-amz-date;x-amz-security-token, Signature=583ecef0a951463033a7cc7bcc04cac99e0175ec2238b524def449d7095cb3a3"
        );
        assert_eq!(
            request
                .headers
                .get("x-amz-date")
                .expect("x-amz-date header")
                .to_str()
                .expect("x-amz-date should be UTF-8"),
            "20260329T200216Z"
        );
    }
}
