use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use plist::{Dictionary, Value};

const OPTION_OFF: &str = "OFF";
const OPTION_ON: &str = "ON";
const OPTION_ICLOUD_XCODE_6: &str = "XCODE_6";
const OPTION_APPLE_ID_PRIMARY_CONSENT: &str = "PRIMARY_APP_CONSENT";
const OPTION_DATA_PROTECTION_COMPLETE: &str = "COMPLETE_PROTECTION";
const OPTION_DATA_PROTECTION_PROTECTED_UNLESS_OPEN: &str = "PROTECTED_UNLESS_OPEN";
const OPTION_DATA_PROTECTION_PROTECTED_UNTIL_FIRST_USER_AUTH: &str =
    "PROTECTED_UNTIL_FIRST_USER_AUTH";
const OPTION_PUSH_BROADCAST: &str = "PUSH_NOTIFICATION_FEATURE_BROADCAST";
const SETTING_DATA_PROTECTION: &str = "DATA_PROTECTION_PERMISSION_LEVEL";

const ICLOUD_SERVICE_OPTIONS: &[&str] = &[
    "CloudDocuments",
    "CloudKit",
    "CloudKit-Anonymous",
    "CloudKit-Anonymous-Dev",
];
const NETWORK_EXTENSION_OPTIONS: &[&str] = &[
    "dns-proxy",
    "app-proxy-provider",
    "content-filter-provider",
    "packet-tunnel-provider",
    "dns-proxy-systemextension",
    "app-proxy-provider-systemextension",
    "content-filter-provider-systemextension",
    "packet-tunnel-provider-systemextension",
    "dns-settings",
    "app-push-provider",
];
const NFC_OPTIONS: &[&str] = &["NDEF", "TAG"];
const VPN_OPTIONS: &[&str] = &["allow-vpn"];
const APPLE_SIGN_IN_OPTIONS: &[&str] = &["Default"];
const FONT_INSTALLATION_OPTIONS: &[&str] = &["app-usage", "system-installation"];
const APPLE_PAY_LATER_OPTIONS: &[&str] = &["payinfour-merchandising"];
const SENSITIVE_CONTENT_ANALYSIS_OPTIONS: &[&str] = &["analysis"];
const JOURNALING_SUGGESTIONS_OPTIONS: &[&str] = &["suggestions"];
const MANAGED_APP_INSTALLATION_UI_OPTIONS: &[&str] = &["managed-app"];
const NETWORK_SLICING_APP_CATEGORY_OPTIONS: &[&str] =
    &["gaming-6014", "communication-9000", "streaming-9001"];
