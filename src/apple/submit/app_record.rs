use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use plist::Value as PlistValue;

use crate::apple::asc_api::AscClient;
use crate::apple::asc_session::{AscSessionAppsClient, CreateAppRecordInput};
use crate::context::ProjectContext;
use crate::manifest::DistributionKind;

use super::super::build::receipt::BuildReceipt;

const APP_RECORD_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(90);
const APP_RECORD_VISIBILITY_POLL_INTERVAL: Duration = Duration::from_secs(3);

pub(super) fn ensure_submit_app_record(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    provider_public_id: Option<&str>,
) -> Result<()> {
    if !matches!(
        receipt.distribution,
        DistributionKind::AppStore | DistributionKind::MacAppStore
    ) {
        return Ok(());
    }

    if let Some(api_key_auth) = crate::apple::auth::resolve_api_key_auth(&project.app)? {
        ensure_submit_app_record_with_api_key(project, receipt, api_key_auth)?;
        return Ok(());
    }

    let provider_public_id = provider_public_id.context(
        "Apple ID submit requires a provider public ID before creating the App Store Connect app record",
    )?;
    ensure_submit_app_record_with_session(project, receipt, provider_public_id)
}

fn ensure_submit_app_record_with_api_key(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    api_key_auth: crate::apple::auth::ApiKeyAuth,
) -> Result<()> {
    let client = AscClient::new(api_key_auth)?;
    let bundle_id = client
        .find_bundle_id(&receipt.bundle_id)?
        .with_context(|| {
            format!(
                "missing App Store Connect bundle ID for `{}`",
                receipt.bundle_id
            )
        })?;
    if client.find_app_by_bundle_id(&bundle_id.data.id)?.is_some() {
        return Ok(());
    }

    let app_name = project.resolved_manifest.name.clone();
    let sku = app_store_sku(&receipt.bundle_id);
    let _ = client.create_app_record(&app_name, &sku, "en-US", &bundle_id.data.id)?;
    wait_for_api_key_app_record(&client, &bundle_id.data.id)
}

fn ensure_submit_app_record_with_session(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    provider_public_id: &str,
) -> Result<()> {
    let client = AscSessionAppsClient::authenticate(&project.app, provider_public_id.to_owned())?;
    if client.find_app_by_bundle_id(&receipt.bundle_id)?.is_some() {
        return Ok(());
    }

    let app_name = project.resolved_manifest.name.clone();
    let sku = app_store_sku(&receipt.bundle_id);
    let version_number = bundle_short_version(receipt)?;
    let request = CreateAppRecordInput {
        name: &app_name,
        sku: &sku,
        primary_locale: "en-US",
        bundle_id: &receipt.bundle_id,
        platform: receipt.platform,
        version_number: &version_number,
    };
    let _ = client.create_app_record(&request)?;
    wait_for_session_app_record(&client, &receipt.bundle_id)
}

fn wait_for_api_key_app_record(client: &AscClient, bundle_id_id: &str) -> Result<()> {
    let deadline = Instant::now() + APP_RECORD_VISIBILITY_TIMEOUT;
    while Instant::now() < deadline {
        if client.find_app_by_bundle_id(bundle_id_id)?.is_some() {
            return Ok(());
        }
        thread::sleep(APP_RECORD_VISIBILITY_POLL_INTERVAL);
    }
    bail!(
        "App Store Connect app record did not become visible within {} seconds",
        APP_RECORD_VISIBILITY_TIMEOUT.as_secs()
    )
}

fn wait_for_session_app_record(client: &AscSessionAppsClient, bundle_id: &str) -> Result<()> {
    let deadline = Instant::now() + APP_RECORD_VISIBILITY_TIMEOUT;
    while Instant::now() < deadline {
        if client.find_app_by_bundle_id(bundle_id)?.is_some() {
            return Ok(());
        }
        thread::sleep(APP_RECORD_VISIBILITY_POLL_INTERVAL);
    }
    bail!(
        "App Store Connect app record did not become visible within {} seconds",
        APP_RECORD_VISIBILITY_TIMEOUT.as_secs()
    )
}

fn bundle_short_version(receipt: &BuildReceipt) -> Result<String> {
    let info_path = receipt.bundle_path.join("Info.plist");
    let plist = PlistValue::from_file(&info_path)
        .with_context(|| format!("failed to read {}", info_path.display()))?;
    let dict = plist
        .into_dictionary()
        .context("bundle Info.plist is not a dictionary")?;
    dict.get("CFBundleShortVersionString")
        .and_then(PlistValue::as_string)
        .map(ToOwned::to_owned)
        .context("bundle Info.plist is missing CFBundleShortVersionString")
}

fn app_store_sku(bundle_id: &str) -> String {
    let mut sku = bundle_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    sku.truncate(255);
    sku
}
