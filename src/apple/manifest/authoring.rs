use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::ApplePlatform;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppManifest {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub bundle_id: String,
    pub version: String,
    pub build: u64,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub provider_id: Option<String>,
    pub platforms: BTreeMap<ApplePlatform, String>,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<PathBuf>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub info: InfoManifest,
    #[serde(default)]
    pub entitlements: EntitlementsManifest,
    #[serde(rename = "pushBroadcastForLiveActivities", default)]
    pub push_broadcast_for_live_activities: bool,
    #[serde(default)]
    pub extensions: BTreeMap<String, ExtensionConfig>,
    #[serde(default)]
    pub watch: Option<WatchConfig>,
    #[serde(default)]
    pub app_clip: Option<AppClipConfig>,
    #[serde(default)]
    pub hooks: Option<HooksManifest>,
    #[serde(default)]
    pub tests: Option<TestsManifest>,
    #[serde(default)]
    pub quality: QualityManifest,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QualityManifest {
    #[serde(default)]
    pub lint: LintQualityManifest,
    #[serde(default)]
    pub format: FormatQualityManifest,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LintQualityManifest {
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub rules: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FormatQualityManifest {
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub rules: BTreeMap<String, JsonValue>,
    #[serde(default)]
    pub editorconfig: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfoManifest {
    #[serde(default)]
    pub extra: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EntitlementsManifest {
    #[serde(default)]
    pub app_groups: Vec<String>,
    #[serde(default)]
    pub associated_domains: Vec<String>,
    #[serde(default)]
    pub merchant_ids: Vec<String>,
    #[serde(default)]
    pub cloud_containers: Vec<String>,
    #[serde(default)]
    pub icloud_services: Vec<String>,
    #[serde(default)]
    pub classkit_environment: Option<String>,
    #[serde(default)]
    pub default_data_protection: Option<String>,
    #[serde(default)]
    pub network_extensions: Vec<String>,
    #[serde(default)]
    pub nfc_reader_session_formats: Vec<String>,
    #[serde(default)]
    pub vpn_api: Vec<String>,
    #[serde(default)]
    pub pass_type_identifiers: Vec<String>,
    #[serde(default)]
    pub apple_sign_in: Vec<String>,
    #[serde(default)]
    pub user_fonts: Vec<String>,
    #[serde(default)]
    pub apple_pay_later_merchandising: Vec<String>,
    #[serde(default)]
    pub sensitive_content_analysis: Vec<String>,
    #[serde(default)]
    pub app_attest_environment: Option<String>,
    #[serde(default)]
    pub journal_allow: Vec<String>,
    #[serde(default)]
    pub managed_app_distribution_install_ui: Vec<String>,
    #[serde(default)]
    pub network_slicing_app_category: Vec<String>,
    #[serde(default)]
    pub network_slicing_traffic_category: Vec<String>,
    #[serde(default)]
    pub homekit: bool,
    #[serde(default)]
    pub hotspot_configuration: bool,
    #[serde(default)]
    pub multipath: bool,
    #[serde(default)]
    pub siri: bool,
    #[serde(default)]
    pub wireless_accessory_configuration: bool,
    #[serde(default)]
    pub extended_virtual_addressing: bool,
    #[serde(default)]
    pub wifi_info: bool,
    #[serde(default)]
    pub autofill_credential_provider: bool,
    #[serde(default)]
    pub healthkit: bool,
    #[serde(default)]
    pub communication_notifications: bool,
    #[serde(default)]
    pub time_sensitive_notifications: bool,
    #[serde(default)]
    pub push_notifications: bool,
    #[serde(default)]
    pub group_activities: bool,
    #[serde(default)]
    pub family_controls: bool,
    #[serde(default)]
    pub inter_app_audio: bool,
    #[serde(default)]
    pub hls_low_latency: bool,
    #[serde(default)]
    pub mdm_managed_associated_domains: bool,
    #[serde(default)]
    pub fileprovider_testing_mode: bool,
    #[serde(default)]
    pub healthkit_recalibrate_estimates: bool,
    #[serde(default)]
    pub maps: bool,
    #[serde(default)]
    pub user_management: bool,
    #[serde(default)]
    pub custom_protocol: bool,
    #[serde(default)]
    pub system_extension_install: bool,
    #[serde(default)]
    pub push_to_talk: bool,
    #[serde(default)]
    pub driverkit_transport_usb: bool,
    #[serde(default)]
    pub increased_memory_limit: bool,
    #[serde(default)]
    pub driverkit_communicates_with_drivers: bool,
    #[serde(default)]
    pub media_device_discovery_extension: bool,
    #[serde(default)]
    pub driverkit_allow_third_party_userclients: bool,
    #[serde(default)]
    pub weatherkit: bool,
    #[serde(default)]
    pub on_demand_install_capable: bool,
    #[serde(default)]
    pub driverkit_family_scsi_controller: bool,
    #[serde(default)]
    pub driverkit_family_serial: bool,
    #[serde(default)]
    pub driverkit_family_networking: bool,
    #[serde(default)]
    pub driverkit_family_hid_eventservice: bool,
    #[serde(default)]
    pub driverkit_family_hid_device: bool,
    #[serde(default)]
    pub driverkit: bool,
    #[serde(default)]
    pub driverkit_transport_hid: bool,
    #[serde(default)]
    pub driverkit_family_audio: bool,
    #[serde(default)]
    pub shared_with_you: bool,
    #[serde(default)]
    pub shared_with_you_collaboration: bool,
    #[serde(default)]
    pub submerged_shallow_depth_and_pressure: bool,
    #[serde(default)]
    pub proximity_reader_identity_display: bool,
    #[serde(default)]
    pub proximity_reader_payment_acceptance: bool,
    #[serde(default)]
    pub matter_allow_setup_payload: bool,
    #[serde(default)]
    pub sandbox: Option<SandboxConfig>,
    #[serde(default)]
    pub extra: BTreeMap<String, JsonValue>,
}

impl EntitlementsManifest {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub network: Vec<SandboxNetworkPermission>,
    #[serde(default)]
    pub files: Vec<SandboxFilePermission>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxNetworkPermission {
    Client,
    Server,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxFilePermission {
    UserSelectedReadOnly,
    UserSelectedReadWrite,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionConfig {
    pub kind: ExtensionKind,
    #[serde(default)]
    pub platforms: Vec<ApplePlatform>,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<PathBuf>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub info: InfoManifest,
    #[serde(default)]
    pub entitlements: EntitlementsManifest,
    #[serde(default)]
    pub entry: Option<EntryConfig>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionKind {
    PacketTunnel,
    Widget,
    Share,
    Safari,
    SafariWeb,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryConfig {
    pub class: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchConfig {
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<PathBuf>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub info: InfoManifest,
    #[serde(default)]
    pub entitlements: EntitlementsManifest,
    pub extension: WatchExtensionConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchExtensionConfig {
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<PathBuf>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub info: InfoManifest,
    #[serde(default)]
    pub entitlements: EntitlementsManifest,
    pub entry: EntryConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppClipConfig {
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<PathBuf>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub info: InfoManifest,
    #[serde(default)]
    pub entitlements: EntitlementsManifest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencySpec {
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub framework: Option<bool>,
    #[serde(default)]
    pub xcframework: Option<PathBuf>,
    #[serde(default)]
    pub embed: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HooksManifest {
    #[serde(default)]
    pub before_build: Vec<String>,
    #[serde(default)]
    pub before_run: Vec<String>,
    #[serde(default)]
    pub after_sign: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestsManifest {
    #[serde(default)]
    pub unit: Option<TestTargetManifest>,
    #[serde(default)]
    pub ui: Option<TestTargetManifest>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TestFormat {
    SwiftTesting,
    Maestro,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestTargetManifest {
    #[serde(default)]
    pub format: Option<TestFormat>,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
}
