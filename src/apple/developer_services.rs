use anyhow::{Context, Result, bail};
use plist::{Dictionary as PlistDictionary, Value as PlistValue};
use reqwest::Method;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::apple::authkit::{AuthKitIdentity, bootstrap_authkit, build_cookie_client, header_map};
use crate::apple::grand_slam::{
    XcodeNotaryAuth, establish_xcode_download_auth, establish_xcode_notary_auth,
};
use crate::context::AppContext;
const DEVELOPER_SERVICES_V2_BASE_URL: &str = "https://developerservices2.apple.com";
const DEVELOPER_SERVICES_PROTOCOL_VERSION: &str = "QH65B2";
const DEVELOPER_SERVICES_CLIENT_ID: &str = "XABBG36SBA";
const DEVELOPER_SERVICES_JSON_CONTENT_TYPE: &str = "application/vnd.api+json";

#[derive(Debug, Clone, Deserialize)]
pub struct DeveloperServicesTeam {
    #[serde(rename = "teamId")]
    pub team_id: String,
    pub name: String,
    #[serde(default, rename = "type")]
    pub team_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeveloperServicesTeamsResponse {
    #[serde(rename = "resultCode")]
    result_code: i64,
    #[serde(default)]
    teams: Vec<DeveloperServicesTeam>,
}

#[derive(Debug)]
pub struct DeveloperServicesClient {
    client: Client,
    auth: XcodeNotaryAuth,
    dsession_id: Option<String>,
}

impl DeveloperServicesClient {
    pub fn authenticate(app: &AppContext) -> Result<Self> {
        let auth = establish_xcode_notary_auth(app)?;
        Self::authenticate_with_auth(auth)
    }

    pub fn authenticate_for_xcode_download(
        app: &AppContext,
        short_version: &str,
        build_version: &str,
    ) -> Result<Self> {
        let auth = establish_xcode_download_auth(app, short_version, build_version)?;
        Self::authenticate_with_auth(auth)
    }

    pub fn clone_http_client(&self) -> Client {
        self.client.clone()
    }

