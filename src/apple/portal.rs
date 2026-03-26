use std::collections::HashMap;
use std::io::BufReader;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use cookie_store::serde::json::load as load_cookie_store_json;
use reqwest::Method;
use reqwest::Url;
use reqwest::blocking::{Client, ClientBuilder, Response};
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use reqwest_cookie_store::CookieStoreMutex;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::apple::apple_id::StoredAppleSession;

const APPLE_PORTAL_BASE_URL: &str = "https://developer.apple.com/services-account/QH65B2";
const PAGE_SIZE: usize = 200;

#[derive(Debug, Clone)]
pub struct PortalClient {
    client: Client,
    team_id: String,
    csrf_cache: HashMap<CsrfScope, CsrfTokens>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
enum CsrfScope {
    App,
    Device(bool),
    Certificate(bool),
    Merchant(bool),
    AppGroup,
    CloudContainer,
    Provisioning(bool),
}

#[derive(Debug, Clone, Default)]
struct CsrfTokens {
    values: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PortalEnvelope {
    #[serde(rename = "resultCode", default)]
    result_code: Option<i64>,
    #[serde(rename = "resultString", default)]
    result_string: Option<String>,
    #[serde(rename = "userString", default)]
    user_string: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalDevice {
    #[serde(rename = "deviceId")]
    pub id: String,
    pub name: String,
    #[serde(rename = "deviceNumber")]
    pub udid: String,
    #[serde(rename = "devicePlatform", default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(rename = "deviceClass", default)]
    pub device_class: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalAppId {
    #[serde(rename = "appIdId")]
    pub id: String,
    pub name: String,
    pub identifier: String,
    #[serde(rename = "appIdPlatform", default)]
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalCertificate {
    #[serde(rename = "certificateId")]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "serialNumber", default)]
    pub serial_number: Option<String>,
    #[serde(rename = "statusString", default)]
    pub status: Option<String>,
    #[serde(rename = "certificateTypeDisplayId", default)]
    pub certificate_type_display_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalProvisioningProfile {
    #[serde(rename = "provisioningProfileId")]
    pub id: String,
    #[serde(rename = "UUID", default)]
    pub uuid: Option<String>,
    pub name: String,
    #[serde(rename = "distributionMethod")]
    pub distribution_method: String,
    #[serde(rename = "proProPlatform", default)]
    pub platform: Option<String>,
    #[serde(rename = "proProSubPlatform", default)]
    pub sub_platform: Option<String>,
    #[serde(rename = "appId", default)]
    pub app: Option<PortalAppReference>,
    #[serde(default)]
    pub certificates: Vec<PortalCertificateReference>,
    #[serde(default)]
    pub devices: Vec<PortalDeviceReference>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalAppReference {
    #[serde(rename = "appIdId")]
    pub id: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalCertificateReference {
    #[serde(rename = "certificateId")]
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalDeviceReference {
    #[serde(rename = "deviceId")]
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalMerchant {
    #[serde(rename = "omcId")]
    pub id: String,
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalAppGroup {
    #[serde(rename = "applicationGroup")]
    pub id: String,
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortalCloudContainer {
    #[serde(rename = "cloudContainer")]
    pub id: String,
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone)]
pub struct PortalServiceUpdate {
    pub service_id: &'static str,
    pub value: String,
    pub uses_push_uri: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum PortalDeviceClass {
    Iphone,
    Tvos,
    Watch,
    Mac,
}

#[derive(Debug, Clone, Copy)]
pub enum PortalProfilePlatform {
    Ios,
    Tvos,
    Watchos,
    Visionos,
    Macos,
}

impl PortalProfilePlatform {
    fn mac(self) -> bool {
        matches!(self, Self::Macos)
    }

    fn sub_platform(self) -> Option<&'static str> {
        match self {
            Self::Tvos => Some("tvOS"),
            Self::Ios | Self::Watchos | Self::Visionos | Self::Macos => None,
        }
    }
}

impl PortalClient {
    pub fn from_session(session: &StoredAppleSession, team_id: impl Into<String>) -> Result<Self> {
        let reader = BufReader::new(session.cookies_json.as_bytes());
        let cookie_store = load_cookie_store_json(reader)
            .map_err(|error| anyhow!("failed to parse stored Apple session cookies: {error}"))?;
        let cookie_store = Arc::new(CookieStoreMutex::new(cookie_store));
        let client = ClientBuilder::new()
            .cookie_provider(cookie_store)
            .user_agent(format!("orbit/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to create the Apple portal HTTP client")?;
        Ok(Self {
            client,
            team_id: team_id.into(),
            csrf_cache: HashMap::new(),
        })
    }

    pub fn list_devices(
        &mut self,
        device_class: PortalDeviceClass,
        include_disabled: bool,
    ) -> Result<Vec<PortalDevice>> {
        match device_class {
            PortalDeviceClass::Iphone | PortalDeviceClass::Mac => {
                let mac = matches!(device_class, PortalDeviceClass::Mac);
                self.paged_post_collection(
                    &format!("account/{}/device/listDevices.action", platform_slug(mac)),
                    "devices",
                    vec![
                        ("teamId", self.team_id.clone()),
                        ("sort", "name=asc".to_owned()),
                        (
                            "includeRemovedDevices",
                            if include_disabled { "true" } else { "false" }.to_owned(),
                        ),
                    ],
                    Some(CsrfScope::Device(mac)),
                )
            }
            PortalDeviceClass::Tvos | PortalDeviceClass::Watch => {
                let class = match device_class {
                    PortalDeviceClass::Tvos => "tvOS",
                    PortalDeviceClass::Watch => "watch",
                    PortalDeviceClass::Iphone | PortalDeviceClass::Mac => unreachable!(),
                };
                self.paged_post_collection(
                    "account/ios/device/listDevices.action",
                    "devices",
                    vec![
                        ("teamId", self.team_id.clone()),
                        ("sort", "name=asc".to_owned()),
                        ("deviceClasses", class.to_owned()),
                        (
                            "includeRemovedDevices",
                            if include_disabled { "true" } else { "false" }.to_owned(),
                        ),
                    ],
                    None,
                )
            }
        }
    }

    pub fn find_device_by_udid(
        &mut self,
        udid: &str,
        device_class: PortalDeviceClass,
    ) -> Result<Option<PortalDevice>> {
        Ok(self
            .list_devices(device_class, true)?
            .into_iter()
            .find(|device| device.udid.eq_ignore_ascii_case(udid)))
    }

    pub fn create_device(
        &mut self,
        name: &str,
        udid: &str,
        device_class: PortalDeviceClass,
    ) -> Result<PortalDevice> {
        let mac = matches!(device_class, PortalDeviceClass::Mac);
        self.ensure_csrf(CsrfScope::Device(mac))?;
        let class = match device_class {
            PortalDeviceClass::Iphone => "iphone",
            PortalDeviceClass::Tvos => "tvOS",
            PortalDeviceClass::Watch => "watch",
            PortalDeviceClass::Mac => "mac",
        };
        let (envelope, _) = self.request_json(
            Method::POST,
            &format!("account/{}/device/addDevices.action", platform_slug(mac)),
            &[
                ("teamId", self.team_id.clone()),
                ("deviceClasses", class.to_owned()),
                ("deviceNumbers", udid.to_owned()),
                ("deviceNames", name.to_owned()),
                ("register", "single".to_owned()),
            ],
            self.csrf_headers(CsrfScope::Device(mac)),
        )?;
        let devices = json_field::<Vec<PortalDevice>>(&envelope.extra, "devices")?;
        if let Some(device) = devices.into_iter().next() {
            return Ok(device);
        }

        let messages =
            json_field::<Vec<PortalValidationMessage>>(&envelope.extra, "validationMessages")
                .unwrap_or_default()
                .into_iter()
                .filter_map(|message| message.validation_user_message)
                .collect::<Vec<_>>();
        if !messages.is_empty() {
            bail!("{}", messages.join("\n"));
        }
        bail!("Apple Developer Portal did not return the registered device")
    }

    pub fn delete_device(&mut self, id: &str, device_class: PortalDeviceClass) -> Result<()> {
        let mac = matches!(device_class, PortalDeviceClass::Mac);
        let _ = self.request_json(
            Method::POST,
            &format!("account/{}/device/deleteDevice.action", platform_slug(mac)),
            &[
                ("teamId", self.team_id.clone()),
                ("deviceId", id.to_owned()),
            ],
            self.csrf_headers(CsrfScope::Device(mac)),
        )?;
        Ok(())
    }

    pub fn list_apps(&mut self, mac: bool) -> Result<Vec<PortalAppId>> {
        self.paged_post_collection(
            &format!(
                "account/{}/identifiers/listAppIds.action",
                platform_slug(mac)
            ),
            "appIds",
            vec![
                ("teamId", self.team_id.clone()),
                ("sort", "name=asc".to_owned()),
            ],
            Some(CsrfScope::App),
        )
    }

    pub fn find_app_by_bundle_id(
        &mut self,
        bundle_id: &str,
        mac: bool,
    ) -> Result<Option<PortalAppId>> {
        let mut candidates = self.list_apps(mac)?;
        if mac {
            candidates.extend(self.list_apps(false)?);
        }
        Ok(candidates
            .into_iter()
            .find(|app| app.identifier.eq_ignore_ascii_case(bundle_id)))
    }

    pub fn create_app(&mut self, name: &str, bundle_id: &str, mac: bool) -> Result<PortalAppId> {
        self.ensure_csrf(CsrfScope::App)?;
        let app_type = if bundle_id.ends_with(".*") {
            "wildcard"
        } else {
            "explicit"
        };
        let mut params = vec![
            ("name", name.to_owned()),
            ("teamId", self.team_id.clone()),
            ("type", app_type.to_owned()),
            ("identifier", bundle_id.to_owned()),
        ];
        if app_type == "explicit" {
            params.push(("inAppPurchase", "on".to_owned()));
            params.push(("gameCenter", "on".to_owned()));
        }
        let (envelope, _) = self.request_json(
            Method::POST,
            &format!("account/{}/identifiers/addAppId.action", platform_slug(mac)),
            &params,
            self.csrf_headers(CsrfScope::App),
        )?;
        json_field::<PortalAppId>(&envelope.extra, "appId")
    }

    pub fn update_app_service(&mut self, app_id: &str, update: &PortalServiceUpdate) -> Result<()> {
        self.ensure_csrf(CsrfScope::App)?;
        let uri = if update.uses_push_uri {
            "account/ios/identifiers/updatePushService.action"
        } else {
            "account/ios/identifiers/updateService.action"
        };
        let _ = self.request_json(
            Method::POST,
            uri,
            &[
                ("teamId", self.team_id.clone()),
                ("displayId", app_id.to_owned()),
                ("featureType", update.service_id.to_owned()),
                ("featureValue", update.value.clone()),
            ],
            self.csrf_headers(CsrfScope::App),
        )?;
        Ok(())
    }

    pub fn list_app_groups(&mut self) -> Result<Vec<PortalAppGroup>> {
        self.paged_post_collection(
            "account/ios/identifiers/listApplicationGroups.action",
            "applicationGroupList",
            vec![
                ("teamId", self.team_id.clone()),
                ("sort", "name=asc".to_owned()),
            ],
            Some(CsrfScope::AppGroup),
        )
    }

    pub fn create_app_group(&mut self, name: &str, identifier: &str) -> Result<PortalAppGroup> {
        self.ensure_csrf(CsrfScope::AppGroup)?;
        let (envelope, _) = self.request_json(
            Method::POST,
            "account/ios/identifiers/addApplicationGroup.action",
            &[
                ("name", name.to_owned()),
                ("identifier", identifier.to_owned()),
                ("teamId", self.team_id.clone()),
            ],
            self.csrf_headers(CsrfScope::AppGroup),
        )?;
        json_field::<PortalAppGroup>(&envelope.extra, "applicationGroup")
    }

    pub fn list_merchants(&mut self, mac: bool) -> Result<Vec<PortalMerchant>> {
        self.paged_post_collection(
            &format!("account/{}/identifiers/listOMCs.action", platform_slug(mac)),
            "identifierList",
            vec![
                ("teamId", self.team_id.clone()),
                ("sort", "name=asc".to_owned()),
            ],
            Some(CsrfScope::Merchant(mac)),
        )
    }

    pub fn create_merchant(
        &mut self,
        name: &str,
        identifier: &str,
        mac: bool,
    ) -> Result<PortalMerchant> {
        self.ensure_csrf(CsrfScope::Merchant(mac))?;
        let (envelope, _) = self.request_json(
            Method::POST,
            &format!("account/{}/identifiers/addOMC.action", platform_slug(mac)),
            &[
                ("name", name.to_owned()),
                ("identifier", identifier.to_owned()),
                ("teamId", self.team_id.clone()),
            ],
            self.csrf_headers(CsrfScope::Merchant(mac)),
        )?;
        json_field::<PortalMerchant>(&envelope.extra, "omcId")
    }

    pub fn list_cloud_containers(&mut self) -> Result<Vec<PortalCloudContainer>> {
        self.paged_post_collection(
            "account/cloudContainer/listCloudContainers.action",
            "cloudContainerList",
            vec![
                ("teamId", self.team_id.clone()),
                ("sort", "name=asc".to_owned()),
            ],
            Some(CsrfScope::CloudContainer),
        )
    }

    pub fn create_cloud_container(
        &mut self,
        name: &str,
        identifier: &str,
    ) -> Result<PortalCloudContainer> {
        self.ensure_csrf(CsrfScope::CloudContainer)?;
        let (envelope, _) = self.request_json(
            Method::POST,
            "account/cloudContainer/addCloudContainer.action",
            &[
                ("name", name.to_owned()),
                ("identifier", identifier.to_owned()),
                ("teamId", self.team_id.clone()),
            ],
            self.csrf_headers(CsrfScope::CloudContainer),
        )?;
        json_field::<PortalCloudContainer>(&envelope.extra, "cloudContainer")
    }

    pub fn assign_app_groups(&mut self, app_id: &str, groups: &[String]) -> Result<()> {
        self.ensure_csrf(CsrfScope::AppGroup)?;
        let mut params = vec![
            ("teamId", self.team_id.clone()),
            ("appIdId", app_id.to_owned()),
            ("displayId", app_id.to_owned()),
        ];
        for group in groups {
            params.push(("applicationGroups", group.clone()));
        }
        let _ = self.request_json(
            Method::POST,
            "account/ios/identifiers/assignApplicationGroupToAppId.action",
            &params,
            self.csrf_headers(CsrfScope::AppGroup),
        )?;
        Ok(())
    }

    pub fn assign_merchants(
        &mut self,
        app_id: &str,
        merchants: &[String],
        mac: bool,
    ) -> Result<()> {
        self.ensure_csrf(CsrfScope::Merchant(mac))?;
        let mut params = vec![
            ("teamId", self.team_id.clone()),
            ("appIdId", app_id.to_owned()),
        ];
        for merchant in merchants {
            params.push(("omcIds", merchant.clone()));
        }
        let _ = self.request_json(
            Method::POST,
            &format!(
                "account/{}/identifiers/assignOMCToAppId.action",
                platform_slug(mac)
            ),
            &params,
            self.csrf_headers(CsrfScope::Merchant(mac)),
        )?;
        Ok(())
    }

    pub fn assign_cloud_containers(&mut self, app_id: &str, containers: &[String]) -> Result<()> {
        self.ensure_csrf(CsrfScope::CloudContainer)?;
        let mut params = vec![
            ("teamId", self.team_id.clone()),
            ("appIdId", app_id.to_owned()),
        ];
        for container in containers {
            params.push(("cloudContainers", container.clone()));
        }
        let _ = self.request_json(
            Method::POST,
            "account/ios/identifiers/assignCloudContainerToAppId.action",
            &params,
            self.csrf_headers(CsrfScope::CloudContainer),
        )?;
        Ok(())
    }

    pub fn list_certificates(
        &mut self,
        certificate_types: &[&str],
        mac: bool,
    ) -> Result<Vec<PortalCertificate>> {
        self.paged_post_collection(
            &format!(
                "account/{}/certificate/listCertRequests.action",
                platform_slug(mac)
            ),
            "certRequests",
            vec![
                ("teamId", self.team_id.clone()),
                ("types", certificate_types.join(",")),
                ("sort", "certRequestStatusCode=asc".to_owned()),
            ],
            Some(CsrfScope::Certificate(mac)),
        )
    }

    pub fn create_certificate(
        &mut self,
        certificate_type: &str,
        csr_content: &str,
        mac: bool,
    ) -> Result<PortalCertificate> {
        self.ensure_csrf(CsrfScope::Certificate(mac))?;
        let (envelope, _) = self.request_json(
            Method::POST,
            &format!(
                "account/{}/certificate/submitCertificateRequest.action",
                platform_slug(mac)
            ),
            &[
                ("teamId", self.team_id.clone()),
                ("type", certificate_type.to_owned()),
                ("csrContent", csr_content.to_owned()),
            ],
            self.csrf_headers(CsrfScope::Certificate(mac)),
        )?;
        json_field::<PortalCertificate>(&envelope.extra, "certRequest")
    }

    pub fn download_certificate(
        &mut self,
        certificate_id: &str,
        certificate_type: &str,
        mac: bool,
    ) -> Result<Vec<u8>> {
        self.request_bytes(
            Method::GET,
            &format!(
                "account/{}/certificate/downloadCertificateContent.action",
                platform_slug(mac)
            ),
            &[
                ("teamId", self.team_id.clone()),
                ("certificateId", certificate_id.to_owned()),
                ("type", certificate_type.to_owned()),
            ],
            self.csrf_headers(CsrfScope::Certificate(mac)),
        )
    }

    pub fn list_profiles(
        &mut self,
        platform: PortalProfilePlatform,
    ) -> Result<Vec<PortalProvisioningProfile>> {
        self.paged_post_collection(
            &format!(
                "account/{}/profile/listProvisioningProfiles.action",
                platform_slug(platform.mac())
            ),
            "provisioningProfiles",
            vec![
                ("teamId", self.team_id.clone()),
                ("sort", "name=asc".to_owned()),
                ("includeInactiveProfiles", "true".to_owned()),
                ("onlyCountLists", "true".to_owned()),
            ],
            Some(CsrfScope::Provisioning(platform.mac())),
        )
        .map(|profiles: Vec<PortalProvisioningProfile>| {
            profiles
                .into_iter()
                .filter(|profile| profile.sub_platform.as_deref() == platform.sub_platform())
                .collect()
        })
    }

    pub fn create_profile(
        &mut self,
        platform: PortalProfilePlatform,
        name: &str,
        distribution_method: &str,
        app_id: &str,
        certificate_ids: &[String],
        device_ids: &[String],
    ) -> Result<PortalProvisioningProfile> {
        self.ensure_csrf(CsrfScope::Provisioning(platform.mac()))?;
        let mut params = vec![
            ("teamId", self.team_id.clone()),
            ("provisioningProfileName", name.to_owned()),
            ("appIdId", app_id.to_owned()),
            ("distributionType", distribution_method.to_owned()),
        ];
        for certificate_id in certificate_ids {
            params.push(("certificateIds", certificate_id.clone()));
        }
        for device_id in device_ids {
            params.push(("deviceIds", device_id.clone()));
        }
        if let Some(sub_platform) = platform.sub_platform() {
            params.push(("subPlatform", sub_platform.to_owned()));
        }
        let (envelope, _) = self.request_json(
            Method::POST,
            &format!(
                "account/{}/profile/createProvisioningProfile.action",
                platform_slug(platform.mac())
            ),
            &params,
            self.csrf_headers(CsrfScope::Provisioning(platform.mac())),
        )?;
        json_field::<PortalProvisioningProfile>(&envelope.extra, "provisioningProfile")
    }

    pub fn delete_profile(
        &mut self,
        platform: PortalProfilePlatform,
        profile_id: &str,
    ) -> Result<()> {
        self.ensure_csrf(CsrfScope::Provisioning(platform.mac()))?;
        let _ = self.request_json(
            Method::POST,
            &format!(
                "account/{}/profile/deleteProvisioningProfile.action",
                platform_slug(platform.mac())
            ),
            &[
                ("teamId", self.team_id.clone()),
                ("provisioningProfileId", profile_id.to_owned()),
            ],
            self.csrf_headers(CsrfScope::Provisioning(platform.mac())),
        )?;
        Ok(())
    }

    pub fn download_profile(
        &mut self,
        platform: PortalProfilePlatform,
        profile_id: &str,
    ) -> Result<Vec<u8>> {
        self.ensure_csrf(CsrfScope::Provisioning(platform.mac()))?;
        self.request_bytes(
            Method::GET,
            &format!(
                "account/{}/profile/downloadProfileContent",
                platform_slug(platform.mac())
            ),
            &[
                ("teamId", self.team_id.clone()),
                ("provisioningProfileId", profile_id.to_owned()),
            ],
            self.csrf_headers(CsrfScope::Provisioning(platform.mac())),
        )
    }

    fn ensure_csrf(&mut self, scope: CsrfScope) -> Result<()> {
        if self.csrf_cache.contains_key(&scope) {
            return Ok(());
        }

        match scope {
            CsrfScope::App => {
                let _ = self.list_apps(false)?;
            }
            CsrfScope::Device(mac) => {
                let class = if mac {
                    PortalDeviceClass::Mac
                } else {
                    PortalDeviceClass::Iphone
                };
                let _ = self.list_devices(class, true)?;
            }
            CsrfScope::Certificate(mac) => {
                let _ = self.list_certificates(
                    if mac {
                        &[
                            "749Y1QAGU7",
                            "HXZEUKP0FP",
                            "2PQI8IDXNH",
                            "OYVN2GW35E",
                            "W0EURJRMC5",
                        ]
                    } else {
                        &["83Q87W3TGH", "WXV89964HE"]
                    },
                    mac,
                )?;
            }
            CsrfScope::Merchant(mac) => {
                let _ = self.list_merchants(mac)?;
            }
            CsrfScope::AppGroup => {
                let _ = self.list_app_groups()?;
            }
            CsrfScope::CloudContainer => {
                let _ = self.list_cloud_containers()?;
            }
            CsrfScope::Provisioning(mac) => {
                let (_, headers) = self.request_json(
                    Method::POST,
                    &format!(
                        "account/{}/profile/listProvisioningProfiles.action",
                        platform_slug(mac)
                    ),
                    &[
                        ("teamId", self.team_id.clone()),
                        ("pageNumber", "1".to_owned()),
                        ("pageSize", "1".to_owned()),
                        ("sort", "name=asc".to_owned()),
                    ],
                    None,
                )?;
                self.store_csrf_tokens(scope, &headers);
            }
        }

        Ok(())
    }

    fn csrf_headers(&self, scope: CsrfScope) -> Option<HeaderMap> {
        self.csrf_cache.get(&scope).map(|tokens| {
            let mut headers = HeaderMap::new();
            for (key, value) in &tokens.values {
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                    HeaderValue::from_str(value),
                ) {
                    headers.insert(name, value);
                }
            }
            headers
        })
    }

    fn paged_post_collection<T>(
        &mut self,
        path: &str,
        key: &str,
        base_params: Vec<(&str, String)>,
        csrf_scope: Option<CsrfScope>,
    ) -> Result<Vec<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut page_number = 1usize;
        let mut results = Vec::new();
        loop {
            let mut params = base_params.clone();
            params.push(("pageNumber", page_number.to_string()));
            params.push(("pageSize", PAGE_SIZE.to_string()));
            let (envelope, headers) = self.request_json(Method::POST, path, &params, None)?;
            if let Some(scope) = csrf_scope {
                self.store_csrf_tokens(scope, &headers);
            }
            let mut items = json_field::<Vec<T>>(&envelope.extra, key).unwrap_or_default();
            let count = items.len();
            results.append(&mut items);
            if count < PAGE_SIZE {
                break;
            }
            page_number += 1;
        }
        Ok(results)
    }

    fn request_json(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        headers: Option<HeaderMap>,
    ) -> Result<(PortalEnvelope, HeaderMap)> {
        let response = self.send(method, path, params, headers)?;
        let header_map = response.headers().clone();
        let status = response.status();
        let bytes = response
            .bytes()
            .context("failed to read Apple Developer Portal response body")?;
        let envelope: PortalEnvelope = serde_json::from_slice(&bytes).with_context(|| {
            format!("failed to parse Apple Developer Portal response as JSON (status {status})")
        })?;

        if let Some(result_code) = envelope.result_code {
            if result_code != 0 {
                let message = envelope
                    .user_string
                    .clone()
                    .or_else(|| envelope.result_string.clone())
                    .unwrap_or_else(|| "Apple Developer Portal request failed".to_owned());
                bail!("{message}");
            }
        } else if !status.is_success() {
            bail!(
                "Apple Developer Portal request failed with {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }

        Ok((envelope, header_map))
    }

    fn request_bytes(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        headers: Option<HeaderMap>,
    ) -> Result<Vec<u8>> {
        let response = self.send(method, path, params, headers)?;
        let status = response.status();
        let bytes = response
            .bytes()
            .context("failed to read Apple Developer Portal response body")?;
        if !status.is_success() {
            bail!(
                "Apple Developer Portal request failed with {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        Ok(bytes.to_vec())
    }

    fn send(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        headers: Option<HeaderMap>,
    ) -> Result<Response> {
        let url = if path.starts_with("https://") {
            path.to_owned()
        } else {
            format!("{APPLE_PORTAL_BASE_URL}/{path}")
        };
        let mut request = self.client.request(
            method.clone(),
            url_with_query(&url, method.clone(), params)?,
        );
        if let Some(headers) = headers {
            request = request.headers(headers);
        }
        request = request.header(USER_AGENT, format!("orbit/{}", env!("CARGO_PKG_VERSION")));
        if method == Method::POST {
            request = request
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form_body(params)?);
        }
        request
            .send()
            .with_context(|| format!("failed to call Apple Developer Portal `{url}`"))
    }

    fn store_csrf_tokens(&mut self, scope: CsrfScope, headers: &HeaderMap) {
        let mut tokens = HashMap::new();
        for key in ["csrf", "csrf_ts"] {
            if let Some(value) = headers.get(key).and_then(|value| value.to_str().ok()) {
                tokens.insert(key.to_owned(), value.to_owned());
            }
        }
        if !tokens.is_empty() {
            self.csrf_cache.insert(scope, CsrfTokens { values: tokens });
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct PortalValidationMessage {
    #[serde(rename = "validationUserMessage", default)]
    validation_user_message: Option<String>,
}

fn json_field<T>(value: &Map<String, Value>, key: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(value.get(key).cloned().unwrap_or(Value::Null))
        .with_context(|| format!("failed to parse Apple Developer Portal field `{key}`"))
}

fn platform_slug(mac: bool) -> &'static str {
    if mac { "mac" } else { "ios" }
}

fn url_with_query(url: &str, method: Method, params: &[(&str, String)]) -> Result<Url> {
    if method != Method::GET {
        return Url::parse(url)
            .with_context(|| format!("failed to parse Apple Developer Portal URL `{url}`"));
    }
    Url::parse_with_params(
        url,
        params.iter().map(|(key, value)| (*key, value.as_str())),
    )
    .with_context(|| format!("failed to build Apple Developer Portal URL `{url}`"))
}

fn form_body(params: &[(&str, String)]) -> Result<String> {
    let url = Url::parse_with_params(
        "https://orbit.invalid",
        params.iter().map(|(key, value)| (*key, value.as_str())),
    )
    .context("failed to encode Apple Developer Portal form parameters")?;
    Ok(url.query().unwrap_or_default().to_owned())
}
