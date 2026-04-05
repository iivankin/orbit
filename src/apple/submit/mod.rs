mod app_record;
mod auth_flow;
mod content_delivery;
mod endpoints;
mod label_service;
mod notary;
mod package;

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::apple::build::receipt::{BuildReceipt, list_receipts, load_receipt};
use crate::apple::runtime::distribution_from_cli;
use crate::cli::SubmitArgs;
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest};
use crate::util::{CliSpinner, ensure_dir, format_elapsed, prompt_select};

use self::app_record::ensure_submit_app_record;
use self::auth_flow::establish_submit_auth;
use self::content_delivery::{BuildDeliveryFileDocument, ContentDeliveryClient};
use self::label_service::LabelServiceClient;
use self::notary::submit_with_xcode_notary;
use self::package::{AssetType, prepare_upload, software_type_for_receipt};

pub fn submit_artifact(project: &ProjectContext, args: &SubmitArgs) -> Result<()> {
    let receipt = resolve_submit_receipt(project, args)?;

    match receipt.platform {
        ApplePlatform::Ios | ApplePlatform::Tvos | ApplePlatform::Visionos => {
            crate::apple::auth::ensure_project_authenticated(project)?;
            submit_with_content_delivery(project, &receipt, args.wait)
        }
        ApplePlatform::Watchos => bail!("watchOS submit is not implemented yet"),
        ApplePlatform::Macos => match receipt.distribution {
            DistributionKind::DeveloperId => submit_with_xcode_notary(project, &receipt, args.wait),
            DistributionKind::MacAppStore => {
                crate::apple::auth::ensure_project_authenticated(project)?;
                bail!("macOS App Store submit via content delivery is not implemented yet")
            }
            other => bail!("macOS submit is not supported for {:?} builds", other),
        },
    }
}

fn submit_with_content_delivery(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    wait: bool,
) -> Result<()> {
    let (lookup_auth, upload_auth) = submit_progress_step(
        "Submit: Refreshing App Store Connect upload auth".to_owned(),
        |_| "Submit: Refreshed App Store Connect upload auth.".to_owned(),
        || establish_submit_auth(project),
    )?;
    submit_progress_step(
        format!(
            "Submit: Ensuring App Store Connect app record for {}",
            receipt.bundle_id
        ),
        |_| {
            format!(
                "Submit: App Store Connect app record ready for `{}`.",
                receipt.bundle_id
            )
        },
        || {
            ensure_submit_app_record(
                project,
                receipt,
                Some(upload_auth.provider_public_id.as_str()),
            )
        },
    )?;
    let app_lookup = submit_progress_step(
        format!(
            "Submit: Looking up `{}` in App Store Connect",
            receipt.bundle_id
        ),
        |_| {
            format!(
                "Submit: Found App Store Connect app record for `{}`.",
                receipt.bundle_id
            )
        },
        || {
            let label_service = LabelServiceClient::from_headers(lookup_auth.headers().clone())?;
            label_service.lookup_software_for_bundle_id(
                &receipt.bundle_id,
                software_type_for_receipt(receipt)?,
            )
        },
    )?;
    let provider_public_id = upload_auth.provider_public_id.clone();
    let mut client = submit_progress_step(
        "Submit: Connecting to Content Delivery".to_owned(),
        |_| "Submit: Connected to Content Delivery.".to_owned(),
        || ContentDeliveryClient::from_live_auth(&upload_auth),
    )?;

    let submit_workspace = project
        .project_paths
        .orbit_dir
        .join("submit")
        .join(&receipt.id);
    ensure_dir(&submit_workspace)?;
    let upload = submit_progress_step(
        format!(
            "Submit: Preparing upload package for {}",
            receipt.artifact_path.display()
        ),
        |upload: &package::PreparedUpload| {
            format!("Submit: Prepared {} upload asset(s).", upload.assets.len())
        },
        || prepare_upload(receipt, &provider_public_id, &submit_workspace),
    )?;
    let build_id = submit_progress_step(
        "Submit: Creating App Store Connect build record".to_owned(),
        |build_id| format!("Submit: Created build `{build_id}`."),
        || client.create_build(&app_lookup.app_id, &upload),
    )?;

    for asset in &upload.assets {
        let build_file = submit_progress_step(
            format!(
                "Submit: Creating delivery file for {}",
                asset.asset_type.as_str()
            ),
            |_| {
                format!(
                    "Submit: Created delivery file for {}.",
                    asset.asset_type.as_str()
                )
            },
            || client.create_build_file(&build_id, asset),
        )?;
        upload_asset(&mut client, &build_file, asset)?;
    }

    let build = client.get_build(&build_id)?;
    if wait {
        wait_for_processing(&mut client, &build_id)?;
    } else if let Some(errors) = build.data.attributes.processing_errors
        && !errors.is_empty()
    {
        bail!("Apple returned processing errors immediately after upload");
    }
    if let Err(error) = client.send_metrics(&upload, &build_id) {
        eprintln!("warning: failed to send metricsAndLogging: {error:#}");
    }
    Ok(())
}

