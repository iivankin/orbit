use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::apple::auth::{EnsureUserAuthRequest, ensure_portal_authenticated};
use crate::apple::portal::{PortalClient, PortalDevice, PortalDeviceClass};
use crate::cli::{
    DevicePlatform, ImportDevicesArgs, ListDevicesArgs, RegisterDeviceArgs, RemoveDeviceArgs,
};
use crate::context::{AppContext, DeviceCache};
use crate::util::{prompt_input, prompt_select};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDevice {
    pub id: String,
    pub name: String,
    pub udid: String,
    pub platform: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ImportedDevice {
    name: String,
    udid: String,
    platform: String,
}

pub fn list_devices(app: &AppContext, args: &ListDevicesArgs) -> Result<()> {
    let cache = load_cached_or_remote_devices(app, args.refresh)?;
    if cache.devices.is_empty() {
        println!("no devices registered");
        return Ok(());
    }

    for device in cache.devices {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            device.id, device.platform, device.udid, device.status, device.name
        );
    }
    Ok(())
}

pub fn register_device(app: &AppContext, args: &RegisterDeviceArgs) -> Result<()> {
    let mut client = portal_client(app)?;
    let imported = if args.current_machine {
        current_machine_device(args.platform)?
    } else {
        ImportedDevice {
            name: match &args.name {
                Some(name) => name.clone(),
                None if app.interactive => prompt_input("Device name", None)?,
                None => bail!("--name is required in non-interactive mode"),
            },
            udid: match &args.udid {
                Some(udid) => udid.clone(),
                None if app.interactive => prompt_input("Device UDID", None)?,
                None => bail!("--udid is required in non-interactive mode"),
            },
            platform: platform_value(args.platform).to_owned(),
        }
    };
    let device_class = platform_device_class(args.platform);

    if let Some(existing) = client.find_device_by_udid(&imported.udid, device_class)? {
        println!(
            "reused\t{}\t{}\t{}\t{}",
            existing.id,
            device_platform_label(&existing),
            existing.udid,
            existing.name
        );
    } else {
        let created = client.create_device(&imported.name, &imported.udid, device_class)?;
        println!(
            "created\t{}\t{}\t{}\t{}",
            created.id,
            device_platform_label(&created),
            created.udid,
            created.name
        );
    }

    let _ = refresh_cache(app)?;
    Ok(())
}

pub fn import_devices(app: &AppContext, args: &ImportDevicesArgs) -> Result<()> {
    let mut client = portal_client(app)?;
    let file = match &args.file {
        Some(file) => file.clone(),
        None if app.interactive => {
            PathBuf::from(prompt_input("Path to JSON or CSV device list", None)?)
        }
        None => bail!("--file is required in non-interactive mode"),
    };

    let mut created_count = 0usize;
    let devices = load_import_file(&file)?;
    for device in devices {
        let device_class = imported_platform_device_class(&device.platform)?;
        if client
            .find_device_by_udid(&device.udid, device_class)?
            .is_none()
        {
            let created = client.create_device(&device.name, &device.udid, device_class)?;
            println!(
                "created\t{}\t{}\t{}\t{}",
                created.id,
                device_platform_label(&created),
                created.udid,
                created.name
            );
            created_count += 1;
        }
    }

    let _ = refresh_cache(app)?;
    if created_count == 0 {
        println!("no new devices were imported");
    }
    Ok(())
}

pub fn remove_device(app: &AppContext, args: &RemoveDeviceArgs) -> Result<()> {
    let mut client = portal_client(app)?;
    let target = if let Some(id) = &args.id {
        find_registered_device_by_id(&mut client, id)?
            .with_context(|| format!("no Apple device found for id `{id}`"))?
    } else if let Some(udid) = &args.udid {
        find_registered_device_by_udid(&mut client, udid)?
            .with_context(|| format!("no Apple device found for UDID `{udid}`"))?
    } else if app.interactive {
        let cache = refresh_cache(app)?;
        if cache.devices.is_empty() {
            bail!("no registered Apple devices found");
        }
        let labels = cache
            .devices
            .iter()
            .map(|device| format!("{} [{}] {}", device.name, device.platform, device.udid))
            .collect::<Vec<_>>();
        let index = prompt_select("Select a device to remove", &labels)?;
        find_registered_device_by_id(&mut client, &cache.devices[index].id)?
            .context("selected Apple device no longer exists")?
    } else {
        bail!("pass --id or --udid");
    };

    client.delete_device(
        &target.id,
        device_class_for_cached_platform(&target.platform),
    )?;
    println!("removed\t{}", target.id);
    let _ = refresh_cache(app)?;
    Ok(())
}

pub fn refresh_cache(app: &AppContext) -> Result<DeviceCache> {
    let mut client = portal_client(app)?;
    let mut devices = Vec::new();
    for class in [
        PortalDeviceClass::Iphone,
        PortalDeviceClass::Tvos,
        PortalDeviceClass::Watch,
        PortalDeviceClass::Mac,
    ] {
        devices.extend(
            client
                .list_devices(class, true)?
                .into_iter()
                .map(cached_device_from_portal),
        );
    }
    let mut devices = devices;
    devices.sort_by(|left, right| {
        left.platform
            .cmp(&right.platform)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.udid.cmp(&right.udid))
    });
    let cache = DeviceCache { devices };
    app.write_device_cache(&cache)?;
    Ok(cache)
}