    pub fn authorize_download_path(&mut self, path: &str) -> Result<()> {
        let url = reqwest::Url::parse_with_params(
            &format!("{DEVELOPER_SERVICES_V2_BASE_URL}/services/download"),
            [("path", path)],
        )
        .context("failed to build Apple Developer download authorization URL")?;
        let response = self
            .client
            .get(url)
            .headers(self.base_headers()?)
            .send()
            .context("failed to authorize Apple Developer download session")?;
        let status = response.status();
        self.capture_session_state(response.headers());
        let body = response
            .bytes()
            .context("failed to read Apple Developer download authorization response")?;
        if !status.is_success() {
            bail!(
                "Apple Developer download authorization failed with {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    fn authenticate_with_auth(auth: XcodeNotaryAuth) -> Result<Self> {
        let client = build_cookie_client("developer services")?;
        let mut developer_services = Self {
            client,
            auth,
            dsession_id: None,
        };
        developer_services.authenticate_with_authkit()?;
        developer_services.bootstrap_session()?;
        Ok(developer_services)
    }

    pub fn list_teams(&mut self) -> Result<Vec<DeveloperServicesTeam>> {
        let response = self.post_plist_action("listTeams.action")?;
        let teams: DeveloperServicesTeamsResponse = plist::from_bytes(&response.body)
            .context("failed to decode developer services team list response")?;
        if teams.result_code != 0 {
            bail!(
                "developer services team list returned resultCode={}",
                teams.result_code
            );
        }
        Ok(teams.teams)
    }

    pub fn request_json<T>(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        team_id: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self.send_json(method.clone(), path, query, team_id, body)?;
        let status = response.status();
        self.capture_session_state(response.headers());
        let bytes = response
            .bytes()
            .context("failed to read developer services response body")?;
        if !status.is_success() {
            bail!(
                "developer services request `{path}` failed with {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse developer services response for `{path}`"))
    }

    pub fn request_empty(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        team_id: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Result<()> {
        let response = self.send_json(method, path, query, team_id, body)?;
        let status = response.status();
        self.capture_session_state(response.headers());
        let bytes = response
            .bytes()
            .context("failed to read developer services response body")?;
        if !status.is_success() {
            bail!(
                "developer services request `{path}` failed with {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        Ok(())
    }

    fn authenticate_with_authkit(&mut self) -> Result<()> {
        let _: serde_json::Value = bootstrap_authkit(
            &self.client,
            &self.auth,
            AuthKitIdentity::Xcode,
            "developer services",
        )?;
        Ok(())
    }

    fn bootstrap_session(&mut self) -> Result<()> {
        let response = self.post_plist_action("viewDeveloper.action")?;
        let status = response.status;
        self.capture_session_state(&response.headers);
        let body = response.body;
        if !status.is_success() {
            bail!(
                "developer services bootstrap failed with {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        if self.dsession_id.is_none() {
            bail!("developer services bootstrap did not yield DSESSIONID");
        }
        Ok(())
    }

    fn post_plist_action(&mut self, action: &str) -> Result<DeveloperServicesResponse> {
        let request_id = Uuid::new_v4().to_string().to_uppercase();
        let body = plist_body({
            let mut request = PlistDictionary::new();
            request.insert(
                "clientId".to_owned(),
                PlistValue::String(DEVELOPER_SERVICES_CLIENT_ID.to_owned()),
            );
            request.insert(
                "protocolVersion".to_owned(),
                PlistValue::String(DEVELOPER_SERVICES_PROTOCOL_VERSION.to_owned()),
            );
            request.insert("requestId".to_owned(), PlistValue::String(request_id));
            request
        })?;

        let response = self
            .client
            .post(format!(
                "{DEVELOPER_SERVICES_V2_BASE_URL}/services/{DEVELOPER_SERVICES_PROTOCOL_VERSION}/{action}?clientId={DEVELOPER_SERVICES_CLIENT_ID}"
            ))
            .headers(self.plist_headers()?)
            .body(body)
            .send()
            .with_context(|| format!("failed to execute developer services action `{action}`"))?;
        DeveloperServicesResponse::from_response(response)
    }

    fn send_json(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        team_id: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Result<Response> {
        let url = format!("{DEVELOPER_SERVICES_V2_BASE_URL}/services/v1/{path}");
        let mut headers = self.json_headers()?;

        let (actual_method, payload) = if matches!(method, Method::GET | Method::DELETE) {
            headers.insert(
                HeaderName::from_static("x-http-method-override"),
                HeaderValue::from_str(method.as_str())?,
            );
            let mut encoded = encode_query(query)?;
            if let Some(team_id) = team_id {
                push_query_pair(&mut encoded, "teamId", team_id);
            }
            (
                Method::POST,
                json!({
                    "urlEncodedQueryParams": encoded,
                }),
            )
        } else {
            let mut payload =
                body.context("developer services write request must include a JSON body")?;
            if let Some(team_id) = team_id {
                payload["data"]["attributes"]["teamId"] =
                    serde_json::Value::String(team_id.to_owned());
            }
            (method, payload)
        };

        let response = self
            .client
            .request(actual_method, url)
            .headers(headers)
            .json(&payload)
            .send()
            .with_context(|| format!("failed to call developer services endpoint `{path}`"))?;
        Ok(response)
    }

    fn plist_headers(&self) -> Result<HeaderMap> {
        let mut headers = self.base_headers()?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/x-xml-plist"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/x-xml-plist"));
        Ok(headers)
    }

    fn json_headers(&self) -> Result<HeaderMap> {
        let mut headers = self.base_headers()?;
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(DEVELOPER_SERVICES_JSON_CONTENT_TYPE),
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(DEVELOPER_SERVICES_JSON_CONTENT_TYPE),
        );
        Ok(headers)
    }

    fn base_headers(&self) -> Result<HeaderMap> {
        let mut headers = header_map(&self.auth.developer_services_headers())?;
        if let Some(dsession_id) = self.dsession_id.as_deref() {
            headers.insert(
                HeaderName::from_static("dsessionid"),
                HeaderValue::from_str(dsession_id)?,
            );
        }
        Ok(headers)
    }

    fn capture_session_state(&mut self, headers: &HeaderMap) {
        if let Some(value) = headers
            .get("DSESSIONID")
            .or_else(|| headers.get("dsessionid"))
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.trim().is_empty())
        {
            self.dsession_id = Some(value.to_owned());
            return;
        }

        for value in headers.get_all("set-cookie") {
            let Some(value) = value.to_str().ok() else {
                continue;
            };
            let Some(cookie) = value.strip_prefix("DSESSIONID=") else {
                continue;
            };
            let session_id = cookie.split(';').next().unwrap_or_default().trim();
            if !session_id.is_empty() {
                self.dsession_id = Some(session_id.to_owned());
                return;
            }
        }
    }
}

struct DeveloperServicesResponse {
    status: reqwest::StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl DeveloperServicesResponse {
    fn from_response(response: Response) -> Result<Self> {
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .context("failed to read developer services response body")?
            .to_vec();
        Ok(Self {
            status,
            headers,
            body,
        })
    }
}

fn plist_body(request: PlistDictionary) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    PlistValue::Dictionary(request)
        .to_writer_xml(&mut body)
        .context("failed to serialize developer services plist body")?;
    Ok(body)
}

fn encode_query(query: &[(&str, String)]) -> Result<String> {
    let mut url = reqwest::Url::parse("https://example.invalid")
        .context("failed to initialize developer services query builder")?;
    for (key, value) in query {
        url.query_pairs_mut().append_pair(key, value);
    }
    Ok(url.query().unwrap_or_default().to_owned())
}

fn push_query_pair(encoded: &mut String, key: &str, value: &str) {
    let pair = reqwest::Url::parse_with_params("https://example.invalid", [(key, value)])
        .ok()
        .and_then(|url| url.query().map(ToOwned::to_owned))
        .unwrap_or_default();
    if !encoded.is_empty() {
        encoded.push('&');
    }
    encoded.push_str(&pair);
}
