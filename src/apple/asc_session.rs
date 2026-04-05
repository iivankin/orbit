use anyhow::{Context, Result, bail};
use reqwest::Url;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::apple::asc_api::{AppAttributes, JsonApiDocument, JsonApiListDocument, Resource};
use crate::apple::authkit::{AuthKitIdentity, bootstrap_authkit, build_cookie_client, header_map};
use crate::apple::grand_slam::{XcodeNotaryAuth, establish_xcode_notary_auth};
use crate::context::AppContext;
use crate::manifest::ApplePlatform;

const APP_STORE_CONNECT_IRIS_BASE_URL: &str = "https://appstoreconnect.apple.com/iris";
const APP_STORE_CONNECT_JSON_CONTENT_TYPE: &str = "application/vnd.api+json";

#[derive(Debug, Clone)]
pub struct CreateAppRecordInput<'a> {
    pub name: &'a str,
    pub sku: &'a str,
    pub primary_locale: &'a str,
    pub bundle_id: &'a str,
    pub platform: ApplePlatform,
    pub version_number: &'a str,
}

#[derive(Debug)]
pub struct AscSessionAppsClient {
    client: Client,
    auth: XcodeNotaryAuth,
    provider_public_id: String,
}

impl AscSessionAppsClient {
    pub fn authenticate(app: &AppContext, provider_public_id: impl Into<String>) -> Result<Self> {
        let auth = establish_xcode_notary_auth(app)?;
        let client = build_cookie_client("ASC session")?;
        let asc = Self {
            client,
            auth,
            provider_public_id: provider_public_id.into(),
        };
        asc.authenticate_with_authkit()?;
        Ok(asc)
    }

    pub fn find_app_by_bundle_id(
        &self,
        bundle_id: &str,
    ) -> Result<Option<Resource<AppAttributes>>> {
        let response: JsonApiListDocument<AppAttributes> = self.get(
            "/v1/apps",
            &[
                ("limit", "1".to_owned()),
                ("filter[bundleId]", bundle_id.to_owned()),
                ("fields[apps]", "name,sku,primaryLocale,bundleId".to_owned()),
            ],
        )?;
        Ok(response.data.into_iter().next())
    }

    pub fn create_app_record(
        &self,
        input: &CreateAppRecordInput<'_>,
    ) -> Result<Resource<AppAttributes>> {
        let platform = asc_platform(input.platform)?;
        let store_version_id = format!("${{store-version-{platform}}}");
        let version_localization_id = format!("${{new-{platform}VersionLocalization-id}}");
        let request = serde_json::json!({
            "data": {
                "type": "apps",
                "attributes": {
                    "bundleId": input.bundle_id,
                    "name": input.name,
                    "primaryLocale": input.primary_locale,
                    "sku": input.sku,
                },
                "relationships": {
                    "appInfos": {
                        "data": [
                            {
                                "type": "appInfos",
                                "id": "${new-appInfo-id}",
                            }
                        ]
                    },
                    "appStoreVersions": {
                        "data": [
                            {
                                "type": "appStoreVersions",
                                "id": store_version_id,
                            }
                        ]
                    }
                }
            },
            "included": [
                {
                    "type": "appInfos",
                    "id": "${new-appInfo-id}",
                    "relationships": {
                        "appInfoLocalizations": {
                            "data": [
                                {
                                    "type": "appInfoLocalizations",
                                    "id": "${new-appInfoLocalization-id}",
                                }
                            ]
                        }
                    }
                },
                {
                    "type": "appInfoLocalizations",
                    "id": "${new-appInfoLocalization-id}",
                    "attributes": {
                        "locale": input.primary_locale,
                        "name": input.name,
                    }
                },
                {
                    "type": "appStoreVersions",
                    "id": store_version_id,
                    "attributes": {
                        "platform": platform,
                        "versionString": input.version_number,
                    },
                    "relationships": {
                        "appStoreVersionLocalizations": {
                            "data": [
                                {
                                    "type": "appStoreVersionLocalizations",
                                    "id": version_localization_id,
                                }
                            ]
                        }
                    }
                },
                {
                    "type": "appStoreVersionLocalizations",
                    "id": version_localization_id,
                    "attributes": {
                        "locale": input.primary_locale,
                    }
                }
            ]
        });
        let response: JsonApiDocument<AppAttributes> = self.post("/v1/apps", &request)?;
        Ok(response.data)
    }

    fn authenticate_with_authkit(&self) -> Result<()> {
        let _: serde_json::Value = bootstrap_authkit(
            &self.client,
            &self.auth,
            AuthKitIdentity::Xcode,
            "ASC session",
        )?;
        Ok(())
    }

    fn get<T>(&self, path: &str, query: &[(&str, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let mut url = Url::parse(&self.provider_url(path))
            .with_context(|| format!("failed to build ASC session URL for `{path}`"))?;
        for (key, value) in query {
            url.query_pairs_mut().append_pair(key, value);
        }
        let response = self
            .client
            .get(url)
            .headers(self.json_headers()?)
            .send()
            .with_context(|| format!("failed to call ASC session endpoint `{path}`"))?;
        parse_json_response(response, path)
    }

    fn post<T, S>(&self, path: &str, body: &S) -> Result<T>
    where
        T: DeserializeOwned,
        S: Serialize,
    {
        let response = self
            .client
            .post(self.provider_url(path))
            .headers(self.json_headers()?)
            .json(body)
            .send()
            .with_context(|| format!("failed to call ASC session endpoint `{path}`"))?;
        parse_json_response(response, path)
    }

    fn provider_url(&self, path: &str) -> String {
        format!(
            "{APP_STORE_CONNECT_IRIS_BASE_URL}/provider/{}/{}",
            self.provider_public_id,
            path.trim_start_matches('/')
        )
    }

    fn json_headers(&self) -> Result<HeaderMap> {
        let mut headers = header_map(&self.auth.authkit_headers())?;
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(APP_STORE_CONNECT_JSON_CONTENT_TYPE),
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(APP_STORE_CONNECT_JSON_CONTENT_TYPE),
        );
        Ok(headers)
    }
}

fn asc_platform(platform: ApplePlatform) -> Result<&'static str> {
    match platform {
        ApplePlatform::Ios => Ok("IOS"),
        ApplePlatform::Macos => Ok("MAC_OS"),
        ApplePlatform::Tvos => Ok("TV_OS"),
        ApplePlatform::Visionos => Ok("VISION_OS"),
        ApplePlatform::Watchos => bail!("watchOS App Store Connect apps are not supported"),
    }
}

fn parse_json_response<T>(response: Response, label: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let body = response
        .bytes()
        .with_context(|| format!("failed to read `{label}` response body"))?;
    if !status.is_success() {
        bail!(
            "{label} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body)
        .with_context(|| format!("failed to parse `{label}` response body"))
}