fn load_import_file(path: &Path) -> Result<Vec<ImportedDevice>> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if path.extension().and_then(|value| value.to_str()) == Some("json") {
        if let Ok(items) = serde_json::from_str::<Vec<ImportedDevice>>(&contents) {
            return Ok(items);
        }
    }

    let mut items = Vec::new();
    let mut seen_udids = std::collections::HashSet::new();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts = trimmed.split(',').map(str::trim).collect::<Vec<_>>();
        if parts.len() != 3 {
            bail!(
                "invalid device import line {} in {}; expected `udid,name,platform`",
                index + 1,
                path.display()
            );
        }
        if index == 0
            && parts[0].eq_ignore_ascii_case("udid")
            && parts[1].eq_ignore_ascii_case("name")
            && parts[2].eq_ignore_ascii_case("platform")
        {
            continue;
        }
        let device = ImportedDevice {
            udid: parts[0].to_owned(),
            name: parts[1].to_owned(),
            platform: parts[2].to_owned(),
        };
        if seen_udids.insert(device.udid.clone()) {
            items.push(device);
        }
    }
    Ok(items)
}

fn current_machine_device(platform: DevicePlatform) -> Result<ImportedDevice> {
    if matches!(platform, DevicePlatform::Ios) {
        bail!("`--current-machine` requires `--platform macos` or `--platform universal`");
    }

    let output = crate::util::command_output(
        std::process::Command::new("system_profiler").args(["-json", "SPHardwareDataType"]),
    )?;
    let value: Value = serde_json::from_str(&output)?;
    let entry = value
        .get("SPHardwareDataType")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .context("system_profiler did not return hardware information")?;
    let udid = entry
        .get("provisioning_UDID")
        .and_then(Value::as_str)
        .context("current machine does not expose provisioning_UDID")?;
    let name = entry
        .get("_name")
        .and_then(Value::as_str)
        .unwrap_or("Current Mac");
    Ok(ImportedDevice {
        name: name.to_owned(),
        udid: udid.to_owned(),
        platform: platform_value(platform).to_owned(),
    })
}

fn portal_client(app: &AppContext) -> Result<PortalClient> {
    let auth = ensure_portal_authenticated(
        app,
        EnsureUserAuthRequest {
            prompt_for_missing: app.interactive,
            ..Default::default()
        },
    )?;
    let team_id = auth.user.team_id.clone().context(
        "device management requires an Apple Developer team selection; log in again and choose a team if prompted",
    )?;
    PortalClient::from_session(&auth.session, team_id)
}

fn load_cached_or_remote_devices(app: &AppContext, refresh: bool) -> Result<DeviceCache> {
    if refresh {
        return refresh_cache(app);
    }

    let cache = app.read_device_cache()?;
    if cache.devices.is_empty() {
        refresh_cache(app)
    } else {
        Ok(cache)
    }
}

fn cached_device_from_portal(device: PortalDevice) -> CachedDevice {
    let platform = device_platform_label(&device);
    CachedDevice {
        id: device.id,
        name: device.name,
        udid: device.udid,
        platform,
        status: device.status.unwrap_or_else(|| "UNKNOWN".to_owned()),
    }
}

fn platform_value(platform: DevicePlatform) -> &'static str {
    match platform {
        DevicePlatform::Ios => "IOS",
        DevicePlatform::MacOs => "MAC_OS",
        DevicePlatform::Universal => "UNIVERSAL",
    }
}

fn platform_device_class(platform: DevicePlatform) -> PortalDeviceClass {
    match platform {
        DevicePlatform::Ios => PortalDeviceClass::Iphone,
        DevicePlatform::MacOs | DevicePlatform::Universal => PortalDeviceClass::Mac,
    }
}

fn imported_platform_device_class(platform: &str) -> Result<PortalDeviceClass> {
    match platform {
        "IOS" => Ok(PortalDeviceClass::Iphone),
        "MAC_OS" | "UNIVERSAL" => Ok(PortalDeviceClass::Mac),
        "TVOS" => Ok(PortalDeviceClass::Tvos),
        "WATCH" | "WATCHOS" => Ok(PortalDeviceClass::Watch),
        other => bail!("unsupported imported device platform `{other}`"),
    }
}

fn device_platform_label(device: &PortalDevice) -> String {
    if let Some(platform) = &device.platform {
        return platform.clone();
    }
    match device.device_class.as_deref() {
        Some("mac") => "MAC_OS".to_owned(),
        Some("tvOS") => "TVOS".to_owned(),
        Some("watch") => "WATCH".to_owned(),
        _ => "IOS".to_owned(),
    }
}

fn device_class_for_cached_platform(platform: &str) -> PortalDeviceClass {
    match platform {
        "MAC_OS" | "UNIVERSAL" => PortalDeviceClass::Mac,
        "TVOS" => PortalDeviceClass::Tvos,
        "WATCH" | "WATCHOS" => PortalDeviceClass::Watch,
        _ => PortalDeviceClass::Iphone,
    }
}

fn find_registered_device_by_udid(
    client: &mut PortalClient,
    udid: &str,
) -> Result<Option<CachedDevice>> {
    for class in [
        PortalDeviceClass::Iphone,
        PortalDeviceClass::Tvos,
        PortalDeviceClass::Watch,
        PortalDeviceClass::Mac,
    ] {
        if let Some(device) = client.find_device_by_udid(udid, class)? {
            return Ok(Some(cached_device_from_portal(device)));
        }
    }
    Ok(None)
}

fn find_registered_device_by_id(
    client: &mut PortalClient,
    id: &str,
) -> Result<Option<CachedDevice>> {
    for class in [
        PortalDeviceClass::Iphone,
        PortalDeviceClass::Tvos,
        PortalDeviceClass::Watch,
        PortalDeviceClass::Mac,
    ] {
        let devices = client.list_devices(class, true)?;
        if let Some(device) = devices.into_iter().find(|device| device.id == id) {
            return Ok(Some(cached_device_from_portal(device)));
        }
    }
    Ok(None)
}
