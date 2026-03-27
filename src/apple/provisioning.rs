use std::io::BufReader;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use cookie_store::serde::json::load as load_cookie_store_json;
use reqwest::Method;
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use reqwest_cookie_store::CookieStoreMutex;
use serde::Deserialize;
use serde_json::json;

use crate::apple::apple_id::StoredAppleSession;
use crate::apple::asc_api::{IncludedResource, JsonApiListDocument};

const PROVISIONING_BASE_URL: &str = "https://developer.apple.com/services-account/v1/";
const PROVISIONING_CONTENT_TYPE: &str = "application/vnd.api+json";
const SETTING_ICLOUD_VERSION: &str = "ICLOUD_VERSION";
const SETTING_DATA_PROTECTION: &str = "DATA_PROTECTION_PERMISSION_LEVEL";
const SETTING_APPLE_ID_AUTH: &str = "TIBURON_APP_CONSENT";
const SETTING_PUSH_NOTIFICATIONS: &str = "PUSH_NOTIFICATION_FEATURES";

#[derive(Debug, Clone)]
pub struct ProvisioningClient {
    client: Client,
    team_id: String,
}

#[derive(Debug, Clone)]
pub struct ProvisioningBundleId {
    pub id: String,
    pub name: String,
    pub identifier: String,
    pub seed_id: String,
    pub capabilities: Vec<crate::apple::capabilities::RemoteCapability>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvisioningCapabilityUpdate {
    pub capability_type: String,
    pub option: String,
    pub relationships: ProvisioningCapabilityRelationships,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvisioningCapabilityRelationships {
    pub app_groups: Option<Vec<String>>,
    pub merchant_ids: Option<Vec<String>>,
    pub cloud_containers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
struct BundleIdAttributes {
    pub name: String,
    pub identifier: String,
    #[serde(rename = "seedId")]
    pub seed_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct BundleIdCapabilityAttributes {
    #[serde(rename = "capabilityType", default)]
    pub capability_type: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub settings: Option<Vec<CapabilitySetting>>,
}

#[derive(Debug, Clone, Deserialize)]
struct CapabilitySetting {
    pub key: String,
    #[serde(default)]
    pub options: Vec<CapabilitySettingOption>,
}

#[derive(Debug, Clone, Deserialize)]
struct CapabilitySettingOption {
    pub key: String,
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl ProvisioningClient {
    pub fn from_session(session: &StoredAppleSession, team_id: impl Into<String>) -> Result<Self> {
        let reader = BufReader::new(session.cookies_json.as_bytes());
        let cookie_store = load_cookie_store_json(reader).map_err(|error| {
            anyhow::anyhow!("failed to parse stored Apple session cookies: {error}")
        })?;
        let cookie_store = Arc::new(CookieStoreMutex::new(cookie_store));
        let client = ClientBuilder::new()
            .cookie_provider(cookie_store)
            .user_agent(format!("orbit/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to create the Apple provisioning HTTP client")?;
        Ok(Self {
            client,
            team_id: team_id.into(),
        })
    }

    pub fn find_bundle_id(&self, identifier: &str) -> Result<Option<ProvisioningBundleId>> {
        let response: JsonApiListDocument<BundleIdAttributes> = self.request_json(
            Method::GET,
            "bundleIds",
            &[
                ("limit", "200".to_owned()),
                ("filter[identifier]", identifier.to_owned()),
                ("include", "bundleIdCapabilities".to_owned()),
                (
                    "fields[bundleIds]",
                    "name,identifier,seedId,bundleIdCapabilities".to_owned(),
                ),
                (
                    "fields[bundleIdCapabilities]",
                    "capabilityType,enabled,settings".to_owned(),
                ),
            ],
            None,
        )?;

        let Some(bundle_id) = response.data.into_iter().next() else {
            return Ok(None);
        };
        let capabilities = response
            .included
            .into_iter()
            .filter(|resource| resource.resource_type == "bundleIdCapabilities")
            .map(parse_remote_capability)
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(ProvisioningBundleId {
            id: bundle_id.id,
            name: bundle_id.attributes.name,
            identifier: bundle_id.attributes.identifier,
            seed_id: bundle_id.attributes.seed_id,
            capabilities,
        }))
    }

    pub fn update_bundle_capabilities(
        &self,
        bundle_id: &ProvisioningBundleId,
        updates: &[ProvisioningCapabilityUpdate],
    ) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }

        let relationships = updates
            .iter()
            .map(build_capability_relationship)
            .collect::<Result<Vec<_>>>()?;
        let request = json!({
            "data": {
                "id": &bundle_id.id,
                "type": "bundleIds",
                "attributes": {
                    "name": &bundle_id.name,
                    "identifier": &bundle_id.identifier,
                    "seedId": &bundle_id.seed_id,
                },
                "relationships": {
                    "bundleIdCapabilities": {
                        "data": relationships,
                    }
                }
            }
        });

        let _: serde_json::Value = self.request_json(
            Method::PATCH,
            &format!("bundleIds/{}", bundle_id.id),
            &[],
            Some(request),
        )?;
        Ok(())
    }

    fn request_json<T>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self.send(method.clone(), path, query, body)?;
        let status = response.status();
        let bytes = response
            .bytes()
            .context("failed to read Apple provisioning response body")?;
        if !status.is_success() {
            bail!(
                "Apple provisioning request `{path}` failed with {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse Apple provisioning response for `{path}`"))
    }

    fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::blocking::Response> {
        let url = format!("{PROVISIONING_BASE_URL}{path}");
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(PROVISIONING_CONTENT_TYPE),
        );
        headers.insert(
            "X-Requested-With",
            HeaderValue::from_static("XMLHttpRequest"),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&format!("orbit/{}", env!("CARGO_PKG_VERSION")))?,
        );

        let (actual_method, payload) = if matches!(method, Method::GET | Method::DELETE) {
            headers.insert(
                "X-HTTP-Method-Override",
                HeaderValue::from_str(method.as_str())?,
            );
            (
                Method::POST,
                json!({
                    "urlEncodedQueryParams": encode_query(query)?,
                    "teamId": self.team_id.clone(),
                }),
            )
        } else {
            let mut payload =
                body.context("Apple provisioning write request must include a JSON body")?;
            payload["data"]["attributes"]["teamId"] =
                serde_json::Value::String(self.team_id.clone());
            (method, payload)
        };

        self.client
            .request(actual_method, url)
            .headers(headers)
            .json(&payload)
            .send()
            .with_context(|| format!("failed to call Apple provisioning endpoint `{path}`"))
    }
}

