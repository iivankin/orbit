use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;

use crate::apple::asc_api::{
    CertificateAttributes, DeviceAttributes, IncludedResource, JsonApiDocument,
    JsonApiListDocument, ProfileAttributes, RelationshipData, Resource, ResourceLink,
};
use crate::apple::capabilities::{
    CapabilityRelationships, CapabilityUpdate, RemoteCapability, RemoteCapabilityOption,
    RemoteCapabilitySetting,
};
use crate::apple::developer_services::DeveloperServicesClient;
use crate::context::AppContext;

const SETTING_ICLOUD_VERSION: &str = "ICLOUD_VERSION";
const SETTING_DATA_PROTECTION: &str = "DATA_PROTECTION_PERMISSION_LEVEL";
const SETTING_APPLE_ID_AUTH: &str = "TIBURON_APP_CONSENT";
const SETTING_PUSH_NOTIFICATIONS: &str = "PUSH_NOTIFICATION_FEATURES";

#[derive(Debug)]
pub struct ProvisioningClient {
    developer_services: DeveloperServicesClient,
    team_id: String,
}

#[derive(Debug, Clone)]
pub struct ProvisioningBundleId {
    pub id: String,
    pub name: String,
    pub identifier: String,
    pub seed_id: String,
    pub bundle_type: String,
    pub has_exclusive_managed_capabilities: bool,
    pub capabilities: Vec<crate::apple::capabilities::RemoteCapability>,
}

