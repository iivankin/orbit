use std::collections::HashSet;

use anyhow::Result;

use super::{
    CapabilityRelationships, CapabilityUpdate, ProjectContext, ProvisioningClient,
    delete_certificate_files, delete_file_if_exists, delete_p12_password, identifier_name,
    load_state, orbit_managed_app_name, resolve_local_team_id, resolve_local_team_id_if_known,
    save_state,
};
use crate::apple::capabilities::capability_sync_plan_from_entitlements;

#[derive(Debug, Clone, Default)]
pub struct LocalSigningCleanSummary {
    pub removed_profiles: usize,
    pub removed_certificates: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteSigningCleanSummary {
    pub removed_apps: usize,
    pub removed_profiles: usize,
    pub removed_app_groups: usize,
    pub removed_merchants: usize,
    pub removed_cloud_containers: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ProjectEntitlementIdentifiers {
    pub(super) app_groups: Vec<String>,
    pub(super) merchant_ids: Vec<String>,
    pub(super) cloud_containers: Vec<String>,
}

pub fn clean_local_signing_state(project: &ProjectContext) -> Result<LocalSigningCleanSummary> {
    let Some(team_id) = resolve_local_team_id_if_known(project)? else {
        return Ok(LocalSigningCleanSummary::default());
    };
    let mut state = load_state(project, &team_id)?;
    let bundle_ids = project_bundle_ids(project);
    let mut removed_profile_cert_ids = HashSet::new();
    let mut removed_profiles = 0usize;

    state.profiles.retain(|profile| {
        if !bundle_ids.contains(&profile.bundle_id) {
            return true;
        }
        let _ = delete_file_if_exists(&profile.path);
        removed_profile_cert_ids.extend(profile.certificate_ids.iter().cloned());
        removed_profiles += 1;
        false
    });

    let remaining_certificate_ids = state
        .profiles
        .iter()
        .flat_map(|profile| profile.certificate_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let mut removed_certificates = 0usize;
    state.certificates.retain(|certificate| {
        if !removed_profile_cert_ids.contains(&certificate.id)
            || remaining_certificate_ids.contains(&certificate.id)
        {
            return true;
        }
        let _ = delete_certificate_files(certificate);
        let _ = delete_p12_password(&certificate.p12_password_account);
        removed_certificates += 1;
        false
    });

    save_state(project, &team_id, &state)?;
    Ok(LocalSigningCleanSummary {
        removed_profiles,
        removed_certificates,
    })
}

pub fn clean_remote_signing_state(project: &ProjectContext) -> Result<RemoteSigningCleanSummary> {
    let team_id = resolve_local_team_id(project)?;
    let mut provisioning = ProvisioningClient::authenticate(&project.app, team_id.clone())?;
    let state = load_state(project, &team_id)?;
    let bundle_ids = project_bundle_ids(project);
    let orbit_app_name = orbit_managed_app_name(&project.resolved_manifest.name);
    let mut summary = RemoteSigningCleanSummary::default();

    let stored_project_profile_ids = state
        .profiles
        .iter()
        .filter(|profile| bundle_ids.contains(&profile.bundle_id))
        .map(|profile| profile.id.clone())
        .collect::<HashSet<_>>();
    remove_orbit_managed_profiles(
        &mut provisioning,
        &bundle_ids,
        &stored_project_profile_ids,
        &mut summary,
    )?;
    remove_orbit_managed_bundle_ids(&mut provisioning, project, &orbit_app_name, &mut summary)?;

    let ProjectEntitlementIdentifiers {
        app_groups,
        merchant_ids,
        cloud_containers,
    } = project_entitlement_identifiers(project)?;
    remove_orbit_managed_app_groups(&mut provisioning, app_groups, &mut summary)?;
    remove_orbit_managed_merchants(&mut provisioning, merchant_ids, &mut summary)?;
    remove_orbit_managed_cloud_containers(&mut provisioning, cloud_containers, &mut summary)?;

    Ok(summary)
}

fn remove_orbit_managed_profiles(
    provisioning: &mut ProvisioningClient,
    bundle_ids: &HashSet<String>,
    stored_project_profile_ids: &HashSet<String>,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    for profile in provisioning.list_profiles(None)? {
        let Some(bundle_identifier) = profile.bundle_id_identifier.as_deref() else {
            continue;
        };
        if !bundle_ids.contains(bundle_identifier) {
            continue;
        }
        if stored_project_profile_ids.contains(&profile.id) || profile.name.starts_with("*[orbit] ")
        {
            provisioning.delete_profile(&profile.id)?;
            summary.removed_profiles += 1;
        }
    }
    Ok(())
}

fn remove_orbit_managed_bundle_ids(
    provisioning: &mut ProvisioningClient,
    project: &ProjectContext,
    orbit_app_name: &str,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    for target in &project.resolved_manifest.targets {
        if let Some(bundle_id) = provisioning.find_bundle_id(&target.bundle_id)?
            && bundle_id.name == orbit_app_name
        {
            provisioning.delete_bundle_id(&bundle_id.id)?;
            summary.removed_apps += 1;
        }
    }
    Ok(())
}

fn remove_orbit_managed_app_groups(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    let app_groups = provisioning.list_app_groups()?;
    for identifier in identifiers {
        if let Some(group) = app_groups.iter().find(|group| {
            group.identifier == identifier
                && group.name == identifier_name("App Group", &identifier)
        }) {
            provisioning.delete_app_group(&group.id)?;
            summary.removed_app_groups += 1;
        }
    }
    Ok(())
}

fn remove_orbit_managed_merchants(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    let merchants = provisioning.list_merchant_ids()?;
    for identifier in identifiers {
        if let Some(merchant) = merchants.iter().find(|merchant| {
            merchant.identifier == identifier
                && merchant.name == identifier_name("Merchant ID", &identifier)
        }) {
            provisioning.delete_merchant_id(&merchant.id)?;
            summary.removed_merchants += 1;
        }
    }
    Ok(())
}

fn remove_orbit_managed_cloud_containers(
    provisioning: &mut ProvisioningClient,
    identifiers: Vec<String>,
    summary: &mut RemoteSigningCleanSummary,
) -> Result<()> {
    let containers = provisioning.list_cloud_containers()?;
    for identifier in identifiers {
        if let Some(container) = containers.iter().find(|container| {
            container.identifier == identifier
                && container.name == identifier_name("iCloud Container", &identifier)
        }) {
            provisioning.delete_cloud_container(&container.id)?;
            summary.removed_cloud_containers += 1;
        }
    }
    Ok(())
}

fn project_bundle_ids(project: &ProjectContext) -> HashSet<String> {
    project
        .resolved_manifest
        .targets
        .iter()
        .map(|target| target.bundle_id.clone())
        .collect()
}

pub(super) fn project_entitlement_identifiers(
    project: &ProjectContext,
) -> Result<ProjectEntitlementIdentifiers> {
    let mut app_groups = HashSet::new();
    let mut merchant_ids = HashSet::new();
    let mut cloud_containers = HashSet::new();

    for target in &project.resolved_manifest.targets {
        let Some(entitlements_path) = &target.entitlements else {
            continue;
        };
        let plan =
            capability_sync_plan_from_entitlements(&project.root.join(entitlements_path), &[])?;
        app_groups.extend(collect_identifier_values(&plan.updates, |relationships| {
            relationships.app_groups.as_ref()
        }));
        cloud_containers.extend(collect_identifier_values(&plan.updates, |relationships| {
            relationships.cloud_containers.as_ref()
        }));
        merchant_ids.extend(collect_identifier_values(&plan.updates, |relationships| {
            relationships.merchant_ids.as_ref()
        }));
    }

    Ok(ProjectEntitlementIdentifiers {
        app_groups: sorted_strings(app_groups),
        merchant_ids: sorted_strings(merchant_ids),
        cloud_containers: sorted_strings(cloud_containers),
    })
}

pub(super) fn collect_identifier_values<F>(updates: &[CapabilityUpdate], select: F) -> Vec<String>
where
    F: Fn(&CapabilityRelationships) -> Option<&Vec<String>>,
{
    let mut values = updates
        .iter()
        .flat_map(|update| {
            select(&update.relationships)
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn sorted_strings(values: HashSet<String>) -> Vec<String> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values
}
