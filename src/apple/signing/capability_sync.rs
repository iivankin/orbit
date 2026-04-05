use std::collections::HashMap;

use super::cleanup::collect_identifier_values;
use super::*;
use crate::apple::provisioning::ProvisioningCapabilityPatch;

fn entitlement_dictionary_for_capability_sync(
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<Dictionary> {
    match &target.entitlements {
        Some(entitlements_path) => load_plist_dictionary(&project.root.join(entitlements_path)),
        None => Ok(Dictionary::new()),
    }
}

fn capability_sync_plan_for_target(
    project: &ProjectContext,
    target: &TargetManifest,
    remote_capabilities: &[RemoteCapability],
    options: &CapabilitySyncOptions,
) -> Result<crate::apple::capabilities::CapabilitySyncPlan> {
    let entitlements = entitlement_dictionary_for_capability_sync(project, target)?;
    capability_sync_plan_from_dictionary_with_options(&entitlements, remote_capabilities, options)
}

fn capability_sync_options_for_target(target: &TargetManifest) -> CapabilitySyncOptions {
    CapabilitySyncOptions {
        uses_push_notifications: target.push.is_some(),
        uses_broadcast_push_notifications: target
            .push
            .as_ref()
            .is_some_and(|push| push.broadcast_for_live_activities),
    }
}

pub(super) fn validate_push_setup_with_api_key(target: &TargetManifest) -> CapabilitySyncOptions {
    let mut options = capability_sync_options_for_target(target);
    if !options.uses_broadcast_push_notifications {
        return options;
    }

    eprintln!(
        "warning: App Store Connect API key auth cannot configure broadcast push settings for target `{}`; continuing without broadcast support. Enable Broadcast Push Notifications manually in the Apple developer console if needed.",
        target.name
    );
    options.uses_broadcast_push_notifications = false;
    options
}

pub(super) fn ensure_bundle_id_with_api_key(
    client: &AscClient,
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
) -> Result<Resource<BundleIdAttributes>> {
    if let Some(bundle_id) = client
        .find_bundle_id(&target.bundle_id)?
        .map(|document| document.data)
    {
        return Ok(bundle_id);
    }

    client.create_bundle_id(
        &orbit_managed_app_name(&project.resolved_manifest.name),
        &target.bundle_id,
        asc_bundle_id_platform(platform),
    )
}

pub(super) fn sync_capabilities_with_api_key(
    client: &AscClient,
    project: &ProjectContext,
    target: &TargetManifest,
    bundle_id: &Resource<BundleIdAttributes>,
) -> Result<CapabilitySyncOutcome> {
    let capability_sync_options = validate_push_setup_with_api_key(target);
    if target.entitlements.is_none() && !capability_sync_options.uses_push_notifications {
        return Ok(CapabilitySyncOutcome::Skipped);
    }
    let remote_bundle = client
        .find_bundle_id(&bundle_id.attributes.identifier)?
        .with_context(|| {
            format!(
                "bundle identifier `{}` exists in App Store Connect but could not be reloaded",
                bundle_id.attributes.identifier
            )
        })?;
    let remote_capabilities = remote_capabilities_from_included(&remote_bundle.included)?;
    let plan = capability_sync_plan_for_target(
        project,
        target,
        &remote_capabilities,
        &capability_sync_options,
    )?;
    if plan.updates.is_empty() {
        return Ok(CapabilitySyncOutcome::NoUpdates);
    }

    let mutations = plan_asc_capability_mutations(&plan.updates, &remote_capabilities)?;
    for mutation in mutations {
        if mutation.delete {
            if let Some(remote_id) = mutation.remote_id {
                client.delete_bundle_capability(&remote_id)?;
            }
            continue;
        }

        match mutation.remote_id {
            Some(remote_id) => {
                let _ = client.update_bundle_capability(
                    &remote_id,
                    &mutation.capability_type,
                    &mutation.settings,
                )?;
            }
            None => {
                let _ = client.create_bundle_capability(
                    &bundle_id.id,
                    &mutation.capability_type,
                    &mutation.settings,
                )?;
            }
        }
    }
    Ok(CapabilitySyncOutcome::Updated(plan.updates.len()))
}

pub(super) fn plan_asc_capability_mutations(
    updates: &[CapabilityUpdate],
    remote_capabilities: &[RemoteCapability],
) -> Result<Vec<AscCapabilityMutation>> {
    let mut mutations = Vec::new();
    for update in updates {
        if !ASC_SUPPORTED_CAPABILITIES.contains(&update.capability_type.as_str()) {
            bail!(
                "App Store Connect API key auth does not support syncing capability `{}`; log in with Apple ID so Orbit can use the Developer Portal flow",
                update.capability_type
            );
        }
        if update.relationships.app_groups.is_some() {
            bail!(
                "App Store Connect API key auth cannot link App Groups for capability `{}`; log in with Apple ID so Orbit can use the Developer Portal flow",
                update.capability_type
            );
        }
        if update.relationships.cloud_containers.is_some() {
            bail!(
                "App Store Connect API key auth cannot link iCloud containers for capability `{}`; log in with Apple ID so Orbit can use the Developer Portal flow",
                update.capability_type
            );
        }
        if update.relationships.merchant_ids.is_some() {
            bail!(
                "App Store Connect API key auth cannot link merchant IDs for capability `{}`; log in with Apple ID so Orbit can use the Developer Portal flow",
                update.capability_type
            );
        }

        let remote_id = remote_capabilities
            .iter()
            .find(|candidate| candidate.capability_type == update.capability_type)
            .map(|candidate| candidate.id.clone());
        if update.option == ASC_OPTION_OFF {
            mutations.push(AscCapabilityMutation {
                remote_id,
                capability_type: update.capability_type.clone(),
                settings: Vec::new(),
                delete: true,
            });
            continue;
        }

        mutations.push(AscCapabilityMutation {
            remote_id,
            capability_type: update.capability_type.clone(),
            settings: asc_capability_settings(update)?,
            delete: false,
        });
    }
    Ok(mutations)
}

pub(super) fn asc_capability_settings(update: &CapabilityUpdate) -> Result<Vec<CapabilitySetting>> {
    let setting = match update.capability_type.as_str() {
        "ICLOUD" => match update.option.as_str() {
            ASC_OPTION_ON => Some((ASC_SETTING_ICLOUD_VERSION, ASC_OPTION_ICLOUD_XCODE_6)),
            "XCODE_5" | ASC_OPTION_ICLOUD_XCODE_6 => {
                Some((ASC_SETTING_ICLOUD_VERSION, update.option.as_str()))
            }
            other => {
                bail!("App Store Connect API key auth does not support iCloud option `{other}`")
            }
        },
        "DATA_PROTECTION" => match update.option.as_str() {
            ASC_OPTION_ON => Some((
                ASC_SETTING_DATA_PROTECTION,
                ASC_OPTION_DATA_PROTECTION_COMPLETE,
            )),
            ASC_OPTION_DATA_PROTECTION_COMPLETE
            | ASC_OPTION_DATA_PROTECTION_PROTECTED_UNLESS_OPEN
            | ASC_OPTION_DATA_PROTECTION_PROTECTED_UNTIL_FIRST_USER_AUTH => {
                Some((ASC_SETTING_DATA_PROTECTION, update.option.as_str()))
            }
            other => bail!(
                "App Store Connect API key auth does not support data protection option `{other}`"
            ),
        },
        "APPLE_ID_AUTH" => match update.option.as_str() {
            ASC_OPTION_ON => Some((
                ASC_SETTING_APPLE_ID_AUTH,
                ASC_OPTION_APPLE_ID_PRIMARY_CONSENT,
            )),
            ASC_OPTION_APPLE_ID_PRIMARY_CONSENT => {
                Some((ASC_SETTING_APPLE_ID_AUTH, update.option.as_str()))
            }
            other => bail!(
                "App Store Connect API key auth does not support Sign In with Apple option `{other}`"
            ),
        },
        "PUSH_NOTIFICATIONS" if update.option == ASC_OPTION_PUSH_BROADCAST => {
            bail!(
                "App Store Connect API key auth cannot configure broadcast push settings; log in with Apple ID so Orbit can use the Developer Portal flow"
            )
        }
        _ => None,
    };

    Ok(setting
        .into_iter()
        .map(|(key, option)| CapabilitySetting {
            key: key.to_owned(),
            options: vec![CapabilityOption {
                key: option.to_owned(),
                enabled: true,
            }],
        })
        .collect())
}

pub(super) fn ensure_bundle_id_with_developer_services(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<ProvisioningBundleId> {
    provisioning.ensure_bundle_id(
        &orbit_managed_app_name(&project.resolved_manifest.name),
        &target.bundle_id,
    )
}

pub(super) fn sync_capabilities(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    target: &TargetManifest,
    bundle_id: &ProvisioningBundleId,
) -> Result<CapabilitySyncOutcome> {
    let capability_sync_options = capability_sync_options_for_target(target);
    if target.entitlements.is_none() && !capability_sync_options.uses_push_notifications {
        return Ok(CapabilitySyncOutcome::Skipped);
    }
    let plan = capability_sync_plan_for_target(
        project,
        target,
        &bundle_id.capabilities,
        &capability_sync_options,
    )?;
    if plan.updates.is_empty() {
        return Ok(CapabilitySyncOutcome::NoUpdates);
    }

    let app_group_ids = resolve_app_group_ids(
        provisioning,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.app_groups.as_deref()
        }),
    )?;
    let merchant_ids = resolve_merchant_ids(
        provisioning,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.merchant_ids.as_deref()
        }),
    )?;
    let cloud_container_ids = resolve_cloud_container_ids(
        provisioning,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.cloud_containers.as_deref()
        }),
    )?;
    let updates = plan
        .updates
        .iter()
        .map(|update| {
            let remote_id = bundle_id
                .capabilities
                .iter()
                .find(|candidate| candidate.capability_type == update.capability_type)
                .map(|candidate| candidate.id.clone());
            Ok(ProvisioningCapabilityPatch {
                remote_id,
                update: CapabilityUpdate {
                    capability_type: update.capability_type.clone(),
                    option: update.option.clone(),
                    relationships: CapabilityRelationships {
                        app_groups: map_relationship_ids(
                            update.relationships.app_groups.as_deref(),
                            &app_group_ids,
                        )?,
                        merchant_ids: map_relationship_ids(
                            update.relationships.merchant_ids.as_deref(),
                            &merchant_ids,
                        )?,
                        cloud_containers: map_relationship_ids(
                            update.relationships.cloud_containers.as_deref(),
                            &cloud_container_ids,
                        )?,
                    },
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let (deletes, upserts): (Vec<_>, Vec<_>) = updates
        .into_iter()
        .partition(|update| update.update.option == ASC_OPTION_OFF);
    for delete in deletes {
        if delete.update.capability_type == "ASSOCIATED_DOMAINS" {
            // Xcode does not emit a matching disable mutation when users remove
            // Associated Domains in Signing & Capabilities, so keep the remote
            // capability untouched to stay aligned with Apple tooling.
            continue;
        }
        if let Some(remote_id) = delete.remote_id.as_deref() {
            provisioning.delete_bundle_capability(remote_id)?;
        }
    }
    if !upserts.is_empty() {
        provisioning.update_bundle_capabilities(bundle_id, &upserts)?;
    }
    Ok(CapabilitySyncOutcome::Updated(plan.updates.len()))
}

fn resolve_app_group_ids(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = provisioning.list_app_groups()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_group) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_group.id.clone()
        } else {
            let name = identifier_name("App Group", &identifier);
            provisioning.create_app_group(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn resolve_merchant_ids(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = provisioning.list_merchant_ids()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_merchant) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_merchant.id.clone()
        } else {
            let name = identifier_name("Merchant ID", &identifier);
            provisioning.create_merchant_id(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn resolve_cloud_container_ids(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = provisioning.list_cloud_containers()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_container) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_container.id.clone()
        } else {
            let name = identifier_name("iCloud Container", &identifier);
            provisioning.create_cloud_container(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn map_relationship_ids(
    identifiers: Option<&[String]>,
    resolved: &HashMap<String, String>,
) -> Result<Option<Vec<String>>> {
    let Some(identifiers) = identifiers else {
        return Ok(None);
    };
    identifiers
        .iter()
        .map(|identifier| {
            resolved
                .get(identifier)
                .cloned()
                .with_context(|| format!("missing Apple identifier record for `{identifier}`"))
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}
