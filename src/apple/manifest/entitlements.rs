use anyhow::{Result, bail};
use plist::{Dictionary, Value as PlistValue};
use serde_json::Value as JsonValue;

use super::authoring::{EntitlementsManifest, SandboxFilePermission, SandboxNetworkPermission};

const DEV_PROD_OPTIONS: &[&str] = &["development", "production"];
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
const NFC_READER_SESSION_FORMAT_OPTIONS: &[&str] = &["NDEF", "TAG"];
const VPN_API_OPTIONS: &[&str] = &["allow-vpn"];
const APPLE_SIGN_IN_OPTIONS: &[&str] = &["Default"];
const USER_FONT_OPTIONS: &[&str] = &["app-usage", "system-installation"];
const APPLE_PAY_LATER_OPTIONS: &[&str] = &["payinfour-merchandising"];
const SENSITIVE_CONTENT_ANALYSIS_OPTIONS: &[&str] = &["analysis"];
const JOURNAL_ALLOW_OPTIONS: &[&str] = &["suggestions"];
const MANAGED_APP_DISTRIBUTION_INSTALL_UI_OPTIONS: &[&str] = &["managed-app"];
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
const DEFAULT_DATA_PROTECTION_OPTIONS: &[&str] = &[
    "NSFileProtectionCompleteUnlessOpen",
    "NSFileProtectionCompleteUntilFirstUserAuthentication",
    "NSFileProtectionNone",
    "NSFileProtectionComplete",
];

macro_rules! insert_boolean_entitlements {
    ($dictionary:expr, $entitlements:expr, { $($field:ident => $key:literal),* $(,)? }) => {
        $(
            if $entitlements.$field {
                $dictionary.insert($key.to_owned(), PlistValue::Boolean(true));
            }
        )*
    };
}

macro_rules! insert_string_entitlements {
    ($dictionary:expr, $entitlements:expr, { $($field:ident => $key:literal),* $(,)? }) => {
        $(
            if let Some(value) = &$entitlements.$field {
                $dictionary.insert($key.to_owned(), PlistValue::String(value.clone()));
            }
        )*
    };
}

macro_rules! insert_string_array_entitlements {
    ($dictionary:expr, $entitlements:expr, { $($field:ident => $key:literal),* $(,)? }) => {
        $(
            if !$entitlements.$field.is_empty() {
                $dictionary.insert(
                    $key.to_owned(),
                    PlistValue::Array(
                        $entitlements
                            .$field
                            .iter()
                            .cloned()
                            .map(PlistValue::String)
                            .collect(),
                    ),
                );
            }
        )*
    };
}

