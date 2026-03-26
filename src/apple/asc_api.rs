use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::blocking::{Client, Response};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::apple::auth::ApiKeyAuth;

const ASC_BASE_URL: &str = "https://api.appstoreconnect.apple.com";

#[derive(Debug, Clone)]
pub struct AscClient {
    auth: ApiKeyAuth,
    client: Client,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonApiDocument<T> {
    pub data: Resource<T>,
    #[serde(default)]
    pub included: Vec<IncludedResource>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonApiListDocument<T> {
    pub data: Vec<Resource<T>>,
    #[serde(default)]
    pub included: Vec<IncludedResource>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Resource<T> {
    pub id: String,
    #[serde(rename = "type")]
    pub resource_type: String,
    pub attributes: T,
    #[serde(default)]
    pub relationships: HashMap<String, Relationship>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Relationship {
    pub data: Option<RelationshipData>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RelationshipData {
    One(ResourceLink),
    Many(Vec<ResourceLink>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceLink {
    pub id: String,
    #[serde(rename = "type")]
    pub resource_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IncludedResource {
    pub id: String,
    #[serde(rename = "type")]
    pub resource_type: String,
    #[serde(default)]
    pub attributes: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAttributes {
    pub name: String,
    pub platform: String,
    pub udid: String,
    #[serde(rename = "deviceClass", default)]
    pub device_class: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleIdAttributes {
    pub name: String,
    pub identifier: String,
    pub platform: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleIdCapabilityAttributes {
    #[serde(rename = "capabilityType")]
    pub capability_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CertificateAttributes {
    #[serde(rename = "certificateType")]
    pub certificate_type: String,
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    #[serde(rename = "serialNumber", default)]
    pub serial_number: Option<String>,
    #[serde(rename = "expirationDate", default)]
    pub expiration_date: Option<String>,
    #[serde(rename = "certificateContent", default)]
    pub certificate_content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileAttributes {
    pub name: String,
    #[serde(rename = "profileType")]
    pub profile_type: String,
    #[serde(rename = "profileState")]
    pub profile_state: String,
    #[serde(rename = "profileContent", default)]
    pub profile_content: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(rename = "expirationDate", default)]
    pub expiration_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppAttributes {
    pub name: String,
    pub sku: String,
    #[serde(rename = "primaryLocale")]
    pub primary_locale: String,
}

#[derive(Debug, Serialize)]
struct AscClaims<'a> {
    iss: &'a str,
    aud: &'static str,
    exp: u64,
    iat: u64,
}

#[derive(Debug, Deserialize)]
struct AscErrorDocument {
    errors: Vec<AscError>,
}

#[derive(Debug, Deserialize)]
struct AscError {
    status: Option<String>,
    code: Option<String>,
    title: Option<String>,
    detail: Option<String>,
}

impl AscClient {
    pub fn new(auth: ApiKeyAuth) -> Result<Self> {
        let client = Client::builder()
            .user_agent("orbit/0.1.0")
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { auth, client })
    }

    pub fn list_devices(&self) -> Result<Vec<Resource<DeviceAttributes>>> {
        let response: JsonApiListDocument<DeviceAttributes> = self.get(
            "/v1/devices",
            &[
                ("limit", "200".to_owned()),
                (
                    "fields[devices]",
                    "name,platform,udid,deviceClass,status,model,addedDate".to_owned(),
                ),
            ],
        )?;
        Ok(response.data)
    }

    pub fn find_device_by_udid(&self, udid: &str) -> Result<Option<Resource<DeviceAttributes>>> {
        let response: JsonApiListDocument<DeviceAttributes> = self.get(
            "/v1/devices",
            &[
                ("limit", "200".to_owned()),
                ("filter[udid]", udid.to_owned()),
                (
                    "fields[devices]",
                    "name,platform,udid,deviceClass,status,model,addedDate".to_owned(),
                ),
            ],
        )?;
        Ok(response.data.into_iter().next())
    }

    pub fn create_device(
        &self,
        name: &str,
        udid: &str,
        platform: &str,
    ) -> Result<Resource<DeviceAttributes>> {
        let request = serde_json::json!({
            "data": {
                "type": "devices",
                "attributes": {
                    "name": name,
                    "udid": udid,
                    "platform": platform,
                }
            }
        });
        let response: JsonApiDocument<DeviceAttributes> = self.post("/v1/devices", &request)?;
        Ok(response.data)
    }

    pub fn delete_device(&self, id: &str) -> Result<()> {
        self.delete(&format!("/v1/devices/{id}"))
    }

    pub fn find_bundle_id(
        &self,
        identifier: &str,
    ) -> Result<Option<JsonApiDocument<BundleIdAttributes>>> {
        let response: JsonApiListDocument<BundleIdAttributes> = self.get(
            "/v1/bundleIds",
            &[
                ("limit", "200".to_owned()),
                ("filter[identifier]", identifier.to_owned()),
                ("include", "bundleIdCapabilities".to_owned()),
                (
                    "fields[bundleIds]",
                    "name,platform,identifier,bundleIdCapabilities".to_owned(),
                ),
                (
                    "fields[bundleIdCapabilities]",
                    "capabilityType,settings".to_owned(),
                ),
            ],
        )?;
        Ok(response
            .data
            .into_iter()
            .next()
            .map(|data| JsonApiDocument {
                data,
                included: response.included,
            }))
    }

    pub fn create_bundle_id(
        &self,
        name: &str,
        identifier: &str,
        platform: &str,
    ) -> Result<Resource<BundleIdAttributes>> {
        let request = serde_json::json!({
            "data": {
                "type": "bundleIds",
                "attributes": {
                    "name": name,
                    "identifier": identifier,
                    "platform": platform,
                }
            }
        });
        let response: JsonApiDocument<BundleIdAttributes> = self.post("/v1/bundleIds", &request)?;
        Ok(response.data)
    }

    pub fn create_bundle_capability(
        &self,
        bundle_id_id: &str,
        capability_type: &str,
    ) -> Result<Resource<BundleIdCapabilityAttributes>> {
        let request = serde_json::json!({
            "data": {
                "type": "bundleIdCapabilities",
                "attributes": {
                    "capabilityType": capability_type,
                },
                "relationships": {
                    "bundleId": {
                        "data": {
                            "type": "bundleIds",
                            "id": bundle_id_id,
                        }
                    }
                }
            }
        });
        let response: JsonApiDocument<BundleIdCapabilityAttributes> =
            self.post("/v1/bundleIdCapabilities", &request)?;
        Ok(response.data)
    }

    pub fn delete_bundle_capability(&self, id: &str) -> Result<()> {
        self.delete(&format!("/v1/bundleIdCapabilities/{id}"))
    }

    pub fn list_certificates(
        &self,
        certificate_type: &str,
    ) -> Result<Vec<Resource<CertificateAttributes>>> {
        let response: JsonApiListDocument<CertificateAttributes> = self.get(
            "/v1/certificates",
            &[
                ("limit", "200".to_owned()),
                ("filter[certificateType]", certificate_type.to_owned()),
                (
                    "fields[certificates]",
                    "name,certificateType,displayName,serialNumber,expirationDate,certificateContent".to_owned(),
                ),
            ],
        )?;
        Ok(response.data)
    }

    pub fn create_certificate(
        &self,
        certificate_type: &str,
        csr_content: &str,
    ) -> Result<Resource<CertificateAttributes>> {
        let request = serde_json::json!({
            "data": {
                "type": "certificates",
                "attributes": {
                    "certificateType": certificate_type,
                    "csrContent": csr_content,
                }
            }
        });
        let response: JsonApiDocument<CertificateAttributes> =
            self.post("/v1/certificates", &request)?;
        Ok(response.data)
    }

    pub fn list_profiles(
        &self,
        profile_type: &str,
    ) -> Result<JsonApiListDocument<ProfileAttributes>> {
        self.get(
            "/v1/profiles",
            &[
                ("limit", "200".to_owned()),
                ("filter[profileType]", profile_type.to_owned()),
                ("filter[profileState]", "ACTIVE".to_owned()),
                (
                    "include",
                    "bundleId,devices,certificates".to_owned(),
                ),
                (
                    "fields[profiles]",
                    "name,profileType,profileState,profileContent,uuid,expirationDate,bundleId,devices,certificates".to_owned(),
                ),
                (
                    "fields[bundleIds]",
                    "name,identifier,platform".to_owned(),
                ),
                (
                    "fields[devices]",
                    "name,platform,udid,deviceClass,status".to_owned(),
                ),
                (
                    "fields[certificates]",
                    "displayName,serialNumber,certificateType,expirationDate".to_owned(),
                ),
            ],
        )
    }

    pub fn create_profile(
        &self,
        name: &str,
        profile_type: &str,
        bundle_id_id: &str,
        certificate_ids: &[String],
        device_ids: &[String],
    ) -> Result<Resource<ProfileAttributes>> {
        let request = serde_json::json!({
            "data": {
                "type": "profiles",
                "attributes": {
                    "name": name,
                    "profileType": profile_type,
                },
                "relationships": {
                    "bundleId": {
                        "data": {
                            "type": "bundleIds",
                            "id": bundle_id_id,
                        }
                    },
                    "certificates": {
                        "data": certificate_ids.iter().map(|id| {
                            serde_json::json!({
                                "type": "certificates",
                                "id": id,
                            })
                        }).collect::<Vec<_>>(),
                    },
                    "devices": {
                        "data": device_ids.iter().map(|id| {
                            serde_json::json!({
                                "type": "devices",
                                "id": id,
                            })
                        }).collect::<Vec<_>>(),
                    },
                }
            }
        });
        let response: JsonApiDocument<ProfileAttributes> = self.post("/v1/profiles", &request)?;
        Ok(response.data)
    }

    pub fn delete_profile(&self, id: &str) -> Result<()> {
        self.delete(&format!("/v1/profiles/{id}"))
    }

    pub fn find_app_by_bundle_id(
        &self,
        bundle_id_id: &str,
    ) -> Result<Option<Resource<AppAttributes>>> {
        let response: JsonApiListDocument<AppAttributes> = self.get(
            "/v1/apps",
            &[
                ("limit", "200".to_owned()),
                ("filter[bundleId]", bundle_id_id.to_owned()),
                ("fields[apps]", "name,sku,primaryLocale,bundleId".to_owned()),
            ],
        )?;
        Ok(response.data.into_iter().next())
    }

    pub fn create_app_record(
        &self,
        name: &str,
        sku: &str,
        primary_locale: &str,
        bundle_id_id: &str,
    ) -> Result<Resource<AppAttributes>> {
        let request = serde_json::json!({
            "data": {
                "type": "apps",
                "attributes": {
                    "name": name,
                    "sku": sku,
                    "primaryLocale": primary_locale,
                },
                "relationships": {
                    "bundleId": {
                        "data": {
                            "type": "bundleIds",
                            "id": bundle_id_id,
                        }
                    }
                }
            }
        });
        let response: JsonApiDocument<AppAttributes> = self.post("/v1/apps", &request)?;
        Ok(response.data)
    }

    fn get<T>(&self, path: &str, query: &[(&str, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self.request(Method::GET, path, query, None::<&serde_json::Value>)
    }

    fn post<T, S>(&self, path: &str, body: &S) -> Result<T>
    where
        T: DeserializeOwned,
        S: Serialize,
    {
        self.request(Method::POST, path, &[], Some(body))
    }

    fn delete(&self, path: &str) -> Result<()> {
        let response = self
            .client
            .request(Method::DELETE, format!("{ASC_BASE_URL}{path}"))
            .bearer_auth(self.jwt_token()?)
            .send()
            .with_context(|| format!("failed to call App Store Connect `{path}`"))?;
        handle_empty_response(response)
    }

    fn request<T, S>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&S>,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        S: Serialize,
    {
        let url = build_url(path, query)?;
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(self.jwt_token()?);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .with_context(|| format!("failed to call App Store Connect `{path}`"))?;
        handle_json_response(response)
    }

    fn jwt_token(&self) -> Result<String> {
        let private_key = std::fs::read(&self.auth.api_key_path).with_context(|| {
            format!(
                "failed to read App Store Connect API key {}",
                self.auth.api_key_path.display()
            )
        })?;
        let encoding_key = EncodingKey::from_ec_pem(&private_key)
            .context("failed to parse App Store Connect API private key")?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let claims = AscClaims {
            iss: &self.auth.issuer_id,
            aud: "appstoreconnect-v1",
            iat: now,
            exp: now + 20 * 60,
        };
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.auth.key_id.clone());
        encode(&header, &claims, &encoding_key).context("failed to mint App Store Connect JWT")
    }
}

fn build_url(path: &str, query: &[(&str, String)]) -> Result<Url> {
    let mut url = Url::parse(&format!("{ASC_BASE_URL}{path}"))
        .with_context(|| format!("failed to build App Store Connect URL for `{path}`"))?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
        drop(pairs);
    }
    Ok(url)
}

fn handle_json_response<T>(response: Response) -> Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let body = response
        .text()
        .context("failed to read App Store Connect response body")?;
    if !status.is_success() {
        if let Ok(errors) = serde_json::from_str::<AscErrorDocument>(&body) {
            let message = errors
                .errors
                .into_iter()
                .map(|error| {
                    format!(
                        "[{}:{}] {} {}",
                        error.status.unwrap_or_else(|| "?".to_owned()),
                        error.code.unwrap_or_else(|| "?".to_owned()),
                        error.title.unwrap_or_default(),
                        error.detail.unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            bail!("{message}");
        }
        bail!("App Store Connect request failed with {status}: {body}");
    }
    serde_json::from_str(&body).context("failed to parse App Store Connect response")
}

fn handle_empty_response(response: Response) -> Result<()> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response
        .text()
        .context("failed to read App Store Connect response body")?;
    if let Ok(errors) = serde_json::from_str::<AscErrorDocument>(&body) {
        let message = errors
            .errors
            .into_iter()
            .map(|error| {
                format!(
                    "[{}:{}] {} {}",
                    error.status.unwrap_or_else(|| "?".to_owned()),
                    error.code.unwrap_or_else(|| "?".to_owned()),
                    error.title.unwrap_or_default(),
                    error.detail.unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        bail!("{message}");
    }
    bail!("App Store Connect request failed with {status}: {body}");
}
