use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use md5::Context as Md5Context;
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder};
use reqwest::header::{ACCEPT, CONTENT_TYPE, COOKIE, HeaderName, HeaderValue};
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;
use time::format_description;

use super::auth_flow::ProviderUploadAuth;
use super::endpoints;
use super::package::{PreparedAsset, PreparedUpload};

#[derive(Debug, Clone)]
pub struct ContentDeliveryClient {
    client: Client,
    iris_base_url: String,
    provider_public_id: String,
    notification_device_id: Option<String>,
    build_create_headers: BTreeMap<String, String>,
    file_create_headers: BTreeMap<String, String>,
    file_get_headers: BTreeMap<String, String>,
    file_patch_headers: BTreeMap<String, String>,
    build_get_headers: BTreeMap<String, String>,
    metrics_headers: BTreeMap<String, String>,
    session_auth: Option<SessionAuth>,
    dqsid_cookie: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionAuth {
    session_id: String,
    shared_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct BuildResponseDocument {
    pub data: BuildResource,
}

#[derive(Debug, Deserialize)]
pub struct BuildResource {
    pub id: String,
    pub attributes: BuildAttributes,
}

#[derive(Debug, Deserialize)]
pub struct BuildAttributes {
    #[serde(rename = "uploadedDate", default)]
    pub _uploaded_date: Option<String>,
    #[serde(rename = "processingState", default)]
    pub processing_state: Option<String>,
    #[serde(rename = "processingErrors", default)]
    pub processing_errors: Option<Vec<ProcessingIssue>>,
    #[serde(rename = "buildProcessingState", default)]
    pub build_processing_state: Option<BuildProcessingState>,
}

#[derive(Debug, Deserialize)]
pub struct ProcessingIssue {
    #[serde(default)]
    pub _code: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BuildProcessingState {
    pub state: String,
    #[serde(default)]
    pub errors: Vec<ProcessingIssue>,
}

#[derive(Debug, Deserialize)]
pub struct BuildDeliveryFileDocument {
    pub data: BuildDeliveryFileResource,
}

#[derive(Debug, Deserialize)]
pub struct BuildDeliveryFileResource {
    pub id: String,
    pub attributes: BuildDeliveryFileAttributes,
}

#[derive(Debug, Deserialize)]
pub struct BuildDeliveryFileAttributes {
    #[serde(rename = "assetType")]
    pub _asset_type: String,
    #[serde(rename = "assetDeliveryState")]
    pub asset_delivery_state: AssetDeliveryState,
    #[serde(rename = "uploadOperations", default)]
    pub upload_operations: Option<Vec<UploadOperation>>,
}

#[derive(Debug, Deserialize)]
pub struct AssetDeliveryState {
    pub state: String,
    #[serde(default)]
    pub errors: Vec<DeliveryMessage>,
    #[serde(default)]
    pub warnings: Vec<DeliveryMessage>,
}

#[derive(Debug, Deserialize)]
pub struct DeliveryMessage {
    #[serde(default)]
    pub _code: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UploadOperation {
    pub method: String,
    pub url: String,
    pub length: u64,
    #[serde(rename = "requestHeaders", default)]
    pub request_headers: Vec<UploadOperationHeader>,
}

#[derive(Debug, Deserialize)]
pub struct UploadOperationHeader {
    pub name: String,
    pub value: String,
}

impl ContentDeliveryClient {
    pub fn from_live_auth(auth: &ProviderUploadAuth) -> Result<Self> {
        let client = ClientBuilder::new()
            .brotli(true)
            .gzip(true)
            .deflate(true)
            .build()
            .context("failed to build the Iris HTTP client")?;
        let headers = auth.headers().clone();
        Ok(Self {
            client,
            iris_base_url: endpoints::iris_base_url(),
            provider_public_id: auth.provider_public_id.clone(),
            notification_device_id: None,
            build_create_headers: headers.clone(),
            file_create_headers: headers.clone(),
            file_get_headers: headers.clone(),
            file_patch_headers: headers.clone(),
            build_get_headers: headers.clone(),
            metrics_headers: headers,
            session_auth: auth
                .session_auth()
                .map(|(session_id, shared_secret)| SessionAuth {
                    session_id: session_id.to_owned(),
                    shared_secret: shared_secret.to_owned(),
                }),
            dqsid_cookie: None,
        })
    }

    pub fn create_build(&mut self, app_id: &str, upload: &PreparedUpload) -> Result<String> {
        let notification = self.notification_device_id.as_ref().map(|device_id| {
            json!({
                "attributes": {
                    "deliveryMechanism": "APNS",
                    "deviceId": device_id,
                    "environment": "PRODUCTION",
                    "sourceApplication": "TRANSPORTER"
                },
                "id": "${notification}",
                "type": "deliveryNotifications"
            })
        });

        let mut relationships = json!({
            "app": {"data": {"id": app_id, "type": "apps"}}
        });
        let mut included = Vec::new();
        if let Some(notification) = notification {
            relationships["deliveryNotifications"] = json!({
                "data": [{"id": "${notification}", "type": "deliveryNotifications"}]
            });
            included.push(notification);
        }

        let body = json!({
            "data": {
                "attributes": {
                    "cfBundleShortVersionString": upload.cf_bundle_short_version_string,
                    "cfBundleVersion": upload.cf_bundle_version,
                    "platform": upload.build_platform
                },
                "relationships": relationships,
                "type": "builds"
            },
            "included": included
        });

        let url = self.iris_url("v1/builds");
        let response = self
            .request_with_json("POST", &url, &self.build_create_headers, &body)?
            .send()
            .context("failed to create content delivery build")?;
        self.update_dqsid_cookie(&response);
        let response = response_for_status(response)?;
        let build: BuildResponseDocument = response
            .json()
            .context("failed to parse build create response")?;
        Ok(build.data.id)
    }

    pub fn create_build_file(
        &mut self,
        build_id: &str,
        asset: &PreparedAsset,
    ) -> Result<BuildDeliveryFileDocument> {
        let body = json!({
            "data": {
                "attributes": {
                    "assetType": asset.asset_type.as_str(),
                    "fileName": asset.file_name,
                    "fileSize": asset.file_size,
                    "sourceFileChecksum": asset.md5_uppercase,
                    "uti": asset.uti
                },
                "relationships": {
                    "build": {
                        "data": {"id": build_id, "type": "builds"}
                    }
                },
                "type": "buildDeliveryFiles"
            }
        });
        let url = self.iris_url("v1/buildDeliveryFiles");
        let response = self
            .request_with_json("POST", &url, &self.file_create_headers, &body)?
            .send()
            .context("failed to create buildDeliveryFile")?;
        self.update_dqsid_cookie(&response);
        let response = response_for_status(response)?;
        response
            .json()
            .context("failed to parse buildDeliveryFile response")
    }

    pub fn upload_delivery_file(
        &self,
        operation: &UploadOperation,
        asset_path: &Path,
    ) -> Result<()> {
        if !operation.method.eq_ignore_ascii_case("PUT") {
            bail!("unsupported upload operation method `{}`", operation.method);
        }
        let bytes = fs::read(asset_path)
            .with_context(|| format!("failed to read {}", asset_path.display()))?;
        if bytes.len() as u64 != operation.length {
            bail!(
                "upload length mismatch for {}: server expects {}, local file is {} bytes",
                asset_path.display(),
                operation.length,
                bytes.len()
            );
        }

        let mut request = self.client.put(&operation.url);
        for header in &operation.request_headers {
            request = request.header(&header.name, &header.value);
        }
        request = request
            .header("Upload-Draft-Interop-Version", "6")
            .header("Upload-Complete", "?1");
        let response = request
            .body(bytes)
            .send()
            .context("failed to upload to Apple object storage")?;
        if !response.status().is_success() {
            bail!("object storage upload failed with {}", response.status());
        }
        Ok(())
    }

    pub fn mark_build_file_uploaded(&mut self, file_id: &str) -> Result<BuildDeliveryFileDocument> {
        let body = json!({
            "data": {
                "attributes": {"uploaded": true},
                "id": file_id,
                "type": "buildDeliveryFiles"
            }
        });
        let url = self.iris_url(&format!("v1/buildDeliveryFiles/{file_id}"));
        let response = self
            .request_with_json("PATCH", &url, &self.file_patch_headers, &body)?
            .send()
            .context("failed to mark buildDeliveryFile as uploaded")?;
        self.update_dqsid_cookie(&response);
        let response = response_for_status(response)?;
        response
            .json()
            .context("failed to parse uploaded buildDeliveryFile response")
    }

    pub fn get_build_file(&mut self, file_id: &str) -> Result<BuildDeliveryFileDocument> {
        let url = self.iris_url(&format!("v1/buildDeliveryFiles/{file_id}"));
        let response = self
            .request("GET", &url, &self.file_get_headers)?
            .send()
            .context("failed to fetch buildDeliveryFile")?;
        self.update_dqsid_cookie(&response);
        let response = response_for_status(response)?;
        response
            .json()
            .context("failed to parse buildDeliveryFile fetch response")
    }

    pub fn get_build(&mut self, build_id: &str) -> Result<BuildResponseDocument> {
        let url = self.iris_url(&format!("v1/builds/{build_id}"));
        let response = self
            .request("GET", &url, &self.build_get_headers)?
            .send()
            .context("failed to fetch build")?;
        self.update_dqsid_cookie(&response);
        let response = response_for_status(response)?;
        response
            .json()
            .context("failed to parse build fetch response")
    }

    pub fn send_metrics(&mut self, upload: &PreparedUpload, build_id: &str) -> Result<()> {
        let body = json!({
            "data": {
                "type": "metricsAndLogging",
                "attributes": {
                    "buildId": build_id,
                    "assetCount": upload.assets.len(),
                    "clientName": "orbit",
                    "clientVersion": env!("CARGO_PKG_VERSION")
                }
            }
        });
        let url = self.iris_url("v1/metricsAndLogging");
        let response = self
            .request_with_json("POST", &url, &self.metrics_headers, &body)?
            .send()
            .context("failed to send metricsAndLogging")?;
        self.update_dqsid_cookie(&response);
        let _ = response_for_status(response)?;
        Ok(())
    }

    fn request_with_json(
        &self,
        method: &str,
        url: &str,
        template: &BTreeMap<String, String>,
        body: &impl Serialize,
    ) -> Result<RequestBuilder> {
        let body = serde_json::to_vec(body).context("failed to serialize Iris JSON body")?;
        Ok(self
            .request_builder(method, url, template, Some(&body))?
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body))
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        template: &BTreeMap<String, String>,
    ) -> Result<RequestBuilder> {
        self.request_builder(method, url, template, None)
    }

    fn request_builder(
        &self,
        method: &str,
        url: &str,
        template: &BTreeMap<String, String>,
        body: Option<&[u8]>,
    ) -> Result<RequestBuilder> {
        let method = reqwest::Method::from_bytes(method.as_bytes())?;
        let mut request = self.client.request(method, url);
        for (name, value) in template {
            if name.eq_ignore_ascii_case("host")
                || name.eq_ignore_ascii_case("connection")
                || name.eq_ignore_ascii_case("accept-encoding")
                || name.eq_ignore_ascii_case("content-length")
                || name.eq_ignore_ascii_case("cookie")
            {
                continue;
            }
            if self.session_auth.is_some()
                && (name.eq_ignore_ascii_case("x-request-id")
                    || name.eq_ignore_ascii_case("x-session-id")
                    || name.eq_ignore_ascii_case("x-session-digest"))
            {
                continue;
            }
            request = request.header(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(value)?,
            );
        }
        if let Some(cookie) = &self.dqsid_cookie {
            request = request.header(COOKIE, HeaderValue::from_str(cookie)?);
        }
        if let Some(session_auth) = &self.session_auth {
            if template
                .keys()
                .any(|name| name.eq_ignore_ascii_case("x-apple-i-client-time"))
            {
                request = request.header(
                    HeaderName::from_static("x-apple-i-client-time"),
                    HeaderValue::from_str(&current_client_time())?,
                );
            }
            let request_id = request_id()?;
            request = request.header(
                HeaderName::from_static("x-request-id"),
                HeaderValue::from_str(&request_id)?,
            );
            request = request.header(
                HeaderName::from_static("x-session-id"),
                HeaderValue::from_str(&session_auth.session_id)?,
            );
            request = request.header(
                HeaderName::from_static("x-session-digest"),
                HeaderValue::from_str(&session_digest(
                    &session_auth.session_id,
                    &session_auth.shared_secret,
                    body.unwrap_or_default(),
                    &request_id,
                ))?,
            );
        }
        request = request.header(ACCEPT, HeaderValue::from_static("application/json"));
        Ok(request)
    }

    fn iris_url(&self, tail: &str) -> String {
        format!(
            "{}/provider/{}/{}",
            self.iris_base_url, self.provider_public_id, tail
        )
    }

    fn update_dqsid_cookie(&mut self, response: &reqwest::blocking::Response) {
        for cookie in response.headers().get_all(reqwest::header::SET_COOKIE) {
            let Ok(cookie) = cookie.to_str() else {
                continue;
            };
            if let Some(value) = cookie.split(';').next()
                && value.starts_with("dqsid=")
            {
                self.dqsid_cookie = Some(value.to_owned());
            }
        }
    }
}

fn request_id() -> Result<String> {
    let format =
        format_description::parse("[year][month][day][hour][minute][second]-[subsecond digits:3]")
            .context("failed to build Iris request-id formatter")?;
    OffsetDateTime::now_utc()
        .format(&format)
        .context("failed to format Iris request id")
}

fn session_digest(
    session_id: &str,
    shared_secret: &str,
    json_data: &[u8],
    request_id: &str,
) -> String {
    let mut context = Md5Context::new();
    context.consume(session_id.as_bytes());
    context.consume(json_data);
    context.consume(request_id.as_bytes());
    context.consume(shared_secret.as_bytes());
    format!("{:x}", context.finalize())
}

fn current_client_time() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn response_for_status(
    response: reqwest::blocking::Response,
) -> Result<reqwest::blocking::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let text = response.text().unwrap_or_default();
    if let Ok(document) = serde_json::from_str::<ErrorDocument>(&text)
        && !document.errors.is_empty()
    {
        let summary = document
            .errors
            .iter()
            .filter_map(|error| error.detail.clone().or_else(|| error.title.clone()))
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "content delivery request failed with {}: {}",
            status,
            if summary.is_empty() {
                "unknown Apple error".to_owned()
            } else {
                summary
            }
        );
    }
    bail!("content delivery request failed with {}: {}", status, text);
}

#[derive(Debug, Deserialize)]
struct ErrorDocument {
    errors: Vec<ErrorEntry>,
}

#[derive(Debug, Deserialize)]
struct ErrorEntry {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    detail: Option<String>,
}