const NETWORK_SLICING_TRAFFIC_CATEGORY_OPTIONS: &[&str] = &[
    "defaultslice-1",
    "video-2",
    "background-3",
    "voice-4",
    "callsignaling-5",
    "responsivedata-6",
    "avstreaming-7",
    "responsiveav-8",
];
const DATA_PROTECTION_ENTITLEMENT_OPTIONS: &[&str] = &[
    "NSFileProtectionCompleteUnlessOpen",
    "NSFileProtectionCompleteUntilFirstUserAuthentication",
    "NSFileProtectionNone",
    "NSFileProtectionComplete",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySyncPlan {
    pub updates: Vec<CapabilityUpdate>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityUpdate {
    pub capability_type: String,
    pub option: String,
    pub relationships: CapabilityRelationships,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityRelationships {
    pub app_groups: Option<Vec<String>>,
    pub merchant_ids: Option<Vec<String>>,
    pub cloud_containers: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCapability {
    pub id: String,
    pub capability_type: String,
    pub enabled: Option<bool>,
    pub settings: Vec<RemoteCapabilitySetting>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCapabilitySetting {
    pub key: String,
    pub options: Vec<RemoteCapabilityOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCapabilityOption {
    pub key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncStrategy {
    Boolean,
    DefinedValue,
    SettingsPresence,
    DataProtection,
    PushNotifications,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelationshipKind {
    AppGroups,
    MerchantIds,
    CloudContainers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnableOptionKind {
    On,
    IcloudXcode6,
    AppleIdPrimaryConsent,
    DataProtection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Validator {
    Boolean,
    DevProdString,
    StringArray,
    AllowedStringArray(&'static [&'static str]),
    AllowedString(&'static [&'static str]),
    PrefixedStringArray(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct CapabilityDescriptor {
    entitlement: &'static str,
    capability_type: &'static str,
    validator: Validator,
    sync_strategy: SyncStrategy,
    enable_option: EnableOptionKind,
    relationship: Option<RelationshipKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncOperation {
    Enable,
    Skip,
}

const CAPABILITY_DESCRIPTORS: &[CapabilityDescriptor] = &[
    CapabilityDescriptor {
        entitlement: "com.apple.developer.homekit",
        capability_type: "HOMEKIT",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.networking.HotspotConfiguration",
        capability_type: "HOT_SPOT",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.networking.multipath",
        capability_type: "MULTIPATH",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.siri",
        capability_type: "SIRIKIT",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.external-accessory.wireless-configuration",
        capability_type: "WIRELESS_ACCESSORY_CONFIGURATION",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.kernel.extended-virtual-addressing",
        capability_type: "EXTENDED_VIRTUAL_ADDRESSING",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.networking.wifi-info",
        capability_type: "ACCESS_WIFI_INFORMATION",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.associated-domains",
        capability_type: "ASSOCIATED_DOMAINS",
        validator: Validator::StringArray,
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.authentication-services.autofill-credential-provider",
        capability_type: "AUTOFILL_CREDENTIAL_PROVIDER",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.healthkit",
        capability_type: "HEALTHKIT",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.security.application-groups",
        capability_type: "APP_GROUPS",
        validator: Validator::PrefixedStringArray("group."),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: Some(RelationshipKind::AppGroups),
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.in-app-payments",
        capability_type: "APPLE_PAY",
        validator: Validator::PrefixedStringArray("merchant."),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: Some(RelationshipKind::MerchantIds),
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.icloud-container-identifiers",
        capability_type: "ICLOUD",
        validator: Validator::PrefixedStringArray("iCloud."),
        sync_strategy: SyncStrategy::SettingsPresence,
        enable_option: EnableOptionKind::IcloudXcode6,
        relationship: Some(RelationshipKind::CloudContainers),
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.ClassKit-environment",
        capability_type: "CLASSKIT",
        validator: Validator::DevProdString,
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.usernotifications.communication",
        capability_type: "USERNOTIFICATIONS_COMMUNICATION",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.usernotifications.time-sensitive",
        capability_type: "USERNOTIFICATIONS_TIMESENSITIVE",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.group-session",
        capability_type: "GROUP_ACTIVITIES",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.family-controls",
        capability_type: "FAMILY_CONTROLS",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.default-data-protection",
        capability_type: "DATA_PROTECTION",
        validator: Validator::AllowedString(DATA_PROTECTION_ENTITLEMENT_OPTIONS),
        sync_strategy: SyncStrategy::DataProtection,
        enable_option: EnableOptionKind::DataProtection,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "inter-app-audio",
        capability_type: "INTER_APP_AUDIO",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.networking.networkextension",
        capability_type: "NETWORK_EXTENSIONS",
        validator: Validator::AllowedStringArray(NETWORK_EXTENSION_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.nfc.readersession.formats",
        capability_type: "NFC_TAG_READING",
        validator: Validator::AllowedStringArray(NFC_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.networking.vpn.api",
        capability_type: "PERSONAL_VPN",
        validator: Validator::AllowedStringArray(VPN_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "aps-environment",
        capability_type: "PUSH_NOTIFICATIONS",
        validator: Validator::DevProdString,
        sync_strategy: SyncStrategy::PushNotifications,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.pass-type-identifiers",
        capability_type: "WALLET",
        validator: Validator::StringArray,
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.applesignin",
        capability_type: "APPLE_ID_AUTH",
        validator: Validator::AllowedStringArray(APPLE_SIGN_IN_OPTIONS),
        sync_strategy: SyncStrategy::SettingsPresence,
        enable_option: EnableOptionKind::AppleIdPrimaryConsent,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.user-fonts",
        capability_type: "FONT_INSTALLATION",
        validator: Validator::AllowedStringArray(FONT_INSTALLATION_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.pay-later-merchandising",
        capability_type: "APPLE_PAY_LATER_MERCHANDISING",
        validator: Validator::AllowedStringArray(APPLE_PAY_LATER_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.sensitivecontentanalysis.client",
        capability_type: "SENSITIVE_CONTENT_ANALYSIS",
        validator: Validator::AllowedStringArray(SENSITIVE_CONTENT_ANALYSIS_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.devicecheck.appattest-environment",
        capability_type: "APP_ATTEST",
        validator: Validator::DevProdString,
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.coremedia.hls.low-latency",
        capability_type: "COREMEDIA_HLS_LOW_LATENCY",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.associated-domains.mdm-managed",
        capability_type: "MDM_MANAGED_ASSOCIATED_DOMAINS",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.fileprovider.testing-mode",
        capability_type: "FILEPROVIDER_TESTINGMODE",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.healthkit.recalibrate-estimates",
        capability_type: "HEALTHKIT_RECALIBRATE_ESTIMATES",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.maps",
        capability_type: "MAPS",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.user-management",
        capability_type: "USER_MANAGEMENT",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.networking.custom-protocol",
        capability_type: "NETWORK_CUSTOM_PROTOCOL",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.system-extension.install",
        capability_type: "SYSTEM_EXTENSION_INSTALL",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.push-to-talk",
        capability_type: "PUSH_TO_TALK",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.transport.usb",
        capability_type: "DRIVERKIT_USBTRANSPORT_PUB",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.kernel.increased-memory-limit",
        capability_type: "INCREASED_MEMORY_LIMIT",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.communicates-with-drivers",
        capability_type: "DRIVERKIT_COMMUNICATESWITHDRIVERS",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.media-device-discovery-extension",
        capability_type: "MEDIA_DEVICE_DISCOVERY",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.allow-third-party-userclients",
        capability_type: "DRIVERKIT_ALLOWTHIRDPARTY_USERCLIENTS",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.weatherkit",
        capability_type: "WEATHERKIT",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.on-demand-install-capable",
        capability_type: "ONDEMANDINSTALL_EXTENSIONS",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.family.scsicontroller",
        capability_type: "DRIVERKIT_FAMILY_SCSICONTROLLER_PUB",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.family.serial",
        capability_type: "DRIVERKIT_FAMILY_SERIAL_PUB",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.family.networking",
        capability_type: "DRIVERKIT_FAMILY_NETWORKING_PUB",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.family.hid.eventservice",
        capability_type: "DRIVERKIT_FAMILY_HIDEVENTSERVICE_PUB",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.family.hid.device",
        capability_type: "DRIVERKIT_FAMILY_HIDDEVICE_PUB",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit",
        capability_type: "DRIVERKIT_PUBLIC",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.transport.hid",
        capability_type: "DRIVERKIT_TRANSPORT_HID_PUB",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.driverkit.family.audio",
        capability_type: "DRIVERKIT_FAMILY_AUDIO_PUB",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.shared-with-you",
        capability_type: "SHARED_WITH_YOU",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.shared-with-you.collaboration",
        capability_type: "MESSAGES_COLLABORATION",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.submerged-shallow-depth-and-pressure",
        capability_type: "SHALLOW_DEPTH_PRESSURE",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.proximity-reader.identity.display",
        capability_type: "TAP_TO_DISPLAY_ID",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.proximity-reader.payment.acceptance",
        capability_type: "TAP_TO_PAY_ON_IPHONE",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.matter.allow-setup-payload",
        capability_type: "MATTER_ALLOW_SETUP_PAYLOAD",
        validator: Validator::Boolean,
        sync_strategy: SyncStrategy::Boolean,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.journal.allow",
        capability_type: "JOURNALING_SUGGESTIONS",
        validator: Validator::AllowedStringArray(JOURNALING_SUGGESTIONS_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.managed-app-distribution.install-ui",
        capability_type: "MANAGED_APP_INSTALLATION_UI",
        validator: Validator::AllowedStringArray(MANAGED_APP_INSTALLATION_UI_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.networking.slicing.appcategory",
        capability_type: "NETWORK_SLICING",
        validator: Validator::AllowedStringArray(NETWORK_SLICING_APP_CATEGORY_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
    CapabilityDescriptor {
        entitlement: "com.apple.developer.networking.slicing.trafficcategory",
        capability_type: "NETWORK_SLICING",
        validator: Validator::AllowedStringArray(NETWORK_SLICING_TRAFFIC_CATEGORY_OPTIONS),
        sync_strategy: SyncStrategy::DefinedValue,
        enable_option: EnableOptionKind::On,
        relationship: None,
    },
];

pub fn capability_sync_plan_from_entitlements(
    path: &Path,
    remote: &[RemoteCapability],
) -> Result<CapabilitySyncPlan> {
    let value = Value::from_file(path)
        .with_context(|| format!("failed to parse entitlements {}", path.display()))?;
    let dictionary = value
        .into_dictionary()
        .context("entitlements file must contain a top-level dictionary")?;
    capability_sync_plan_from_dictionary(&dictionary, remote)
}

fn capability_sync_plan_from_dictionary(
    dictionary: &Dictionary,
    remote: &[RemoteCapability],
) -> Result<CapabilitySyncPlan> {
    let mut updates = BTreeMap::<String, CapabilityUpdate>::new();
    let mut remaining_remote = remote.to_vec();
    let ignored = ignored_entitlements();

    for (key, value) in dictionary {
        if ignored.contains(key.as_str()) {
            continue;
        }

        if validate_supplemental_entitlement(key, value)? {
            continue;
        }

        let Some(descriptor) = descriptor_for_entitlement(key) else {
            if key == "com.apple.developer.parent-application-identifiers" {
                bail!(
                    "entitlement `{key}` is recognized, but Orbit does not support App Clip bundle identifier creation yet"
                );
            }
            if key.starts_with("com.apple.") || key == "aps-environment" || key == "inter-app-audio"
            {
                bail!(
                    "entitlement `{key}` is not supported by Orbit yet; remove it or add capability support before signing"
                );
            }
            continue;
        };

        descriptor.validator.validate(key, value)?;
        let relationships = relationships_for_entitlement(descriptor, key, value)?;
        if !relationships.is_unset() {
            let update = upsert_update(
                &mut updates,
                descriptor.capability_type,
                descriptor.enable_option.resolve(key, value)?,
            );
            update.relationships.merge(relationships);
        }

        let existing_index = remaining_remote.iter().position(|candidate| {
            effective_capability_type(candidate)
                .is_some_and(|capability_type| capability_type == descriptor.capability_type)
        });
        let existing = existing_index.map(|index| &remaining_remote[index]);
        match compute_sync_operation(descriptor, key, value, existing)? {
            SyncOperation::Enable => {
                upsert_update(
                    &mut updates,
                    descriptor.capability_type,
                    descriptor.enable_option.resolve(key, value)?,
                );
            }
            SyncOperation::Skip => {
                if !updates.contains_key(descriptor.capability_type) {
                    if let Some(index) = existing_index {
                        remaining_remote.remove(index);
                    }
                }
            }
        }
    }

    for existing in remaining_remote {
        let Some(capability_type) = effective_capability_type(&existing) else {
            continue;
        };

        if capability_type == "IN_APP_PURCHASE" || capability_type == "GAME_CENTER" {
            continue;
        }
        if capability_type == "MDM_MANAGED_ASSOCIATED_DOMAINS"
            && dictionary.contains_key("com.apple.developer.associated-domains")
        {
            continue;
        }

        let Some(descriptor) = descriptor_for_capability_type(capability_type) else {
            continue;
        };
        if updates.contains_key(descriptor.capability_type) {
            continue;
        }

        updates.insert(
            descriptor.capability_type.to_owned(),
            CapabilityUpdate {
                capability_type: descriptor.capability_type.to_owned(),
                option: OPTION_OFF.to_owned(),
                relationships: CapabilityRelationships::default(),
            },
        );
    }

    let mut updates = updates.into_values().collect::<Vec<_>>();
    updates.sort_by(|left, right| left.capability_type.cmp(&right.capability_type));
    Ok(CapabilitySyncPlan { updates })
}

fn compute_sync_operation(
    descriptor: &CapabilityDescriptor,
    key: &str,
    value: &Value,
    existing: Option<&RemoteCapability>,
) -> Result<SyncOperation> {
    let desired_option = descriptor.enable_option.resolve(key, value)?;
    match descriptor.sync_strategy {
        SyncStrategy::Boolean => {
            let desired_enabled = value
                .as_boolean()
                .with_context(|| format!("`{key}` must be a boolean"))?;
            let Some(existing) = existing else {
                return Ok(if desired_enabled {
                    SyncOperation::Enable
                } else {
                    SyncOperation::Skip
                });
            };

            if existing.is_enabled() == desired_enabled && existing.settings.is_empty() {
                Ok(SyncOperation::Skip)
            } else {
                Ok(SyncOperation::Enable)
            }
        }
        SyncStrategy::DefinedValue => {
            let Some(existing) = existing else {
                return Ok(SyncOperation::Enable);
            };
            if existing.is_enabled() && existing.settings.is_empty() {
                Ok(SyncOperation::Skip)
            } else {
                Ok(SyncOperation::Enable)
            }
        }
        SyncStrategy::SettingsPresence => {
            let Some(existing) = existing else {
                return Ok(SyncOperation::Enable);
            };
            if existing.is_enabled() && !existing.settings.is_empty() {
                Ok(SyncOperation::Skip)
            } else {
                Ok(SyncOperation::Enable)
            }
        }
        SyncStrategy::DataProtection => {
            let Some(existing) = existing else {
                return Ok(SyncOperation::Enable);
            };
            if existing.is_enabled()
                && existing
                    .enabled_setting_option(SETTING_DATA_PROTECTION)
                    .is_some_and(|option| option == desired_option)
            {
                Ok(SyncOperation::Skip)
            } else {
                Ok(SyncOperation::Enable)
            }
        }
        SyncStrategy::PushNotifications => {
            let wants_broadcast = desired_option == OPTION_PUSH_BROADCAST;
            let Some(existing) = existing else {
                return Ok(SyncOperation::Enable);
            };
            if existing.is_enabled() && (!existing.settings.is_empty() == wants_broadcast) {
                Ok(SyncOperation::Skip)
            } else {
                Ok(SyncOperation::Enable)
            }
        }
    }
}

fn upsert_update<'a>(
    updates: &'a mut BTreeMap<String, CapabilityUpdate>,
    capability_type: &str,
    option: String,
) -> &'a mut CapabilityUpdate {
    let update = updates
        .entry(capability_type.to_owned())
        .or_insert_with(|| CapabilityUpdate {
            capability_type: capability_type.to_owned(),
            option: option.clone(),
            relationships: CapabilityRelationships::default(),
        });
    update.option = option;
    update
}

fn relationships_for_entitlement(
    descriptor: &CapabilityDescriptor,
    key: &str,
    value: &Value,
) -> Result<CapabilityRelationships> {
    let Some(relationship) = descriptor.relationship else {
        return Ok(CapabilityRelationships::default());
    };
    let values = string_array(key, value)?;
    let values = dedup_sorted(values);
    Ok(match relationship {
        RelationshipKind::AppGroups => CapabilityRelationships {
            app_groups: Some(values),
            ..Default::default()
        },
        RelationshipKind::MerchantIds => CapabilityRelationships {
            merchant_ids: Some(values),
            ..Default::default()
        },
        RelationshipKind::CloudContainers => CapabilityRelationships {
            cloud_containers: Some(values),
            ..Default::default()
        },
    })
}

fn validate_supplemental_entitlement(key: &str, value: &Value) -> Result<bool> {
    match key {
        "com.apple.developer.icloud-services" => {
            Validator::AllowedStringArray(ICLOUD_SERVICE_OPTIONS).validate(key, value)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn descriptor_for_entitlement(key: &str) -> Option<&'static CapabilityDescriptor> {
    CAPABILITY_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.entitlement == key)
}

fn descriptor_for_capability_type(capability_type: &str) -> Option<&'static CapabilityDescriptor> {
    CAPABILITY_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.capability_type == capability_type)
}

impl CapabilityRelationships {
    fn is_unset(&self) -> bool {
        self.app_groups.is_none() && self.merchant_ids.is_none() && self.cloud_containers.is_none()
    }

    fn merge(&mut self, other: CapabilityRelationships) {
        merge_optional_values(&mut self.app_groups, other.app_groups);
        merge_optional_values(&mut self.merchant_ids, other.merchant_ids);
        merge_optional_values(&mut self.cloud_containers, other.cloud_containers);
    }
}

impl RemoteCapability {
    fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    fn enabled_setting_option(&self, key: &str) -> Option<&str> {
        self.settings
            .iter()
            .find(|setting| setting.key == key)
            .and_then(|setting| {
                setting
                    .options
                    .iter()
                    .find(|option| option.enabled)
                    .map(|option| option.key.as_str())
            })
    }
}

impl EnableOptionKind {
    fn resolve(self, key: &str, value: &Value) -> Result<String> {
        match self {
            EnableOptionKind::On => Ok(OPTION_ON.to_owned()),
            EnableOptionKind::IcloudXcode6 => Ok(OPTION_ICLOUD_XCODE_6.to_owned()),
            EnableOptionKind::AppleIdPrimaryConsent => {
                Ok(OPTION_APPLE_ID_PRIMARY_CONSENT.to_owned())
            }
            EnableOptionKind::DataProtection => {
                let Some(text) = value.as_string() else {
                    bail!("`{key}` must be a string");
                };
                match text {
                    "NSFileProtectionComplete" => Ok(OPTION_DATA_PROTECTION_COMPLETE.to_owned()),
                    "NSFileProtectionCompleteUnlessOpen" => {
                        Ok(OPTION_DATA_PROTECTION_PROTECTED_UNLESS_OPEN.to_owned())
                    }
                    "NSFileProtectionCompleteUntilFirstUserAuthentication" => {
                        Ok(OPTION_DATA_PROTECTION_PROTECTED_UNTIL_FIRST_USER_AUTH.to_owned())
                    }
                    "NSFileProtectionNone" => bail!(
                        "entitlement `{key}` uses `NSFileProtectionNone`, which Orbit cannot sync through Apple capability settings"
                    ),
                    _ => bail!(
                        "`{key}` contained unsupported value `{text}`; allowed values are {}",
                        DATA_PROTECTION_ENTITLEMENT_OPTIONS.join(", ")
                    ),
                }
            }
        }
    }
}

impl Validator {
    fn validate(self, key: &str, value: &Value) -> Result<()> {
        match self {
            Validator::Boolean => validate_boolean(key, value),
            Validator::DevProdString => validate_dev_prod(key, value),
            Validator::StringArray => {
                let _ = string_array(key, value)?;
                Ok(())
            }
            Validator::AllowedStringArray(allowed) => {
                let _ = allowed_string_array(key, value, allowed)?;
                Ok(())
            }
            Validator::AllowedString(allowed) => {
                let _ = allowed_string(key, value, allowed)?;
                Ok(())
            }
            Validator::PrefixedStringArray(prefix) => {
                let _ = prefixed_array(key, value, prefix)?;
                Ok(())
            }
        }
    }
}

fn effective_capability_type(capability: &RemoteCapability) -> Option<&str> {
    if !capability.capability_type.is_empty() {
        return Some(capability.capability_type.as_str());
    }
    capability.id.split_once('_').map(|(_, suffix)| suffix)
}

fn validate_boolean(key: &str, value: &Value) -> Result<()> {
    if value.as_boolean().is_some() {
        Ok(())
    } else {
        bail!("`{key}` must be a boolean")
    }
}

fn validate_dev_prod(key: &str, value: &Value) -> Result<()> {
    let Some(text) = value.as_string() else {
        bail!("`{key}` must be a string");
    };
    match text {
        "development" | "production" => Ok(()),
        other => bail!("`{key}` must be `development` or `production`, got `{other}`"),
    }
}

fn string_array(key: &str, value: &Value) -> Result<Vec<String>> {
    let Some(values) = value.as_array() else {
        bail!("`{key}` must be an array");
    };
    values
        .iter()
        .map(|item| {
            item.as_string()
                .map(ToOwned::to_owned)
                .context(format!("`{key}` must contain only strings"))
        })
        .collect()
}

fn prefixed_array(key: &str, value: &Value, prefix: &str) -> Result<Vec<String>> {
    let values = string_array(key, value)?;
    if values.iter().all(|item| item.starts_with(prefix)) {
        Ok(values)
    } else {
        bail!("`{key}` must contain only values prefixed with `{prefix}`")
    }
}

fn allowed_string_array(key: &str, value: &Value, allowed: &[&str]) -> Result<Vec<String>> {
    let values = string_array(key, value)?;
    if values
        .iter()
        .all(|item| allowed.iter().any(|candidate| candidate == item))
    {
        Ok(values)
    } else {
        bail!(
            "`{key}` contained unsupported values; allowed values are {}",
            allowed.join(", ")
        )
    }
}

fn allowed_string(key: &str, value: &Value, allowed: &[&str]) -> Result<String> {
    let Some(text) = value.as_string() else {
        bail!("`{key}` must be a string");
    };
    if allowed.iter().any(|candidate| candidate == &text) {
        Ok(text.to_owned())
    } else {
        bail!(
            "`{key}` contained unsupported value `{text}`; allowed values are {}",
            allowed.join(", ")
        )
    }
}

fn ignored_entitlements() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "application-identifier",
        "com.apple.developer.team-identifier",
        "com.apple.developer.ubiquity-kvstore-identifier",
        "get-task-allow",
        "keychain-access-groups",
        "beta-reports-active",
        "com.apple.security.get-task-allow",
        "com.apple.security.app-sandbox",
        "com.apple.security.network.client",
        "com.apple.security.network.server",
        "com.apple.security.files.user-selected.read-only",
        "com.apple.security.files.user-selected.read-write",
    ])
}

fn merge_optional_values(target: &mut Option<Vec<String>>, other: Option<Vec<String>>) {
    let Some(other) = other else {
        return;
    };
    let merged = match target.take() {
        Some(existing) => {
            let mut seen = existing.into_iter().collect::<BTreeSet<_>>();
            seen.extend(other);
            seen.into_iter().collect()
        }
        None => dedup_sorted(other),
    };
    *target = Some(merged);
}

fn dedup_sorted(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use plist::Value;

    use super::{
        CapabilitySyncPlan, OPTION_APPLE_ID_PRIMARY_CONSENT, OPTION_DATA_PROTECTION_COMPLETE,
        OPTION_DATA_PROTECTION_PROTECTED_UNLESS_OPEN, OPTION_OFF, OPTION_ON, OPTION_PUSH_BROADCAST,
        RemoteCapability, RemoteCapabilityOption, RemoteCapabilitySetting,
        capability_sync_plan_from_dictionary,
    };

    fn remote_capability(
        capability_type: &str,
        enabled: Option<bool>,
        settings: Vec<RemoteCapabilitySetting>,
    ) -> RemoteCapability {
        RemoteCapability {
            id: format!("BUNDLE_{capability_type}"),
            capability_type: capability_type.to_owned(),
            enabled,
            settings,
        }
    }

    fn enabled_setting(key: &str, option: &str) -> RemoteCapabilitySetting {
        RemoteCapabilitySetting {
            key: key.to_owned(),
            options: vec![RemoteCapabilityOption {
                key: option.to_owned(),
                enabled: true,
            }],
        }
    }

    fn plan(dictionary: plist::Dictionary, remote: Vec<RemoteCapability>) -> CapabilitySyncPlan {
        capability_sync_plan_from_dictionary(&dictionary, &remote).unwrap()
    }

    #[test]
    fn builds_identifier_and_settings_updates() {
        let plan = plan(
            plist::Dictionary::from_iter([
                (
                    "com.apple.developer.applesignin".to_owned(),
                    Value::Array(vec![Value::String("Default".to_owned())]),
                ),
                (
                    "com.apple.security.application-groups".to_owned(),
                    Value::Array(vec![Value::String("group.dev.orbit.demo".to_owned())]),
                ),
                (
                    "com.apple.developer.in-app-payments".to_owned(),
                    Value::Array(vec![Value::String("merchant.dev.orbit.demo".to_owned())]),
                ),
                (
                    "com.apple.developer.icloud-container-identifiers".to_owned(),
                    Value::Array(vec![Value::String("iCloud.dev.orbit.demo".to_owned())]),
                ),
            ]),
            vec![
                remote_capability("APPLE_ID_AUTH", Some(true), vec![]),
                remote_capability("APP_GROUPS", Some(true), vec![]),
                remote_capability("APPLE_PAY", Some(true), vec![]),
                remote_capability("ICLOUD", Some(true), vec![]),
            ],
        );

        assert_eq!(plan.updates.len(), 4);
        assert_eq!(plan.updates[0].capability_type, "APPLE_ID_AUTH");
        assert_eq!(plan.updates[0].option, OPTION_APPLE_ID_PRIMARY_CONSENT);
        assert_eq!(plan.updates[1].capability_type, "APPLE_PAY");
        assert_eq!(
            plan.updates[1].relationships.merchant_ids,
            Some(vec!["merchant.dev.orbit.demo".to_owned()])
        );
        assert_eq!(plan.updates[2].capability_type, "APP_GROUPS");
        assert_eq!(
            plan.updates[2].relationships.app_groups,
            Some(vec!["group.dev.orbit.demo".to_owned()])
        );
        assert_eq!(plan.updates[3].capability_type, "ICLOUD");
        assert_eq!(
            plan.updates[3].relationships.cloud_containers,
            Some(vec!["iCloud.dev.orbit.demo".to_owned()])
        );
    }

    #[test]
    fn disables_missing_supported_remote_capabilities() {
        let plan = plan(
            plist::Dictionary::from_iter([(
                "com.apple.developer.healthkit".to_owned(),
                Value::Boolean(true),
            )]),
            vec![
                remote_capability("HEALTHKIT", Some(true), vec![]),
                remote_capability("SIRIKIT", Some(true), vec![]),
            ],
        );

        assert_eq!(plan.updates.len(), 1);
        assert_eq!(plan.updates[0].capability_type, "SIRIKIT");
        assert_eq!(plan.updates[0].option, OPTION_OFF);
    }

    #[test]
    fn syncs_data_protection_when_setting_changes() {
        let plan = plan(
            plist::Dictionary::from_iter([(
                "com.apple.developer.default-data-protection".to_owned(),
                Value::String("NSFileProtectionComplete".to_owned()),
            )]),
            vec![remote_capability(
                "DATA_PROTECTION",
                Some(true),
                vec![enabled_setting(
                    "DATA_PROTECTION_PERMISSION_LEVEL",
                    OPTION_DATA_PROTECTION_PROTECTED_UNLESS_OPEN,
                )],
            )],
        );

        assert_eq!(plan.updates.len(), 1);
        assert_eq!(plan.updates[0].capability_type, "DATA_PROTECTION");
        assert_eq!(plan.updates[0].option, OPTION_DATA_PROTECTION_COMPLETE);
    }

    #[test]
    fn skips_push_update_when_remote_state_matches() {
        let plan = plan(
            plist::Dictionary::from_iter([(
                "aps-environment".to_owned(),
                Value::String("development".to_owned()),
            )]),
            vec![remote_capability("PUSH_NOTIFICATIONS", Some(true), vec![])],
        );

        assert!(plan.updates.is_empty());
    }

    #[test]
    fn updates_push_when_remote_has_broadcast_setting() {
        let plan = plan(
            plist::Dictionary::from_iter([(
                "aps-environment".to_owned(),
                Value::String("development".to_owned()),
            )]),
            vec![remote_capability(
                "PUSH_NOTIFICATIONS",
                Some(true),
                vec![enabled_setting(
                    "PUSH_NOTIFICATION_FEATURES",
                    OPTION_PUSH_BROADCAST,
                )],
            )],
        );

        assert_eq!(plan.updates.len(), 1);
        assert_eq!(plan.updates[0].capability_type, "PUSH_NOTIFICATIONS");
        assert_eq!(plan.updates[0].option, OPTION_ON);
    }

    #[test]
    fn rejects_data_protection_none() {
        let error = capability_sync_plan_from_dictionary(
            &plist::Dictionary::from_iter([(
                "com.apple.developer.default-data-protection".to_owned(),
                Value::String("NSFileProtectionNone".to_owned()),
            )]),
            &[],
        )
        .unwrap_err();

        assert!(error.to_string().contains("NSFileProtectionNone"));
    }
}