pub fn build_entitlements_dictionary(
    entitlements: &EntitlementsManifest,
    app_clip_parent_bundle_id: Option<&str>,
) -> Result<Option<Dictionary>> {
    if entitlements.is_empty() && app_clip_parent_bundle_id.is_none() {
        return Ok(None);
    }

    validate_entitlements_manifest(entitlements)?;

    let mut dictionary = Dictionary::new();

    insert_string_array_entitlements!(dictionary, entitlements, {
        app_groups => "com.apple.security.application-groups",
        associated_domains => "com.apple.developer.associated-domains",
        merchant_ids => "com.apple.developer.in-app-payments",
        cloud_containers => "com.apple.developer.icloud-container-identifiers",
        icloud_services => "com.apple.developer.icloud-services",
        network_extensions => "com.apple.developer.networking.networkextension",
        nfc_reader_session_formats => "com.apple.developer.nfc.readersession.formats",
        vpn_api => "com.apple.developer.networking.vpn.api",
        pass_type_identifiers => "com.apple.developer.pass-type-identifiers",
        apple_sign_in => "com.apple.developer.applesignin",
        user_fonts => "com.apple.developer.user-fonts",
        apple_pay_later_merchandising => "com.apple.developer.pay-later-merchandising",
        sensitive_content_analysis => "com.apple.developer.sensitivecontentanalysis.client",
        journal_allow => "com.apple.developer.journal.allow",
        managed_app_distribution_install_ui => "com.apple.developer.managed-app-distribution.install-ui",
        network_slicing_app_category => "com.apple.developer.networking.slicing.appcategory",
        network_slicing_traffic_category => "com.apple.developer.networking.slicing.trafficcategory"
    });

    insert_string_entitlements!(dictionary, entitlements, {
        classkit_environment => "com.apple.developer.ClassKit-environment",
        default_data_protection => "com.apple.developer.default-data-protection",
        app_attest_environment => "com.apple.developer.devicecheck.appattest-environment"
    });

    insert_boolean_entitlements!(dictionary, entitlements, {
        homekit => "com.apple.developer.homekit",
        hotspot_configuration => "com.apple.developer.networking.HotspotConfiguration",
        multipath => "com.apple.developer.networking.multipath",
        siri => "com.apple.developer.siri",
        wireless_accessory_configuration => "com.apple.external-accessory.wireless-configuration",
        extended_virtual_addressing => "com.apple.developer.kernel.extended-virtual-addressing",
        wifi_info => "com.apple.developer.networking.wifi-info",
        autofill_credential_provider => "com.apple.developer.authentication-services.autofill-credential-provider",
        healthkit => "com.apple.developer.healthkit",
        communication_notifications => "com.apple.developer.usernotifications.communication",
        time_sensitive_notifications => "com.apple.developer.usernotifications.time-sensitive",
        group_activities => "com.apple.developer.group-session",
        family_controls => "com.apple.developer.family-controls",
        inter_app_audio => "inter-app-audio",
        hls_low_latency => "com.apple.developer.coremedia.hls.low-latency",
        mdm_managed_associated_domains => "com.apple.developer.associated-domains.mdm-managed",
        fileprovider_testing_mode => "com.apple.developer.fileprovider.testing-mode",
        healthkit_recalibrate_estimates => "com.apple.developer.healthkit.recalibrate-estimates",
        maps => "com.apple.developer.maps",
        user_management => "com.apple.developer.user-management",
        custom_protocol => "com.apple.developer.networking.custom-protocol",
        system_extension_install => "com.apple.developer.system-extension.install",
        push_to_talk => "com.apple.developer.push-to-talk",
        driverkit_transport_usb => "com.apple.developer.driverkit.transport.usb",
        increased_memory_limit => "com.apple.developer.kernel.increased-memory-limit",
        driverkit_communicates_with_drivers => "com.apple.developer.driverkit.communicates-with-drivers",
        media_device_discovery_extension => "com.apple.developer.media-device-discovery-extension",
        driverkit_allow_third_party_userclients => "com.apple.developer.driverkit.allow-third-party-userclients",
        weatherkit => "com.apple.developer.weatherkit",
        on_demand_install_capable => "com.apple.developer.on-demand-install-capable",
        driverkit_family_scsi_controller => "com.apple.developer.driverkit.family.scsicontroller",
        driverkit_family_serial => "com.apple.developer.driverkit.family.serial",
        driverkit_family_networking => "com.apple.developer.driverkit.family.networking",
        driverkit_family_hid_eventservice => "com.apple.developer.driverkit.family.hid.eventservice",
        driverkit_family_hid_device => "com.apple.developer.driverkit.family.hid.device",
        driverkit => "com.apple.developer.driverkit",
        driverkit_transport_hid => "com.apple.developer.driverkit.transport.hid",
        driverkit_family_audio => "com.apple.developer.driverkit.family.audio",
        shared_with_you => "com.apple.developer.shared-with-you",
        shared_with_you_collaboration => "com.apple.developer.shared-with-you.collaboration",
        submerged_shallow_depth_and_pressure => "com.apple.developer.submerged-shallow-depth-and-pressure",
        proximity_reader_identity_display => "com.apple.developer.proximity-reader.identity.display",
        proximity_reader_payment_acceptance => "com.apple.developer.proximity-reader.payment.acceptance",
        matter_allow_setup_payload => "com.apple.developer.matter.allow-setup-payload"
    });

    if let Some(sandbox) = &entitlements.sandbox {
        if sandbox.enabled {
            dictionary.insert(
                "com.apple.security.app-sandbox".to_owned(),
                PlistValue::Boolean(true),
            );
        }
        for permission in &sandbox.network {
            dictionary.insert(
                sandbox_network_key(*permission).to_owned(),
                PlistValue::Boolean(true),
            );
        }
        for permission in &sandbox.files {
            dictionary.insert(
                sandbox_file_key(*permission).to_owned(),
                PlistValue::Boolean(true),
            );
        }
    }

    if let Some(parent_bundle_id) = app_clip_parent_bundle_id {
        dictionary.insert(
            "com.apple.developer.parent-application-identifiers".to_owned(),
            PlistValue::Array(vec![PlistValue::String(format!(
                "$(AppIdentifierPrefix){parent_bundle_id}"
            ))]),
        );
    }

    for (key, value) in &entitlements.extra {
        if dictionary.contains_key(key) {
            bail!("entitlements.extra cannot override generated entitlement `{key}`");
        }
        dictionary.insert(key.clone(), json_to_plist(value)?);
    }

    Ok(Some(dictionary))
}

