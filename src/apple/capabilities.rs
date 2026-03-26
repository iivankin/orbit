use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::{Context, Result, bail};
use plist::Value;

use crate::apple::portal::PortalServiceUpdate;

#[derive(Debug, Clone, Default)]
pub struct CapabilityPlan {
    pub services: Vec<PortalServiceUpdate>,
    pub app_groups: Vec<String>,
    pub merchant_ids: Vec<String>,
    pub cloud_containers: Vec<String>,
}

pub fn capability_plan_from_entitlements(path: &Path) -> Result<CapabilityPlan> {
    let value = Value::from_file(path)
        .with_context(|| format!("failed to parse entitlements {}", path.display()))?;
    let dictionary = value
        .into_dictionary()
        .context("entitlements file must contain a top-level dictionary")?;

    let mut plan = CapabilityPlan::default();
    let mut services = HashMap::<&'static str, PortalServiceUpdate>::new();
    let ignored = ignored_entitlements();

    for (key, value) in &dictionary {
        if ignored.contains(key.as_str()) {
            continue;
        }

        match key.as_str() {
            "aps-environment" => {
                validate_push_environment(value)?;
                services.insert(
                    "push",
                    PortalServiceUpdate {
                        service_id: "push",
                        value: "true".to_owned(),
                        uses_push_uri: true,
                    },
                );
            }
            "com.apple.security.application-groups" => {
                let groups = prefixed_array(key, value, "group.")?;
                extend_unique(&mut plan.app_groups, groups);
                services.insert("app-groups", enabled_service("APG3427HIY"));
            }
            "com.apple.developer.in-app-payments" => {
                let merchants = prefixed_array(key, value, "merchant.")?;
                extend_unique(&mut plan.merchant_ids, merchants);
                services.insert("apple-pay", enabled_service("OM633U5T5G"));
            }
            "com.apple.developer.icloud-container-identifiers" => {
                let containers = prefixed_array(key, value, "iCloud.")?;
                extend_unique(&mut plan.cloud_containers, containers);
                services.insert("icloud", enabled_service("iCloud"));
            }
            "com.apple.developer.homekit" => {
                validate_boolean(key, value)?;
                services.insert("homekit", enabled_service("homeKit"));
            }
            "com.apple.developer.networking.HotspotConfiguration" => {
                validate_boolean(key, value)?;
                services.insert("hotspot", enabled_service("HSC639VEI8"));
            }
            "com.apple.developer.networking.multipath" => {
                validate_boolean(key, value)?;
                services.insert("multipath", enabled_service("MP49FN762P"));
            }
            "com.apple.developer.siri" => {
                validate_boolean(key, value)?;
                services.insert("sirikit", enabled_service("SI015DKUHP"));
            }
            "com.apple.external-accessory.wireless-configuration" => {
                validate_boolean(key, value)?;
                services.insert("wireless-accessory", enabled_service("WC421J6T7P"));
            }
            "com.apple.developer.networking.wifi-info" => {
                validate_boolean(key, value)?;
                services.insert("wifi-info", enabled_service("AWEQ28MY3E"));
            }
            "com.apple.developer.authentication-services.autofill-credential-provider" => {
                validate_boolean(key, value)?;
                services.insert("autofill", enabled_service("CPEQ28MX4E"));
            }
            "com.apple.developer.healthkit" => {
                validate_boolean(key, value)?;
                services.insert("healthkit", enabled_service("HK421J6T7P"));
            }
            "com.apple.developer.associated-domains" => {
                let _ = string_array(key, value)?;
                services.insert("associated-domains", enabled_service("SKC3T5S89Y"));
            }
            "com.apple.developer.ClassKit-environment" => {
                validate_dev_prod(key, value)?;
                services.insert("classkit", enabled_service("PKTJAN2017"));
            }
            "inter-app-audio" => {
                validate_boolean(key, value)?;
                services.insert("inter-app-audio", enabled_service("IAD53UNK2F"));
            }
            "com.apple.developer.networking.networkextension" => {
                let _ = allowed_string_array(
                    key,
                    value,
                    &[
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
                    ],
                )?;
                services.insert("network-extension", enabled_service("NWEXT04537"));
            }
            "com.apple.developer.nfc.readersession.formats" => {
                let _ = allowed_string_array(key, value, &["NDEF", "TAG"])?;
                services.insert("nfc", enabled_service("NFCTRMAY17"));
            }
            "com.apple.developer.networking.vpn.api" => {
                let _ = allowed_string_array(key, value, &["allow-vpn"])?;
                services.insert("vpn", enabled_service("V66P55NK2I"));
            }
            "com.apple.developer.default-data-protection" => {
                let protection = allowed_string(
                    key,
                    value,
                    &[
                        "NSFileProtectionCompleteUnlessOpen",
                        "NSFileProtectionCompleteUntilFirstUserAuthentication",
                        "NSFileProtectionNone",
                        "NSFileProtectionComplete",
                    ],
                )?;
                let mapped = match protection.as_str() {
                    "NSFileProtectionComplete" => "complete",
                    "NSFileProtectionCompleteUnlessOpen" => "unlessopen",
                    "NSFileProtectionCompleteUntilFirstUserAuthentication" => "untilfirstauth",
                    "NSFileProtectionNone" => "",
                    _ => unreachable!("validated data protection value"),
                };
                services.insert(
                    "data-protection",
                    PortalServiceUpdate {
                        service_id: "dataProtection",
                        value: mapped.to_owned(),
                        uses_push_uri: false,
                    },
                );
            }
            "com.apple.developer.game-center" => {
                validate_boolean(key, value)?;
                services.insert("game-center", enabled_service("gameCenter"));
            }
            "com.apple.developer.icloud-services" => {
                let _ = allowed_string_array(
                    key,
                    value,
                    &[
                        "CloudDocuments",
                        "CloudKit",
                        "CloudKit-Anonymous",
                        "CloudKit-Anonymous-Dev",
                    ],
                )?;
            }
            "com.apple.developer.applesignin"
            | "com.apple.developer.usernotifications.communication"
            | "com.apple.developer.usernotifications.time-sensitive"
            | "com.apple.developer.group-session"
            | "com.apple.developer.family-controls"
            | "com.apple.developer.user-fonts"
            | "com.apple.developer.pay-later-merchandising"
            | "com.apple.developer.sensitivecontentanalysis.client"
            | "com.apple.developer.devicecheck.appattest-environment"
            | "com.apple.developer.coremedia.hls.low-latency"
            | "com.apple.developer.associated-domains.mdm-managed"
            | "com.apple.developer.fileprovider.testing-mode"
            | "com.apple.developer.healthkit.recalibrate-estimates"
            | "com.apple.developer.maps"
            | "com.apple.developer.user-management"
            | "com.apple.developer.networking.custom-protocol"
            | "com.apple.developer.system-extension.install"
            | "com.apple.developer.push-to-talk"
            | "com.apple.developer.driverkit.transport.usb"
            | "com.apple.developer.kernel.increased-memory-limit"
            | "com.apple.developer.driverkit.communicates-with-drivers"
            | "com.apple.developer.media-device-discovery-extension"
            | "com.apple.developer.driverkit.allow-third-party-userclients"
            | "com.apple.developer.weatherkit"
            | "com.apple.developer.on-demand-install-capable"
            | "com.apple.developer.driverkit.family.scsicontroller"
            | "com.apple.developer.driverkit.family.serial"
            | "com.apple.developer.driverkit.family.networking"
            | "com.apple.developer.driverkit.family.hid.eventservice"
            | "com.apple.developer.driverkit.family.hid.device"
            | "com.apple.developer.driverkit"
            | "com.apple.developer.driverkit.transport.hid"
            | "com.apple.developer.driverkit.family.audio"
            | "com.apple.developer.shared-with-you"
            | "com.apple.developer.shared-with-you.collaboration"
            | "com.apple.developer.submerged-shallow-depth-and-pressure"
            | "com.apple.developer.proximity-reader.identity.display"
            | "com.apple.developer.proximity-reader.payment.acceptance"
            | "com.apple.developer.matter.allow-setup-payload"
            | "com.apple.developer.journal.allow"
            | "com.apple.developer.managed-app-distribution.install-ui"
            | "com.apple.developer.networking.slicing.appcategory"
            | "com.apple.developer.networking.slicing.trafficcategory"
            | "com.apple.developer.parent-application-identifiers" => {
                bail!(
                    "entitlement `{key}` is recognized, but Orbit does not support syncing the corresponding Apple capability yet"
                );
            }
            _ if key.starts_with("com.apple.")
                || key == "aps-environment"
                || key == "inter-app-audio" =>
            {
                bail!(
                    "entitlement `{key}` is not supported by Orbit yet; remove it or add capability support before signing"
                );
            }
            _ => {}
        }
    }

    plan.services = services.into_values().collect();
    plan.services
        .sort_by(|left, right| left.service_id.cmp(right.service_id));
    plan.app_groups.sort();
    plan.merchant_ids.sort();
    plan.cloud_containers.sort();
    Ok(plan)
}

