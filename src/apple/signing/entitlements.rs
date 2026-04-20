use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use plist::{Dictionary, Value};

use super::{ProjectContext, TargetManifest};
use crate::util::ensure_dir;

const APP_IDENTIFIER_PREFIX_PLACEHOLDER: &str = "$(AppIdentifierPrefix)";
const TEAM_IDENTIFIER_PREFIX_PLACEHOLDER: &str = "$(TeamIdentifierPrefix)";
const MANAGED_SIGNING_ENTITLEMENTS: &[&str] = &[
    "application-identifier",
    "aps-environment",
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
];

pub(super) fn host_app_for_app_clip<'a>(
    project: &'a ProjectContext,
    target: &'a TargetManifest,
) -> Result<Option<&'a TargetManifest>> {
    if !is_app_clip_target(project, target)? {
        return Ok(None);
    }

    let mut hosts = project
        .resolved_manifest
        .targets
        .iter()
        .filter(|candidate| {
            candidate.name != target.name
                && candidate.kind == crate::manifest::TargetKind::App
                && candidate
                    .dependencies
                    .iter()
                    .any(|dependency| dependency == &target.name)
        })
        .collect::<Vec<_>>();
    match hosts.len() {
        0 => Ok(None),
        1 => Ok(hosts.pop()),
        _ => bail!(
            "App Clip target `{}` cannot be hosted by more than one app target",
            target.name
        ),
    }
}

pub fn target_is_app_clip(project: &ProjectContext, target: &TargetManifest) -> Result<bool> {
    is_app_clip_target(project, target)
}

pub(super) fn materialize_signing_entitlements(
    project: &ProjectContext,
    target: &TargetManifest,
    provisioning_profile_path: &Path,
) -> Result<Option<PathBuf>> {
    let original_path = target
        .entitlements
        .as_ref()
        .map(|path| project.root.join(path));
    let profile_entitlements = provisioning_profile_entitlements(provisioning_profile_path)?;
    let mut entitlements = match &original_path {
        Some(path) => load_plist_dictionary(path)?,
        None => profile_entitlements.clone(),
    };
    let application_identifier_prefix =
        provisioning_profile_application_identifier_prefix(provisioning_profile_path)?;
    let mut changed = original_path.is_none();
    changed |= replace_entitlement_placeholders_in_dictionary(
        &mut entitlements,
        &application_identifier_prefix,
    );

    if let Some(parent_identifiers) = parent_application_identifiers_from_dictionary(&entitlements)?
    {
        let Some(host_target) = host_app_for_app_clip(project, target)? else {
            bail!(
                "App Clip target `{}` must be hosted by an app target in the manifest",
                target.name
            );
        };
        let expected_parent = format!("{application_identifier_prefix}{}", host_target.bundle_id);
        if parent_identifiers[0] != expected_parent {
            bail!(
                "App Clip target `{}` must reference its host app application identifier `{expected_parent}`",
                target.name
            );
        }
        if !target
            .bundle_id
            .starts_with(&format!("{}.", host_target.bundle_id))
        {
            bail!(
                "App Clip target `{}` bundle ID `{}` must use the host app bundle ID `{}` as its prefix",
                target.name,
                target.bundle_id,
                host_target.bundle_id
            );
        }
        changed |= set_dictionary_boolean(
            &mut entitlements,
            "com.apple.developer.on-demand-install-capable",
            true,
        );
    }

    let hosted_app_clip_identifiers = hosted_app_clip_targets(project, target)?
        .into_iter()
        .map(|hosted_target| format!("{application_identifier_prefix}{}", hosted_target.bundle_id))
        .collect::<Vec<_>>();
    if !hosted_app_clip_identifiers.is_empty() {
        if hosted_app_clip_identifiers.len() != 1 {
            bail!(
                "app target `{}` cannot host more than one App Clip because `com.apple.developer.associated-appclip-app-identifiers` must contain exactly one entry",
                target.name
            );
        }
        changed |= set_dictionary_string_array(
            &mut entitlements,
            "com.apple.developer.associated-appclip-app-identifiers",
            hosted_app_clip_identifiers,
        );
    }

    changed |= merge_managed_signing_entitlements(&mut entitlements, &profile_entitlements);
    if !changed {
        return Ok(original_path);
    }

    let generated_dir = project
        .project_paths
        .orbi_dir
        .join("signing")
        .join("entitlements");
    ensure_dir(&generated_dir)?;
    let path = generated_dir.join(format!("{}.entitlements", target.name));
    Value::Dictionary(entitlements)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(Some(path))
}