fn validate_entitlements_manifest(entitlements: &EntitlementsManifest) -> Result<()> {
    ensure_prefixed_string_array("app_groups", &entitlements.app_groups, "group.")?;
    ensure_prefixed_string_array("merchant_ids", &entitlements.merchant_ids, "merchant.")?;
    ensure_prefixed_string_array(
        "cloud_containers",
        &entitlements.cloud_containers,
        "iCloud.",
    )?;
    ensure_allowed_string_array(
        "icloud_services",
        &entitlements.icloud_services,
        ICLOUD_SERVICE_OPTIONS,
    )?;
    ensure_allowed_string(
        "classkit_environment",
        entitlements.classkit_environment.as_deref(),
        DEV_PROD_OPTIONS,
    )?;
    ensure_allowed_string(
        "default_data_protection",
        entitlements.default_data_protection.as_deref(),
        DEFAULT_DATA_PROTECTION_OPTIONS,
    )?;
    ensure_allowed_string_array(
        "network_extensions",
        &entitlements.network_extensions,
        NETWORK_EXTENSION_OPTIONS,
    )?;
    ensure_allowed_string_array(
        "nfc_reader_session_formats",
        &entitlements.nfc_reader_session_formats,
        NFC_READER_SESSION_FORMAT_OPTIONS,
    )?;
    ensure_allowed_string_array("vpn_api", &entitlements.vpn_api, VPN_API_OPTIONS)?;
    ensure_allowed_string_array(
        "apple_sign_in",
        &entitlements.apple_sign_in,
        APPLE_SIGN_IN_OPTIONS,
    )?;
    ensure_allowed_string_array("user_fonts", &entitlements.user_fonts, USER_FONT_OPTIONS)?;
    ensure_allowed_string_array(
        "apple_pay_later_merchandising",
        &entitlements.apple_pay_later_merchandising,
        APPLE_PAY_LATER_OPTIONS,
    )?;
    ensure_allowed_string_array(
        "sensitive_content_analysis",
        &entitlements.sensitive_content_analysis,
        SENSITIVE_CONTENT_ANALYSIS_OPTIONS,
    )?;
    ensure_allowed_string(
        "app_attest_environment",
        entitlements.app_attest_environment.as_deref(),
        DEV_PROD_OPTIONS,
    )?;
    ensure_allowed_string_array(
        "journal_allow",
        &entitlements.journal_allow,
        JOURNAL_ALLOW_OPTIONS,
    )?;
    ensure_allowed_string_array(
        "managed_app_distribution_install_ui",
        &entitlements.managed_app_distribution_install_ui,
        MANAGED_APP_DISTRIBUTION_INSTALL_UI_OPTIONS,
    )?;
    ensure_allowed_string_array(
        "network_slicing_app_category",
        &entitlements.network_slicing_app_category,
        NETWORK_SLICING_APP_CATEGORY_OPTIONS,
    )?;
    ensure_allowed_string_array(
        "network_slicing_traffic_category",
        &entitlements.network_slicing_traffic_category,
        NETWORK_SLICING_TRAFFIC_CATEGORY_OPTIONS,
    )?;
    Ok(())
}

fn ensure_allowed_string(field: &str, value: Option<&str>, allowed: &[&str]) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if allowed.contains(&value) {
        return Ok(());
    }
    bail!(
        "`entitlements.{field}` must be one of [{}], got `{value}`",
        allowed.join(", ")
    )
}

fn ensure_allowed_string_array(field: &str, values: &[String], allowed: &[&str]) -> Result<()> {
    for value in values {
        if !allowed.contains(&value.as_str()) {
            bail!(
                "`entitlements.{field}` contains unsupported value `{value}`; allowed values: [{}]",
                allowed.join(", ")
            );
        }
    }
    Ok(())
}

fn ensure_prefixed_string_array(field: &str, values: &[String], prefix: &str) -> Result<()> {
    for value in values {
        if !value.starts_with(prefix) {
            bail!("`entitlements.{field}` values must start with `{prefix}`, got `{value}`");
        }
    }
    Ok(())
}

fn sandbox_network_key(permission: SandboxNetworkPermission) -> &'static str {
    match permission {
        SandboxNetworkPermission::Client => "com.apple.security.network.client",
        SandboxNetworkPermission::Server => "com.apple.security.network.server",
    }
}

fn sandbox_file_key(permission: SandboxFilePermission) -> &'static str {
    match permission {
        SandboxFilePermission::UserSelectedReadOnly => {
            "com.apple.security.files.user-selected.read-only"
        }
        SandboxFilePermission::UserSelectedReadWrite => {
            "com.apple.security.files.user-selected.read-write"
        }
    }
}