#[derive(Debug, Clone)]
pub struct ProvisioningAppGroup {
    pub id: String,
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone)]
pub struct ProvisioningMerchantId {
    pub id: String,
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone)]
pub struct ProvisioningCloudContainer {
    pub id: String,
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone)]
pub struct ProvisioningCertificate {
    pub id: String,
    pub certificate_type: String,
    pub display_name: Option<String>,
    pub serial_number: Option<String>,
    pub certificate_content: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProvisioningDevice {
    pub id: String,
    pub name: String,
    pub udid: String,
    pub platform: String,
    pub device_class: Option<String>,
    pub status: Option<String>,
    pub model: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProvisioningProfile {
    pub id: String,
    pub name: String,
    pub profile_type: String,
    pub uuid: Option<String>,
    pub profile_content: Option<String>,
    pub bundle_id_id: Option<String>,
    pub bundle_id_identifier: Option<String>,
    pub certificate_ids: Vec<String>,
    pub device_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvisioningCapabilityPatch {
    pub remote_id: Option<String>,
    pub update: CapabilityUpdate,
}

#[derive(Debug, Clone, Deserialize)]
struct BundleIdAttributes {
    pub name: String,
    pub identifier: String,
    #[serde(rename = "seedId")]
    pub seed_id: String,
    #[serde(rename = "bundleType", default)]
    pub bundle_type: Option<String>,
    #[serde(rename = "hasExclusiveManagedCapabilities", default)]
    pub has_exclusive_managed_capabilities: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct BundleIdCapabilityAttributes {
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
    pub fn authenticate(app: &AppContext, team_id: impl Into<String>) -> Result<Self> {
        Ok(Self {
            developer_services: DeveloperServicesClient::authenticate(app)?,
            team_id: team_id.into(),
        })
    }

    pub fn find_bundle_id(&mut self, identifier: &str) -> Result<Option<ProvisioningBundleId>> {
        let response: JsonApiListDocument<BundleIdAttributes> = self.request_json(
            Method::GET,
            "bundleIds",
            &[
                ("limit", "200".to_owned()),
                ("filter[bundleType]", "bundle".to_owned()),
                ("filter[identifier]", format!("{identifier},*")),
            ],
            None,
        )?;

        let Some(bundle_id) = response
            .data
            .into_iter()
            .find(|candidate| candidate.attributes.identifier == identifier)
        else {
            return Ok(None);
        };
        let capabilities = self.fetch_bundle_capabilities(&bundle_id.id)?;
        Ok(Some(ProvisioningBundleId {
            id: bundle_id.id,
            name: bundle_id.attributes.name,
            identifier: bundle_id.attributes.identifier,
            seed_id: bundle_id.attributes.seed_id,
            bundle_type: bundle_id
                .attributes
                .bundle_type
                .unwrap_or_else(|| "bundle".to_owned()),
            has_exclusive_managed_capabilities: bundle_id
                .attributes
                .has_exclusive_managed_capabilities
                .unwrap_or(false),
            capabilities,
        }))
    }

    pub fn ensure_bundle_id(
        &mut self,
        name: &str,
        identifier: &str,
    ) -> Result<ProvisioningBundleId> {
        if let Some(bundle_id) = self.find_bundle_id(identifier)? {
            return Ok(bundle_id);
        }
        self.create_bundle_id(name, identifier)
    }

    pub fn create_bundle_id(
        &mut self,
        name: &str,
        identifier: &str,
    ) -> Result<ProvisioningBundleId> {
        let request = json!({
            "data": {
                "type": "bundleIds",
                "attributes": {
                    "name": name,
                    "identifier": identifier,
                    "seedId": &self.team_id,
                    "bundleType": "bundle",
                    "hasExclusiveManagedCapabilities": false,
                }
            }
        });
        let _: JsonApiDocument<BundleIdAttributes> =
            self.request_json(Method::POST, "bundleIds", &[], Some(request))?;
        self.find_bundle_id(identifier)?.with_context(|| {
            format!(
                "created Apple bundle identifier `{identifier}` but failed to reload it from Developer Services"
            )
        })
    }

    pub fn list_app_groups(&mut self) -> Result<Vec<ProvisioningAppGroup>> {
        let response: JsonApiListDocument<AppGroupAttributes> = self.request_json(
            Method::GET,
            "appGroups",
            &[("limit", "200".to_owned())],
            None,
        )?;
        Ok(response
            .data
            .into_iter()
            .map(|group| ProvisioningAppGroup {
                id: group.id,
                name: group.attributes.name,
                identifier: group.attributes.identifier,
            })
            .collect())
    }

    pub fn create_app_group(
        &mut self,
        name: &str,
        identifier: &str,
    ) -> Result<ProvisioningAppGroup> {
        let request = json!({
            "data": {
                "type": "appGroups",
                "attributes": {
                    "name": name,
                    "identifier": identifier,
                }
            }
        });
        let response: JsonApiDocument<AppGroupAttributes> =
            self.request_json(Method::POST, "appGroups", &[], Some(request))?;
        Ok(ProvisioningAppGroup {
            id: response.data.id,
            name: response.data.attributes.name,
            identifier: response.data.attributes.identifier,
        })
    }

    pub fn list_merchant_ids(&mut self) -> Result<Vec<ProvisioningMerchantId>> {
        let response: JsonApiListDocument<MerchantIdAttributes> = self.request_json(
            Method::GET,
            "merchantIds",
            &[("limit", "200".to_owned())],
            None,
        )?;
        Ok(response
            .data
            .into_iter()
            .map(|merchant| ProvisioningMerchantId {
                id: merchant.id,
                name: merchant.attributes.name,
                identifier: merchant.attributes.identifier,
            })
            .collect())
    }

    pub fn create_merchant_id(
        &mut self,
        name: &str,
        identifier: &str,
    ) -> Result<ProvisioningMerchantId> {
        let request = json!({
            "data": {
                "type": "merchantIds",
                "attributes": {
                    "name": name,
                    "identifier": identifier,
                }
            }
        });
        let response: JsonApiDocument<MerchantIdAttributes> =
            self.request_json(Method::POST, "merchantIds", &[], Some(request))?;
        Ok(ProvisioningMerchantId {
            id: response.data.id,
            name: response.data.attributes.name,
            identifier: response.data.attributes.identifier,
        })
    }

    pub fn delete_merchant_id(&mut self, merchant_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            &format!("merchantIds/{merchant_id}"),
            &[],
            None,
        )
    }

    pub fn list_cloud_containers(&mut self) -> Result<Vec<ProvisioningCloudContainer>> {
        let response: JsonApiListDocument<CloudContainerAttributes> = self.request_json(
            Method::GET,
            "cloudContainers",
            &[("limit", "200".to_owned())],
            None,
        )?;
        Ok(response
            .data
            .into_iter()
            .map(|container| ProvisioningCloudContainer {
                id: container.id,
                name: container.attributes.name,
                identifier: container.attributes.identifier,
            })
            .collect())
    }

    pub fn create_cloud_container(
        &mut self,
        name: &str,
        identifier: &str,
    ) -> Result<ProvisioningCloudContainer> {
        let request = json!({
            "data": {
                "type": "cloudContainers",
                "attributes": {
                    "name": name,
                    "identifier": identifier,
                }
            }
        });
        let response: JsonApiDocument<CloudContainerAttributes> =
            self.request_json(Method::POST, "cloudContainers", &[], Some(request))?;
        Ok(ProvisioningCloudContainer {
            id: response.data.id,
            name: response.data.attributes.name,
            identifier: response.data.attributes.identifier,
        })
    }

    pub fn delete_cloud_container(&mut self, cloud_container_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            &format!("cloudContainers/{cloud_container_id}"),
            &[],
            None,
        )
    }

    pub fn list_certificates(
        &mut self,
        certificate_type: &str,
    ) -> Result<Vec<ProvisioningCertificate>> {
        let response: JsonApiListDocument<CertificateAttributes> = self.request_json(
            Method::GET,
            "certificates",
            &[
                ("limit", "200".to_owned()),
                ("filter[certificateType]", certificate_type.to_owned()),
            ],
            None,
        )?;
        Ok(response
            .data
            .into_iter()
            .map(|certificate| ProvisioningCertificate {
                id: certificate.id,
                certificate_type: certificate.attributes.certificate_type,
                display_name: certificate.attributes.display_name,
                serial_number: certificate.attributes.serial_number,
                certificate_content: certificate.attributes.certificate_content,
            })
            .collect())
    }

    pub fn create_certificate(
        &mut self,
        certificate_type: &str,
        csr_content: &str,
        machine_id: Option<&str>,
        machine_name: Option<&str>,
    ) -> Result<ProvisioningCertificate> {
        let mut attributes = json!({
            "certificateType": certificate_type,
            "csrContent": csr_content,
        });
        if let Some(machine_id) = machine_id {
            attributes["machineId"] = json!(machine_id);
        }
        if let Some(machine_name) = machine_name {
            attributes["machineName"] = json!(machine_name);
        }
        let request = json!({
            "data": {
                "type": "certificates",
                "attributes": attributes
            }
        });
        let response: JsonApiDocument<CertificateAttributes> =
            self.request_json(Method::POST, "certificates", &[], Some(request))?;
        Ok(ProvisioningCertificate {
            id: response.data.id,
            certificate_type: response.data.attributes.certificate_type,
            display_name: response.data.attributes.display_name,
            serial_number: response.data.attributes.serial_number,
            certificate_content: response.data.attributes.certificate_content,
        })
    }

    pub fn delete_certificate(&mut self, certificate_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            &format!("certificates/{certificate_id}"),
            &[],
            None,
        )
    }

    pub fn list_devices(&mut self) -> Result<Vec<ProvisioningDevice>> {
        let response: JsonApiListDocument<DeviceAttributes> = self.request_json(
            Method::GET,
            "devices",
            &[
                ("limit", "200".to_owned()),
                ("filter[status]", "ENABLED".to_owned()),
            ],
            None,
        )?;
        Ok(response
            .data
            .into_iter()
            .map(|device| ProvisioningDevice {
                id: device.id,
                name: device.attributes.name,
                udid: device.attributes.udid,
                platform: device.attributes.platform,
                device_class: device.attributes.device_class,
                status: device.attributes.status,
                model: device.attributes.model,
                created_at: device.attributes.added_date,
            })
            .collect())
    }

    pub fn find_device_by_udid(&mut self, udid: &str) -> Result<Option<ProvisioningDevice>> {
        let response: JsonApiListDocument<DeviceAttributes> = self.request_json(
            Method::GET,
            "devices",
            &[
                ("limit", "200".to_owned()),
                ("filter[udid]", udid.to_owned()),
                ("filter[status]", "ENABLED".to_owned()),
            ],
            None,
        )?;
        Ok(response
            .data
            .into_iter()
            .next()
            .map(|device| ProvisioningDevice {
                id: device.id,
                name: device.attributes.name,
                udid: device.attributes.udid,
                platform: device.attributes.platform,
                device_class: device.attributes.device_class,
                status: device.attributes.status,
                model: device.attributes.model,
                created_at: device.attributes.added_date,
            }))
    }

    pub fn create_device(
        &mut self,
        name: &str,
        udid: &str,
        platform: &str,
    ) -> Result<ProvisioningDevice> {
        let request = json!({
            "data": {
                "type": "devices",
                "attributes": {
                    "name": name,
                    "udid": udid,
                    "platform": platform,
                }
            }
        });
        let response: JsonApiDocument<DeviceAttributes> =
            self.request_json(Method::POST, "devices", &[], Some(request))?;
        Ok(ProvisioningDevice {
            id: response.data.id,
            name: response.data.attributes.name,
            udid: response.data.attributes.udid,
            platform: response.data.attributes.platform,
            device_class: response.data.attributes.device_class,
            status: response.data.attributes.status,
            model: response.data.attributes.model,
            created_at: response.data.attributes.added_date,
        })
    }

    pub fn delete_device(&mut self, device_id: &str) -> Result<()> {
        self.request_empty(Method::DELETE, &format!("devices/{device_id}"), &[], None)
    }

    pub fn create_profile(
        &mut self,
        profile_type: &str,
        bundle_id_id: &str,
    ) -> Result<ProvisioningProfile> {
        let request = json!({
            "data": {
                "type": "profiles",
                "attributes": {
                    "profileType": profile_type,
                },
                "relationships": {
                    "bundleId": {
                        "data": {
                            "id": bundle_id_id,
                            "type": "bundleIds",
                        }
                    }
                }
            }
        });
        let response: JsonApiDocument<ProfileAttributes> =
            self.request_json(Method::POST, "profiles", &[], Some(request))?;
        Ok(parse_provisioning_profile(response.data, &HashMap::new()))
    }

    pub fn list_profiles(
        &mut self,
        profile_type: Option<&str>,
    ) -> Result<Vec<ProvisioningProfile>> {
        let mut query = vec![
            ("limit", "200".to_owned()),
            ("filter[profileState]", "ACTIVE".to_owned()),
            ("include", "bundleId,devices,certificates".to_owned()),
        ];
        if let Some(profile_type) = profile_type {
            query.push(("filter[profileType]", profile_type.to_owned()));
        }
        let response: JsonApiListDocument<ProfileAttributes> =
            self.request_json(Method::GET, "profiles", &query, None)?;
        parse_provisioning_profiles(response)
    }

    pub fn delete_profile(&mut self, profile_id: &str) -> Result<()> {
        self.request_empty(Method::DELETE, &format!("profiles/{profile_id}"), &[], None)
    }

    pub fn delete_bundle_capability(&mut self, capability_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            &format!("bundleIdCapabilities/{capability_id}"),
            &[],
            None,
        )
    }

