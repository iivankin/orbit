mod naming;
mod plan;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asc_sync::{
    auth_store,
    config::{Config as AscConfig, DeviceFamily},
    device::{DeviceAddLocalRequest, DeviceAddRequest, add_local_with_config, add_with_config},
    device_discovery::{DetectedDevice, discover_local_devices},
};

use crate::apple;
use crate::context::AppContext;
use crate::util::{print_success, prompt_input, prompt_select, resolve_path};

use self::naming::{bundle_id_suffix, looks_like_bundle_id, suggested_product_name};
use self::plan::{
    InitAnswers, InitAscDevice, InitDeviceSlot, InitEcosystem, InitTemplate, TemplateChoice,
    create_scaffold, scaffold_plan,
};

const ECOSYSTEM_CHOICES: [EcosystemChoice; 1] = [EcosystemChoice {
    kind: InitEcosystem::Apple,
    label: "Apple",
    description: "iOS, macOS, tvOS, watchOS, and visionOS apps",
}];
const ADD_NEW_ASC_AUTH_LABEL: &str = "Add new App Store Connect authorization...";
const ADD_DEVICE_WITH_REGISTRATION_LABEL: &str = "Add a device with registration flow...";
const ENTER_DEVICE_MANUALLY_LABEL: &str = "Enter device details manually...";
const INIT_DEVICE_REGISTRATION_TIMEOUT_SECONDS: u64 = 300;

#[derive(Debug, Clone, Copy)]
struct EcosystemChoice {
    kind: InitEcosystem,
    label: &'static str,
    description: &'static str,
}

pub fn execute(app: &AppContext, requested_manifest: Option<&Path>) -> Result<()> {
    if !app.interactive {
        bail!("`orbi init` requires an interactive terminal");
    }

    let manifest_path = init_manifest_path(app, requested_manifest);
    if manifest_path.exists() {
        bail!("manifest already exists at {}", manifest_path.display());
    }

    let project_root = manifest_path
        .parent()
        .context("manifest path did not contain a parent directory")?;
    let answers = collect_init_answers(app, project_root)?;
    let schema_reference = published_schema_reference(answers.ecosystem);
    let plan = scaffold_plan(&answers, &schema_reference);

    create_scaffold(project_root, &manifest_path, &plan)?;
    print_success(format!("Created {}", manifest_path.display()));
    let bsp_path = apple::bsp::install_connection_file_for_manifest(&manifest_path)?;
    print_success(format!("Installed {}", bsp_path.display()));

    println!("Next commands:");
    for command in &plan.next_commands {
        println!("  {command}");
    }

    Ok(())
}

fn init_manifest_path(app: &AppContext, requested_manifest: Option<&Path>) -> PathBuf {
    requested_manifest.map_or_else(
        || app.cwd.join("orbi.json"),
        |path| resolve_path(&app.cwd, path),
    )
}

fn collect_init_answers(_app: &AppContext, project_root: &Path) -> Result<InitAnswers> {
    let ecosystem = prompt_ecosystem()?;
    let default_name = suggested_product_name(project_root);
    let name = prompt_non_empty("Product name", Some(default_name.as_str()))?;
    let default_bundle_id = format!("dev.orbi.{}", bundle_id_suffix(&name));
    let bundle_id = prompt_validated(
        "Bundle ID",
        Some(default_bundle_id.as_str()),
        looks_like_bundle_id,
        "Enter a reverse-DNS bundle ID like `dev.orbi.exampleapp`.",
    )?;
    let template = prompt_template(ecosystem)?;
    let asc_team_id = prompt_asc_team_id()?;
    let asc_devices = prompt_asc_devices(template, &asc_team_id)?;

    Ok(InitAnswers {
        ecosystem,
        name,
        bundle_id,
        template,
        asc_team_id,
        asc_devices,
    })
}

fn prompt_ecosystem() -> Result<InitEcosystem> {
    let labels = ECOSYSTEM_CHOICES
        .iter()
        .map(|choice| format!("{}: {}", choice.label, choice.description))
        .collect::<Vec<_>>();
    let index = prompt_select("Ecosystem", &labels)?;
    Ok(ECOSYSTEM_CHOICES[index].kind)
}

fn prompt_template(ecosystem: InitEcosystem) -> Result<InitTemplate> {
    let choices = ecosystem.template_choices();
    let labels = choices
        .iter()
        .map(render_template_label)
        .collect::<Vec<_>>();
    let index = prompt_select("Template", &labels)?;
    Ok(choices[index].kind)
}

fn render_template_label(choice: &TemplateChoice) -> String {
    format!("{}: {}", choice.label, choice.description)
}

fn prompt_asc_team_id() -> Result<String> {
    let team_ids = auth_store::stored_team_ids()?;
    if team_ids.is_empty() {
        return auth_store::import_auth_interactively_with_team_id();
    }

    let mut labels = team_ids
        .iter()
        .map(|team_id| format!("{team_id}: stored App Store Connect authorization"))
        .collect::<Vec<_>>();
    labels.push(ADD_NEW_ASC_AUTH_LABEL.to_owned());

    let index = prompt_select("App Store Connect team", &labels)?;
    if index == team_ids.len() {
        return auth_store::import_auth_interactively_with_team_id();
    }

    Ok(team_ids[index].clone())
}

fn prompt_asc_devices(template: InitTemplate, asc_team_id: &str) -> Result<Vec<InitAscDevice>> {
    template
        .required_device_slots()
        .iter()
        .map(|slot| prompt_asc_device(*slot, asc_team_id))
        .collect()
}