pub(super) fn materialize_local_macos_development_entitlements(
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<Option<PathBuf>> {
    let original_path = target
        .entitlements
        .as_ref()
        .map(|path| project.root.join(path));
    let mut entitlements = original_path
        .as_ref()
        .map(|path| load_plist_dictionary(path))
        .transpose()?
        .unwrap_or_default();
    ensure_local_macos_entitlements_supported(target, &entitlements)?;
    let changed =
        set_dictionary_boolean(&mut entitlements, "com.apple.security.get-task-allow", true);

    if !changed {
        return Ok(original_path);
    }

    let generated_dir = project
        .project_paths
        .orbi_dir
        .join("signing")
        .join("entitlements");
    ensure_dir(&generated_dir)?;
    let path = generated_dir.join(format!("{}.local-debug.entitlements", target.name));
    Value::Dictionary(entitlements)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(Some(path))
}

pub(super) fn materialize_macos_debug_trace_entitlements(
    project: &ProjectContext,
    target: &TargetManifest,
    _bundle_path: &Path,
) -> Result<PathBuf> {
    let mut entitlements = target
        .entitlements
        .as_ref()
        .map(|entitlements_path| load_plist_dictionary(&project.root.join(entitlements_path)))
        .transpose()?
        .unwrap_or_default();
    set_dictionary_boolean(&mut entitlements, "com.apple.security.get-task-allow", true);

    let generated_dir = project
        .project_paths
        .orbi_dir
        .join("signing")
        .join("entitlements");
    ensure_dir(&generated_dir)?;
    let path = generated_dir.join(format!("{}.debug.entitlements", target.name));
    Value::Dictionary(entitlements)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub(super) fn load_plist_dictionary(path: &Path) -> Result<Dictionary> {
    Value::from_file(path)
        .with_context(|| format!("failed to parse plist {}", path.display()))?
        .into_dictionary()
        .context("plist must contain a top-level dictionary")
}

fn hosted_app_clip_targets<'a>(
    project: &'a ProjectContext,
    target: &'a TargetManifest,
) -> Result<Vec<&'a TargetManifest>> {
    let mut hosted = Vec::new();
    for dependency_name in &target.dependencies {
        let dependency = project
            .resolved_manifest
            .resolve_target(Some(dependency_name))?;
        if is_app_clip_target(project, dependency)? {
            hosted.push(dependency);
        }
    }
    Ok(hosted)
}

fn is_app_clip_target(project: &ProjectContext, target: &TargetManifest) -> Result<bool> {
    if target.kind != crate::manifest::TargetKind::App {
        return Ok(false);
    }
    Ok(target_parent_application_identifiers(project, target)?.is_some())
}

fn target_parent_application_identifiers(
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<Option<Vec<String>>> {
    let Some(entitlements_path) = &target.entitlements else {
        return Ok(None);
    };
    let entitlements = load_plist_dictionary(&project.root.join(entitlements_path))?;
    parent_application_identifiers_from_dictionary(&entitlements)
}

fn parent_application_identifiers_from_dictionary(
    dictionary: &Dictionary,
) -> Result<Option<Vec<String>>> {
    let Some(value) = dictionary.get("com.apple.developer.parent-application-identifiers") else {
        return Ok(None);
    };
    let values = string_array_value("com.apple.developer.parent-application-identifiers", value)?;
    if values.len() != 1 {
        bail!(
            "`com.apple.developer.parent-application-identifiers` must contain exactly one application identifier"
        );
    }
    Ok(Some(values))
}

fn provisioning_profile_entitlements(path: &Path) -> Result<Dictionary> {
    Ok(load_provisioning_profile_dictionary(path)?
        .get("Entitlements")
        .and_then(Value::as_dictionary)
        .cloned()
        .unwrap_or_default())
}

fn provisioning_profile_application_identifier_prefix(path: &Path) -> Result<String> {
    let profile = load_provisioning_profile_dictionary(path)?;
    if let Some(prefixes) = profile
        .get("ApplicationIdentifierPrefix")
        .and_then(Value::as_array)
        && let Some(prefix) = prefixes.first().and_then(Value::as_string)
    {
        return Ok(normalize_application_identifier_prefix(prefix));
    }

    let application_identifier = profile
        .get("Entitlements")
        .and_then(Value::as_dictionary)
        .and_then(|entitlements| entitlements.get("application-identifier"))
        .and_then(Value::as_string)
        .context("provisioning profile is missing an application identifier prefix")?;
    let prefix = application_identifier
        .split_once('.')
        .map(|(prefix, _)| prefix)
        .unwrap_or(application_identifier);
    Ok(normalize_application_identifier_prefix(prefix))
}

fn load_provisioning_profile_dictionary(path: &Path) -> Result<Dictionary> {
    if let Ok(value) = Value::from_file(path)
        && let Some(dictionary) = value.into_dictionary()
    {
        return Ok(dictionary);
    }

    let output =
        crate::util::command_output(Command::new("security").args(["cms", "-D", "-i"]).arg(path))?;
    Value::from_reader_xml(output.as_bytes())
        .context("failed to decode provisioning profile CMS payload")?
        .into_dictionary()
        .context("decoded provisioning profile did not contain a top-level dictionary")
}

fn normalize_application_identifier_prefix(prefix: &str) -> String {
    if prefix.ends_with('.') {
        prefix.to_owned()
    } else {
        format!("{prefix}.")
    }
}

fn ensure_local_macos_entitlements_supported(
    target: &TargetManifest,
    entitlements: &Dictionary,
) -> Result<()> {
    if contains_entitlement_placeholders(entitlements) {
        bail!(
            "target `{}` uses entitlement placeholders like `$(AppIdentifierPrefix)` without an embedded `asc` section; add `asc` and run `orbi asc apply`, or replace the placeholder-based entitlements with literal macOS values",
            target.name
        );
    }

    let unsupported = entitlements
        .keys()
        .filter(|key| !key.starts_with("com.apple.security."))
        .cloned()
        .collect::<Vec<_>>();
    if unsupported.is_empty() {
        return Ok(());
    }

    bail!(
        "target `{}` claims provisioning-backed macOS entitlements without an embedded `asc` section: {}; local macOS development fallback only supports unrestricted `com.apple.security.*` entitlements. Add `asc` and run `orbi asc apply`, or remove the restricted entitlements",
        target.name,
        unsupported.join(", ")
    )
}

fn contains_entitlement_placeholders(dictionary: &Dictionary) -> bool {
    dictionary
        .values()
        .any(value_contains_entitlement_placeholders)
}

fn value_contains_entitlement_placeholders(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(value_contains_entitlement_placeholders),
        Value::Dictionary(dictionary) => contains_entitlement_placeholders(dictionary),
        Value::String(text) => {
            text.contains(APP_IDENTIFIER_PREFIX_PLACEHOLDER)
                || text.contains(TEAM_IDENTIFIER_PREFIX_PLACEHOLDER)
        }
        _ => false,
    }
}