fn parse_remote_capability(
    resource: IncludedResource,
) -> Result<crate::apple::capabilities::RemoteCapability> {
    let attributes: BundleIdCapabilityAttributes = serde_json::from_value(resource.attributes)
        .context("failed to parse bundle capability attributes")?;
    Ok(crate::apple::capabilities::RemoteCapability {
        id: resource.id.clone(),
        capability_type: attributes.capability_type.unwrap_or_else(|| {
            resource
                .id
                .split_once('_')
                .map(|(_, suffix)| suffix.to_owned())
                .unwrap_or_default()
        }),
        enabled: attributes.enabled,
        settings: attributes
            .settings
            .unwrap_or_default()
            .into_iter()
            .map(
                |setting| crate::apple::capabilities::RemoteCapabilitySetting {
                    key: setting.key,
                    options: setting
                        .options
                        .into_iter()
                        .map(
                            |option| crate::apple::capabilities::RemoteCapabilityOption {
                                key: option.key,
                                enabled: option.enabled.unwrap_or(false),
                            },
                        )
                        .collect(),
                },
            )
            .collect(),
    })
}

fn build_capability_relationship(
    update: &ProvisioningCapabilityUpdate,
) -> Result<serde_json::Value> {
    let enabled = update.option != "OFF";
    let mut relationships = json!({
        "capability": {
            "data": {
                "id": update.capability_type,
                "type": "capabilities",
            }
        }
    });

    if let Some(ids) = &update.relationships.app_groups {
        relationships["appGroups"] = json_relationship("appGroups", ids);
    }
    if let Some(ids) = &update.relationships.merchant_ids {
        relationships["merchantIds"] = json_relationship("merchantIds", ids);
    }
    if let Some(ids) = &update.relationships.cloud_containers {
        relationships["cloudContainers"] = json_relationship("cloudContainers", ids);
    }

    let mut settings = Vec::new();
    if enabled {
        if let Some((key, option)) = capability_setting(update)? {
            settings.push(json!({
                "key": key,
                "options": [
                    {
                        "key": option,
                        "enabled": true,
                    }
                ]
            }));
        }
    }

    Ok(json!({
        "type": "bundleIdCapabilities",
        "attributes": {
            "enabled": enabled,
            "settings": settings,
        },
        "relationships": relationships,
    }))
}