fn prompt_asc_device(slot: InitDeviceSlot, asc_team_id: &str) -> Result<InitAscDevice> {
    loop {
        let discovered = discover_compatible_local_devices(slot);
        let mut labels = discovered
            .iter()
            .map(render_detected_device_label)
            .collect::<Vec<_>>();
        let registration_index = if slot.allow_registration {
            let index = labels.len();
            labels.push(ADD_DEVICE_WITH_REGISTRATION_LABEL.to_owned());
            Some(index)
        } else {
            None
        };
        let manual_index = labels.len();
        labels.push(ENTER_DEVICE_MANUALLY_LABEL.to_owned());

        let index = prompt_select(slot.prompt, &labels)?;
        if index < discovered.len() {
            return Ok(init_device_from_detected(
                slot.logical_id,
                &discovered[index],
            ));
        }
        if Some(index) == registration_index {
            let device = prompt_registered_device(slot, asc_team_id)?;
            if slot.supports_family(device.family) {
                return Ok(device);
            }
            println!(
                "That device reports family `{}` but {} expects {}.",
                device.family,
                slot.prompt,
                compatible_family_summary(slot),
            );
            continue;
        }
        if index == manual_index {
            let device = prompt_manual_device(slot, asc_team_id)?;
            if slot.supports_family(device.family) {
                return Ok(device);
            }
            println!(
                "That device family `{}` is not compatible with {}. Expected {}.",
                device.family,
                slot.prompt,
                compatible_family_summary(slot),
            );
        }
    }
}

fn discover_compatible_local_devices(slot: InitDeviceSlot) -> Vec<DetectedDevice> {
    let mut discovered = discover_local_devices()
        .unwrap_or_default()
        .into_iter()
        .filter(|device| slot.supports_family(device.family))
        .collect::<Vec<_>>();
    discovered.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.family.to_string().cmp(&right.family.to_string()))
            .then_with(|| left.udid.cmp(&right.udid))
    });
    discovered
}

fn render_detected_device_label(device: &DetectedDevice) -> String {
    format!("{} [{}] {}", device.name, device.family, device.udid)
}

fn compatible_family_summary(slot: InitDeviceSlot) -> String {
    slot.compatible_families
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

fn prompt_registered_device(slot: InitDeviceSlot, asc_team_id: &str) -> Result<InitAscDevice> {
    let name = prompt_non_empty("Device name", Some(slot.default_name))?;
    let registered = add_with_config(
        &init_asc_config(asc_team_id),
        None,
        &DeviceAddRequest {
            name,
            logical_id: Some(slot.logical_id.to_owned()),
            family: None,
            apply: false,
            timeout_seconds: INIT_DEVICE_REGISTRATION_TIMEOUT_SECONDS,
        },
    )?;
    Ok(init_device_from_registered(slot.logical_id, registered))
}

fn prompt_manual_device(slot: InitDeviceSlot, asc_team_id: &str) -> Result<InitAscDevice> {
    let udid = prompt_non_empty("Device UDID", None)?;
    let discovered = discover_local_device_by_udid(&udid);
    let default_name = discovered
        .as_ref()
        .map(|device| device.name.as_str())
        .unwrap_or(slot.default_name);
    let name = prompt_non_empty("Device name", Some(default_name))?;
    let family = if discovered.is_some() {
        None
    } else {
        Some(prompt_device_family(slot)?)
    };

    let registered = add_local_with_config(
        &init_asc_config(asc_team_id),
        None,
        &DeviceAddLocalRequest {
            name: Some(name),
            logical_id: Some(slot.logical_id.to_owned()),
            current_mac: false,
            family,
            udid: Some(udid),
            apply: false,
        },
    )?;
    Ok(init_device_from_registered(slot.logical_id, registered))
}

fn discover_local_device_by_udid(udid: &str) -> Option<DetectedDevice> {
    discover_local_devices()
        .ok()?
        .into_iter()
        .find(|device| device.udid == udid)
}

fn prompt_device_family(slot: InitDeviceSlot) -> Result<DeviceFamily> {
    if let [family] = slot.compatible_families {
        return Ok(*family);
    }

    let labels = slot
        .compatible_families
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let index = prompt_select("Device family", &labels)?;
    Ok(slot.compatible_families[index])
}

fn init_asc_config(team_id: &str) -> AscConfig {
    // `orbi init` can reuse asc-sync's interactive device flows before `orbi.json` exists.
    AscConfig {
        schema: None,
        description: None,
        team_id: team_id.to_owned(),
        bundle_ids: Default::default(),
        devices: Default::default(),
        certs: Default::default(),
        profiles: Default::default(),
        apps: Default::default(),
    }
}

fn init_device_from_detected(logical_id: &'static str, device: &DetectedDevice) -> InitAscDevice {
    InitAscDevice {
        logical_id,
        family: device.family,
        udid: device.udid.clone(),
        name: device.name.clone(),
    }
}

fn init_device_from_registered(
    logical_id: &'static str,
    device: asc_sync::device::RegisteredDevice,
) -> InitAscDevice {
    InitAscDevice {
        logical_id,
        family: device.family,
        udid: device.udid,
        name: device.display_name,
    }
}

fn published_schema_reference(ecosystem: InitEcosystem) -> String {
    ecosystem.manifest_schema().as_str().to_owned()
}

fn prompt_non_empty(prompt: &str, default: Option<&str>) -> Result<String> {
    prompt_validated(
        prompt,
        default,
        |value| !value.is_empty(),
        "Value cannot be empty.",
    )
}

fn prompt_validated(
    prompt: &str,
    default: Option<&str>,
    validator: impl Fn(&str) -> bool,
    error_message: &str,
) -> Result<String> {
    loop {
        let value = prompt_input(prompt, default)?;
        let value = value.trim();
        if validator(value) {
            return Ok(value.to_owned());
        }
        println!("{error_message}");
    }
}