fn submit_progress_step<T, F, G>(
    message: impl Into<String>,
    success_message: G,
    action: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
    G: FnOnce(&T) -> String,
{
    let spinner = CliSpinner::new(message.into());
    match action() {
        Ok(value) => {
            spinner.finish_success(success_message(&value));
            Ok(value)
        }
        Err(error) => {
            spinner.finish_clear();
            Err(error)
        }
    }
}

fn upload_asset(
    client: &mut ContentDeliveryClient,
    build_file: &BuildDeliveryFileDocument,
    asset: &package::PreparedAsset,
) -> Result<()> {
    let upload_spinner = CliSpinner::new(format!(
        "Submit: Uploading {} ({})",
        asset.asset_type.as_str(),
        asset.file_name
    ));
    let operation = build_file
        .data
        .attributes
        .upload_operations
        .as_ref()
        .and_then(|operations| operations.first())
        .context("buildDeliveryFile did not include an upload operation")?;
    client.upload_delivery_file(operation, &asset.path)?;
    let uploaded = client.mark_build_file_uploaded(&build_file.data.id)?;
    ensure_delivery_file_complete(client, uploaded, asset.asset_type, upload_spinner)
}

fn ensure_delivery_file_complete(
    client: &mut ContentDeliveryClient,
    mut current: BuildDeliveryFileDocument,
    asset_type: AssetType,
    spinner: CliSpinner,
) -> Result<()> {
    let started_at = Instant::now();
    if current.data.attributes.asset_delivery_state.state == "COMPLETE" {
        spinner.finish_success(format!(
            "Submit: Uploaded {} in {}.",
            asset_type.as_str(),
            format_elapsed(started_at.elapsed())
        ));
        return Ok(());
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        thread::sleep(Duration::from_secs(1));
        current = client.get_build_file(&current.data.id)?;
        let state = &current.data.attributes.asset_delivery_state.state;
        spinner.set_message(format!(
            "Submit: Waiting for {} upload ({state}, elapsed {})",
            asset_type.as_str(),
            format_elapsed(started_at.elapsed())
        ));
        if state == "COMPLETE" {
            spinner.finish_success(format!(
                "Submit: Uploaded {} in {}.",
                asset_type.as_str(),
                format_elapsed(started_at.elapsed())
            ));
            return Ok(());
        }
        if state == "FAILED" {
            spinner.finish_clear();
            bail!(
                "{} upload failed: {}",
                asset_type.as_str(),
                delivery_error_summary(&current.data.attributes.asset_delivery_state)
            );
        }
    }

    spinner.finish_clear();
    bail!(
        "{} upload did not reach COMPLETE within 30 seconds",
        asset_type.as_str()
    )
}

fn wait_for_processing(client: &mut ContentDeliveryClient, build_id: &str) -> Result<()> {
    let spinner = CliSpinner::new("Submit: Waiting for Apple build processing");
    let started_at = Instant::now();
    let deadline = started_at + Duration::from_secs(300);
    while Instant::now() < deadline {
        let build = client.get_build(build_id)?;
        let attributes = build.data.attributes;
        if let Some(processing_state) = build_processing_state(&attributes) {
            spinner.set_message(format!(
                "Submit: Waiting for Apple build processing ({processing_state}, elapsed {})",
                format_elapsed(started_at.elapsed())
            ));
            if processing_state.eq_ignore_ascii_case("failed")
                || processing_state.eq_ignore_ascii_case("invalid")
            {
                spinner.finish_clear();
                bail!(
                    "Apple build processing failed: {}",
                    processing_error_summary(
                        attributes.processing_errors.as_deref(),
                        attributes
                            .build_processing_state
                            .as_ref()
                            .map(|state| state.errors.as_slice()),
                    )
                );
            }
            if !processing_state.eq_ignore_ascii_case("processing") {
                spinner.finish_success(format!(
                    "Submit: Apple build processing finished with `{processing_state}` in {}.",
                    format_elapsed(started_at.elapsed())
                ));
                return Ok(());
            }
        }
        thread::sleep(Duration::from_secs(5));
    }

    spinner.finish_clear();
    bail!("Apple did not report build processing state within 5 minutes")
}

fn delivery_error_summary(state: &content_delivery::AssetDeliveryState) -> String {
    state
        .errors
        .iter()
        .chain(state.warnings.iter())
        .filter_map(|entry| entry.detail.clone().or_else(|| entry.title.clone()))
        .collect::<Vec<_>>()
        .join("; ")
}

fn build_processing_state(attributes: &content_delivery::BuildAttributes) -> Option<&str> {
    attributes
        .build_processing_state
        .as_ref()
        .map(|state| state.state.as_str())
        .or(attributes.processing_state.as_deref())
}

fn processing_error_summary(
    errors: Option<&[content_delivery::ProcessingIssue]>,
    nested_errors: Option<&[content_delivery::ProcessingIssue]>,
) -> String {
    errors
        .unwrap_or(&[])
        .iter()
        .chain(nested_errors.unwrap_or(&[]).iter())
        .filter_map(|entry| entry.detail.clone().or_else(|| entry.title.clone()))
        .collect::<Vec<_>>()
        .join("; ")
}

fn resolve_submit_receipt(project: &ProjectContext, args: &SubmitArgs) -> Result<BuildReceipt> {
    let requested_platform = args
        .platform
        .map(crate::apple::runtime::apple_platform_from_cli);
    let requested_distribution = distribution_from_cli(args.distribution);

    if let Some(receipt_path) = &args.receipt {
        let receipt = load_receipt(receipt_path)?;
        if !receipt.submit_eligible {
            bail!(
                "receipt `{}` is not submit-eligible because it was built for `{:?}` distribution",
                receipt.id,
                receipt.distribution
            );
        }
        if requested_platform.is_some_and(|platform| receipt.platform != platform) {
            bail!(
                "receipt `{}` targets platform `{}`, not the requested `{}`",
                receipt.id,
                receipt.platform,
                requested_platform
                    .map(|platform| platform.to_string())
                    .unwrap_or_default()
            );
        }
        if requested_distribution.is_some_and(|distribution| receipt.distribution != distribution) {
            bail!(
                "receipt `{}` uses distribution `{}`, not the requested `{}`",
                receipt.id,
                receipt.distribution.as_str(),
                requested_distribution
                    .map(DistributionKind::as_str)
                    .unwrap_or_default()
            );
        }
        return Ok(receipt);
    }

    let mut receipts = list_receipts(
        &project.project_paths.receipts_dir,
        requested_platform,
        requested_distribution,
    )?;
    receipts.retain(|receipt| receipt.submit_eligible);
    receipts.sort_by(|left, right| right.created_at_unix.cmp(&left.created_at_unix));
    if receipts.is_empty() {
        bail!("could not find a submit-eligible build receipt");
    }
    if receipts.len() == 1 || !project.app.interactive {
        return Ok(receipts.remove(0));
    }

    let labels = receipts.iter().map(receipt_label).collect::<Vec<_>>();
    let index = prompt_select("Select a build receipt to submit", &labels)?;
    Ok(receipts.remove(index))
}

fn receipt_label(receipt: &BuildReceipt) -> String {
    format!(
        "{} | {} | {} | {} | {}",
        receipt.id,
        receipt.target,
        profile_description(&ProfileManifest::new(
            receipt.configuration,
            receipt.distribution
        )),
        receipt.destination,
        receipt.artifact_path.display()
    )
}

fn profile_description(profile: &ProfileManifest) -> String {
    format!(
        "{} {}",
        profile.distribution.as_str(),
        profile.configuration.as_str()
    )
}