fn replace_entitlement_placeholders_in_dictionary(
    dictionary: &mut Dictionary,
    application_identifier_prefix: &str,
) -> bool {
    let mut changed = false;
    for value in dictionary.values_mut() {
        changed |= replace_entitlement_placeholders_in_value(value, application_identifier_prefix);
    }
    changed
}

fn replace_entitlement_placeholders_in_value(
    value: &mut Value,
    application_identifier_prefix: &str,
) -> bool {
    match value {
        Value::Array(values) => values.iter_mut().any(|value| {
            replace_entitlement_placeholders_in_value(value, application_identifier_prefix)
        }),
        Value::Dictionary(dictionary) => replace_entitlement_placeholders_in_dictionary(
            dictionary,
            application_identifier_prefix,
        ),
        Value::String(text) => {
            let replaced = text
                .replace(
                    APP_IDENTIFIER_PREFIX_PLACEHOLDER,
                    application_identifier_prefix,
                )
                .replace(
                    TEAM_IDENTIFIER_PREFIX_PLACEHOLDER,
                    application_identifier_prefix,
                );
            if replaced != *text {
                *text = replaced;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

fn set_dictionary_boolean(dictionary: &mut Dictionary, key: &str, value: bool) -> bool {
    let next = Value::Boolean(value);
    if dictionary.get(key) == Some(&next) {
        return false;
    }
    dictionary.insert(key.to_owned(), next);
    true
}

fn set_dictionary_string_array(
    dictionary: &mut Dictionary,
    key: &str,
    values: Vec<String>,
) -> bool {
    let next = Value::Array(values.into_iter().map(Value::String).collect());
    if dictionary.get(key) == Some(&next) {
        return false;
    }
    dictionary.insert(key.to_owned(), next);
    true
}

fn merge_managed_signing_entitlements(
    target: &mut Dictionary,
    profile_entitlements: &Dictionary,
) -> bool {
    let mut changed = false;
    for key in MANAGED_SIGNING_ENTITLEMENTS {
        let Some(value) = profile_entitlements.get(key) else {
            continue;
        };
        if target.get(key) == Some(value) {
            continue;
        }
        target.insert((*key).to_owned(), value.clone());
        changed = true;
    }
    changed
}

fn string_array_value(key: &str, value: &Value) -> Result<Vec<String>> {
    let Some(values) = value.as_array() else {
        bail!("`{key}` must be an array");
    };
    values
        .iter()
        .map(|value| {
            value
                .as_string()
                .map(ToOwned::to_owned)
                .with_context(|| format!("`{key}` must contain only strings"))
        })
        .collect()
}