    pub fn delete_bundle_id(&mut self, bundle_id_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            &format!("bundleIds/{bundle_id_id}"),
            &[],
            None,
        )
    }

    pub fn delete_app_group(&mut self, app_group_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            &format!("appGroups/{app_group_id}"),
            &[],
            None,
        )
    }

    pub fn update_bundle_capabilities(
        &mut self,
        bundle_id: &ProvisioningBundleId,
        updates: &[ProvisioningCapabilityPatch],
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
                    "teamId": &self.team_id,
                    "name": &bundle_id.name,
                    "identifier": &bundle_id.identifier,
                    "seedId": &bundle_id.seed_id,
                    "bundleType": &bundle_id.bundle_type,
                    "hasExclusiveManagedCapabilities": bundle_id.has_exclusive_managed_capabilities,
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
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.developer_services
            .request_json(method, path, query, Some(self.team_id.as_str()), body)
    }

    fn request_empty(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<()> {
        self.developer_services.request_empty(
            method,
            path,
            query,
            Some(self.team_id.as_str()),
            body,
        )
    }

    fn fetch_bundle_capabilities(&mut self, bundle_id_id: &str) -> Result<Vec<RemoteCapability>> {
        let response: JsonApiDocument<BundleIdAttributes> = self.request_json(
            Method::GET,
            &format!("bundleIds/{bundle_id_id}"),
            &[(
                "include",
                "bundleIdCapabilities.capability,bundleIdCapabilities.appGroups,bundleIdCapabilities.cloudContainers,bundleIdCapabilities.merchantIds,bundleIdCapabilities.associatedBundleIds".to_owned(),
            )],
            None,
        )?;
        response
            .included
            .into_iter()
            .filter(|resource| resource.resource_type == "bundleIdCapabilities")
            .map(parse_remote_capability)
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AppGroupAttributes {
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MerchantIdAttributes {
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CloudContainerAttributes {
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileBundleIdAttributes {
    pub identifier: String,
}

fn parse_provisioning_profile(
    resource: Resource<ProfileAttributes>,
    bundle_ids_by_id: &HashMap<String, String>,
) -> ProvisioningProfile {
    let bundle_id_id = relationship_one_id_from_relationships(&resource.relationships, "bundleId");
    ProvisioningProfile {
        id: resource.id,
        name: resource.attributes.name,
        profile_type: resource.attributes.profile_type,
        uuid: resource.attributes.uuid,
        profile_content: resource.attributes.profile_content,
        bundle_id_identifier: bundle_id_id
            .as_ref()
            .and_then(|id| bundle_ids_by_id.get(id).cloned()),
        bundle_id_id,
        certificate_ids: relationship_many_ids(&resource.relationships, "certificates"),
        device_ids: relationship_many_ids(&resource.relationships, "devices"),
    }
}

fn parse_provisioning_profiles(
    response: JsonApiListDocument<ProfileAttributes>,
) -> Result<Vec<ProvisioningProfile>> {
    let bundle_ids_by_id = response
        .included
        .into_iter()
        .filter(|resource| resource.resource_type == "bundleIds")
        .map(|resource| {
            let attributes: ProfileBundleIdAttributes = serde_json::from_value(resource.attributes)
                .context("failed to parse Developer Services profile bundle identifier")?;
            Ok((resource.id, attributes.identifier))
        })
        .collect::<Result<HashMap<_, _>>>()?;

    Ok(response
        .data
        .into_iter()
        .map(|resource| parse_provisioning_profile(resource, &bundle_ids_by_id))
        .collect())
}

fn parse_remote_capability(resource: IncludedResource) -> Result<RemoteCapability> {
    let capability_type = relationship_one_id(&resource, "capability").unwrap_or_default();
    let attributes: BundleIdCapabilityAttributes = serde_json::from_value(resource.attributes)
        .context("failed to parse Developer Services bundle capability attributes")?;
    Ok(RemoteCapability {
        id: resource.id,
        capability_type,
        enabled: attributes.enabled,
        settings: attributes
            .settings
            .unwrap_or_default()
            .into_iter()
            .map(|setting| RemoteCapabilitySetting {
                key: setting.key,
                options: setting
                    .options
                    .into_iter()
                    .map(|option| RemoteCapabilityOption {
                        key: option.key,
                        enabled: option.enabled.unwrap_or(false),
                    })
                    .collect(),
            })
            .collect(),
    })
}

fn relationship_one_id(resource: &IncludedResource, key: &str) -> Option<String> {
    let relationship = resource.relationships.get(key)?;
    match relationship.data.as_ref()? {
        RelationshipData::One(ResourceLink { id, .. }) => Some(id.clone()),
        RelationshipData::Many(_) => None,
    }
}

fn relationship_one_id_from_relationships(
    relationships: &std::collections::HashMap<String, crate::apple::asc_api::Relationship>,
    key: &str,
) -> Option<String> {
    let relationship = relationships.get(key)?;
    match relationship.data.as_ref()? {
        RelationshipData::One(ResourceLink { id, .. }) => Some(id.clone()),
        RelationshipData::Many(_) => None,
    }
}

fn relationship_many_ids(
    relationships: &std::collections::HashMap<String, crate::apple::asc_api::Relationship>,
    key: &str,
) -> Vec<String> {
    let Some(relationship) = relationships.get(key) else {
        return Vec::new();
    };
    match relationship.data.as_ref() {
        Some(RelationshipData::Many(links)) => links.iter().map(|link| link.id.clone()).collect(),
        Some(RelationshipData::One(link)) => vec![link.id.clone()],
        None => Vec::new(),
    }
}

fn build_capability_relationship(
    update: &ProvisioningCapabilityPatch,
) -> Result<serde_json::Value> {
    let mut relationships = capability_relationships(&update.update.relationships);
    relationships.insert(
        "capability".to_owned(),
        json!({
            "data": {
                "id": update.update.capability_type,
                "type": "capabilities",
            }
        }),
    );
    let mut value = json!({
        "attributes": {
            "enabled": update.update.option != "OFF",
            "settings": capability_settings(&update.update)?,
        },
        "relationships": relationships,
        "type": "bundleIdCapabilities",
    });
    if let Some(remote_id) = &update.remote_id {
        value["id"] = json!(remote_id);
    }
    Ok(value)
}

fn capability_settings(update: &CapabilityUpdate) -> Result<Vec<serde_json::Value>> {
    if update.option.is_empty() || update.option == "OFF" {
        return Ok(Vec::new());
    }

    let key = match update.capability_type.as_str() {
        "ICLOUD" => SETTING_ICLOUD_VERSION,
        "DATA_PROTECTION" => SETTING_DATA_PROTECTION,
        "APPLE_ID_AUTH" => SETTING_APPLE_ID_AUTH,
        "PUSH_NOTIFICATIONS" => {
            if update.option == "ON" {
                return Ok(Vec::new());
            }
            SETTING_PUSH_NOTIFICATIONS
        }
        _ => {
            if update.option == "ON" {
                return Ok(Vec::new());
            }
            bail!(
                "unsupported capability option `{}` for {}",
                update.option,
                update.capability_type
            )
        }
    };

    Ok(vec![json!({
        "key": key,
        "options": [
            {
                "key": update.option,
                "enabled": true,
            }
        ]
    })])
}

fn capability_relationships(
    relationships: &CapabilityRelationships,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();

    if let Some(app_groups) = &relationships.app_groups {
        map.insert(
            "appGroups".to_owned(),
            json!({
                "data": app_groups
                    .iter()
                    .map(|id| json!({ "id": id, "type": "appGroups" }))
                    .collect::<Vec<_>>(),
            }),
        );
    }

    if let Some(merchant_ids) = &relationships.merchant_ids {
        map.insert(
            "merchantIds".to_owned(),
            json!({
                "data": merchant_ids
                    .iter()
                    .map(|id| json!({ "id": id, "type": "merchantIds" }))
                    .collect::<Vec<_>>(),
            }),
        );
    }

    if let Some(cloud_containers) = &relationships.cloud_containers {
        map.insert(
            "cloudContainers".to_owned(),
            json!({
                "data": cloud_containers
                    .iter()
                    .map(|id| json!({ "id": id, "type": "cloudContainers" }))
                    .collect::<Vec<_>>(),
            }),
        );
    }

    map
}

#[cfg(test)]
mod tests {
    use super::parse_provisioning_profiles;
    use crate::apple::asc_api::JsonApiListDocument;

    #[test]
    fn parses_profile_bundle_identifier_from_included_bundle_id() {
        let response: JsonApiListDocument<crate::apple::asc_api::ProfileAttributes> =
            serde_json::from_value(serde_json::json!({
                "data": [
                    {
                        "id": "PROFILE123",
                        "type": "profiles",
                        "attributes": {
                            "name": "Orbit Development",
                            "profileType": "IOS_APP_DEVELOPMENT",
                            "profileState": "ACTIVE",
                            "profileContent": "c29tZS1wcm9maWxl",
                            "uuid": "UUID-123"
                        },
                        "relationships": {
                            "bundleId": {
                                "data": { "id": "BUNDLE123", "type": "bundleIds" }
                            },
                            "certificates": {
                                "data": [{ "id": "CERT123", "type": "certificates" }]
                            },
                            "devices": {
                                "data": [{ "id": "DEVICE123", "type": "devices" }]
                            }
                        }
                    }
                ],
                "included": [
                    {
                        "id": "BUNDLE123",
                        "type": "bundleIds",
                        "attributes": {
                            "identifier": "dev.orbit.example"
                        }
                    }
                ]
            }))
            .unwrap();

        let profiles = parse_provisioning_profiles(response).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].bundle_id_id.as_deref(), Some("BUNDLE123"));
        assert_eq!(
            profiles[0].bundle_id_identifier.as_deref(),
            Some("dev.orbit.example")
        );
        assert_eq!(profiles[0].certificate_ids, vec!["CERT123"]);
        assert_eq!(profiles[0].device_ids, vec!["DEVICE123"]);
    }
}