fn enabled_service(service_id: &'static str) -> PortalServiceUpdate {
    PortalServiceUpdate {
        service_id,
        value: "true".to_owned(),
        uses_push_uri: false,
    }
}

fn validate_push_environment(value: &Value) -> Result<()> {
    let Some(environment) = value.as_string() else {
        bail!("`aps-environment` must be a string");
    };
    match environment {
        "development" | "production" => Ok(()),
        other => bail!("`aps-environment` must be `development` or `production`, got `{other}`"),
    }
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

fn extend_unique(target: &mut Vec<String>, values: Vec<String>) {
    let mut seen = target.iter().cloned().collect::<BTreeSet<_>>();
    for value in values {
        if seen.insert(value.clone()) {
            target.push(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use plist::Value;

    use super::capability_plan_from_entitlements;

    fn write_entitlements(temp: &tempfile::TempDir, value: Value) -> std::path::PathBuf {
        let path = temp.path().join("Example.entitlements");
        value.to_file_xml(&path).unwrap();
        path
    }

    #[test]
    fn parses_common_capabilities() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_entitlements(
            &temp,
            Value::Dictionary(plist::Dictionary::from_iter([
                (
                    "aps-environment".to_owned(),
                    Value::String("development".to_owned()),
                ),
                (
                    "com.apple.security.application-groups".to_owned(),
                    Value::Array(vec![Value::String("group.dev.orbit.demo".to_owned())]),
                ),
                (
                    "com.apple.developer.in-app-payments".to_owned(),
                    Value::Array(vec![Value::String("merchant.dev.orbit.demo".to_owned())]),
                ),
            ])),
        );

        let plan = capability_plan_from_entitlements(&path).unwrap();
        assert_eq!(plan.app_groups, vec!["group.dev.orbit.demo".to_owned()]);
        assert_eq!(
            plan.merchant_ids,
            vec!["merchant.dev.orbit.demo".to_owned()]
        );
        assert_eq!(plan.services.len(), 3);
    }

    #[test]
    fn rejects_known_but_unsupported_capabilities() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_entitlements(
            &temp,
            Value::Dictionary(plist::Dictionary::from_iter([(
                "com.apple.developer.applesignin".to_owned(),
                Value::Array(vec![Value::String("Default".to_owned())]),
            )])),
        );

        let error = capability_plan_from_entitlements(&path).unwrap_err();
        assert!(error.to_string().contains("recognized"));
    }
}