fn json_to_plist(value: &JsonValue) -> Result<PlistValue> {
    Ok(match value {
        JsonValue::Null => bail!("null values are not supported in entitlements"),
        JsonValue::Bool(value) => PlistValue::Boolean(*value),
        JsonValue::Number(value) => {
            if let Some(integer) = value.as_i64() {
                PlistValue::Integer(integer.into())
            } else if let Some(float) = value.as_f64() {
                PlistValue::Real(float)
            } else {
                bail!("JSON number `{value}` is not representable in a plist");
            }
        }
        JsonValue::String(value) => PlistValue::String(value.clone()),
        JsonValue::Array(values) => PlistValue::Array(
            values
                .iter()
                .map(json_to_plist)
                .collect::<Result<Vec<_>>>()?,
        ),
        JsonValue::Object(values) => PlistValue::Dictionary(Dictionary::from_iter(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_plist(value)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apple::manifest::authoring::SandboxConfig;

    #[test]
    fn builds_dictionary_for_supported_first_class_entitlements() {
        let entitlements = EntitlementsManifest {
            app_groups: vec!["group.dev.orbi.demo".to_owned()],
            associated_domains: vec!["applinks:orbi.dev".to_owned()],
            merchant_ids: vec!["merchant.dev.orbi.demo".to_owned()],
            cloud_containers: vec!["iCloud.dev.orbi.demo".to_owned()],
            icloud_services: vec!["CloudKit".to_owned()],
            classkit_environment: Some("development".to_owned()),
            default_data_protection: Some("NSFileProtectionComplete".to_owned()),
            network_extensions: vec!["packet-tunnel-provider".to_owned()],
            nfc_reader_session_formats: vec!["TAG".to_owned()],
            vpn_api: vec!["allow-vpn".to_owned()],
            pass_type_identifiers: vec!["$(TeamIdentifierPrefix)*".to_owned()],
            apple_sign_in: vec!["Default".to_owned()],
            user_fonts: vec!["app-usage".to_owned()],
            apple_pay_later_merchandising: vec!["payinfour-merchandising".to_owned()],
            sensitive_content_analysis: vec!["analysis".to_owned()],
            app_attest_environment: Some("production".to_owned()),
            journal_allow: vec!["suggestions".to_owned()],
            managed_app_distribution_install_ui: vec!["managed-app".to_owned()],
            network_slicing_app_category: vec!["gaming-6014".to_owned()],
            network_slicing_traffic_category: vec!["video-2".to_owned()],
            homekit: true,
            group_activities: true,
            sandbox: Some(SandboxConfig {
                enabled: true,
                network: vec![SandboxNetworkPermission::Client],
                files: vec![SandboxFilePermission::UserSelectedReadWrite],
            }),
            extra: [(
                "com.example.custom".to_owned(),
                JsonValue::String("value".to_owned()),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let dictionary = build_entitlements_dictionary(&entitlements, Some("dev.orbi.demo"))
            .expect("dictionary should build")
            .expect("dictionary should exist");

        assert_eq!(
            dictionary.get("com.apple.security.application-groups"),
            Some(&PlistValue::Array(vec![PlistValue::String(
                "group.dev.orbi.demo".to_owned()
            )]))
        );
        assert_eq!(
            dictionary.get("com.apple.developer.ClassKit-environment"),
            Some(&PlistValue::String("development".to_owned()))
        );
        assert_eq!(
            dictionary.get("com.apple.developer.networking.networkextension"),
            Some(&PlistValue::Array(vec![PlistValue::String(
                "packet-tunnel-provider".to_owned()
            )]))
        );
        assert_eq!(
            dictionary.get("com.apple.security.app-sandbox"),
            Some(&PlistValue::Boolean(true))
        );
        assert_eq!(
            dictionary.get("com.apple.developer.parent-application-identifiers"),
            Some(&PlistValue::Array(vec![PlistValue::String(
                "$(AppIdentifierPrefix)dev.orbi.demo".to_owned()
            )]))
        );
        assert_eq!(
            dictionary.get("com.example.custom"),
            Some(&PlistValue::String("value".to_owned()))
        );
    }

    #[test]
    fn rejects_invalid_allowed_values() {
        let entitlements = EntitlementsManifest {
            network_extensions: vec!["not-a-real-extension".to_owned()],
            ..Default::default()
        };

        let error = build_entitlements_dictionary(&entitlements, None)
            .expect_err("invalid value should fail");
        assert!(
            error
                .to_string()
                .contains("entitlements.network_extensions"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_extra_overrides_for_first_class_keys() {
        let entitlements = EntitlementsManifest {
            homekit: true,
            extra: [(
                "com.apple.developer.homekit".to_owned(),
                JsonValue::Bool(false),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };

        let error = build_entitlements_dictionary(&entitlements, None)
            .expect_err("extra override should fail");
        assert!(
            error
                .to_string()
                .contains("cannot override generated entitlement"),
            "unexpected error: {error}"
        );
    }
}