fn capability_setting(
    update: &ProvisioningCapabilityUpdate,
) -> Result<Option<(&'static str, &str)>> {
    match update.capability_type.as_str() {
        "ICLOUD" => match update.option.as_str() {
            "XCODE_5" | "XCODE_6" => Ok(Some((SETTING_ICLOUD_VERSION, update.option.as_str()))),
            "ON" => Ok(Some((SETTING_ICLOUD_VERSION, "XCODE_6"))),
            "OFF" => Ok(None),
            other => bail!(
                "invalid iCloud capability option `{other}`; expected ON, OFF, XCODE_5, or XCODE_6"
            ),
        },
        "DATA_PROTECTION" => match update.option.as_str() {
            "COMPLETE_PROTECTION" | "PROTECTED_UNLESS_OPEN" | "PROTECTED_UNTIL_FIRST_USER_AUTH" => {
                Ok(Some((SETTING_DATA_PROTECTION, update.option.as_str())))
            }
            "ON" => Ok(Some((SETTING_DATA_PROTECTION, "COMPLETE_PROTECTION"))),
            "OFF" => Ok(None),
            other => bail!(
                "invalid data protection capability option `{other}`; expected ON, OFF, COMPLETE_PROTECTION, PROTECTED_UNLESS_OPEN, or PROTECTED_UNTIL_FIRST_USER_AUTH"
            ),
        },
        "APPLE_ID_AUTH" => match update.option.as_str() {
            "PRIMARY_APP_CONSENT" => Ok(Some((SETTING_APPLE_ID_AUTH, update.option.as_str()))),
            "ON" => Ok(Some((SETTING_APPLE_ID_AUTH, "PRIMARY_APP_CONSENT"))),
            "OFF" => Ok(None),
            other => bail!(
                "invalid Sign In with Apple capability option `{other}`; expected ON, OFF, or PRIMARY_APP_CONSENT"
            ),
        },
        "PUSH_NOTIFICATIONS" => match update.option.as_str() {
            "PUSH_NOTIFICATION_FEATURE_BROADCAST" => {
                Ok(Some((SETTING_PUSH_NOTIFICATIONS, update.option.as_str())))
            }
            "ON" | "OFF" => Ok(None),
            other => bail!(
                "invalid push capability option `{other}`; expected ON, OFF, or PUSH_NOTIFICATION_FEATURE_BROADCAST"
            ),
        },
        _ => match update.option.as_str() {
            "ON" | "OFF" => Ok(None),
            other => bail!(
                "invalid capability option `{other}` for `{}`; Orbit only supports ON/OFF for this capability",
                update.capability_type
            ),
        },
    }
}

fn json_relationship(resource_type: &str, ids: &[String]) -> serde_json::Value {
    json!({
        "data": ids
            .iter()
            .map(|id| {
                json!({
                    "type": resource_type,
                    "id": id,
                })
            })
            .collect::<Vec<_>>()
    })
}

fn encode_query(query: &[(&str, String)]) -> Result<String> {
    let url = reqwest::Url::parse_with_params(
        "https://orbit.invalid",
        query.iter().map(|(key, value)| (*key, value.as_str())),
    )
    .context("failed to encode provisioning query parameters")?;
    Ok(url.query().unwrap_or_default().to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ProvisioningCapabilityRelationships, ProvisioningCapabilityUpdate,
        build_capability_relationship,
    };

    const OPTION_DATA_PROTECTION_COMPLETE: &str = "COMPLETE_PROTECTION";

    #[test]
    fn builds_data_protection_relationship() {
        let value = build_capability_relationship(&ProvisioningCapabilityUpdate {
            capability_type: "DATA_PROTECTION".to_owned(),
            option: OPTION_DATA_PROTECTION_COMPLETE.to_owned(),
            relationships: ProvisioningCapabilityRelationships::default(),
        })
        .unwrap();

        assert_eq!(value["attributes"]["enabled"], json!(true));
        assert_eq!(
            value["attributes"]["settings"][0]["key"],
            json!("DATA_PROTECTION_PERMISSION_LEVEL")
        );
        assert_eq!(
            value["attributes"]["settings"][0]["options"][0]["key"],
            json!(OPTION_DATA_PROTECTION_COMPLETE)
        );
    }

    #[test]
    fn builds_identifier_relationships_with_empty_arrays() {
        let value = build_capability_relationship(&ProvisioningCapabilityUpdate {
            capability_type: "APP_GROUPS".to_owned(),
            option: "ON".to_owned(),
            relationships: ProvisioningCapabilityRelationships {
                app_groups: Some(Vec::new()),
                merchant_ids: None,
                cloud_containers: None,
            },
        })
        .unwrap();

        assert_eq!(
            value["relationships"]["appGroups"]["data"],
            serde_json::Value::Array(Vec::new())
        );
    }
}
