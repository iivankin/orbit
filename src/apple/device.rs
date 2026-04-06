use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::apple::asc_api::AscClient;
use crate::apple::auth::resolve_api_key_auth;
use crate::apple::provisioning::{ProvisioningClient, ProvisioningDevice};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ImportedDevice {
    name: String,
    udid: String,
    platform: String,
}

enum DeviceClient {
    ApiKey(AscClient),
    AppleId(Box<ProvisioningClient>),
}

impl DeviceClient {
    fn connect(app: &AppContext) -> Result<Self> {
        if let Some(api_key) = resolve_api_key_auth(app)? {
            return Ok(Self::ApiKey(AscClient::new(api_key)?));
        }

        Ok(Self::AppleId(Box::new(ProvisioningClient::authenticate(
            app,
            resolve_team_id(app)?,
        )?)))
    }

    fn list_devices(&mut self) -> Result<Vec<CachedDevice>> {
        match self {
            Self::ApiKey(client) => Ok(client
                .list_devices()?
                .into_iter()
                .map(|device| CachedDevice {
                    id: device.id,
                    name: device.attributes.name,
                    udid: device.attributes.udid,
                    platform: normalize_device_platform(&device.attributes.platform),
                    status: device
                        .attributes
                        .status
                        .unwrap_or_else(|| "UNKNOWN".to_owned()),
                    device_class: device.attributes.device_class,
                    model: device.attributes.model,
                    created_at: device.attributes.added_date,
                })
                .collect()),
            Self::AppleId(client) => Ok(client
                .list_devices()?
                .into_iter()
                .map(cached_device_from_provisioning)
                .collect()),
        }
    }

    fn find_device_by_udid(&mut self, udid: &str) -> Result<Option<CachedDevice>> {
        match self {
            Self::ApiKey(client) => {
                Ok(client
                    .find_device_by_udid(udid)?
                    .map(|device| CachedDevice {
                        id: device.id,
                        name: device.attributes.name,
                        udid: device.attributes.udid,
                        platform: normalize_device_platform(&device.attributes.platform),
                        status: device
                            .attributes
                            .status
                            .unwrap_or_else(|| "UNKNOWN".to_owned()),
                        device_class: device.attributes.device_class,
                        model: device.attributes.model,
                        created_at: device.attributes.added_date,
                    }))
            }
            Self::AppleId(client) => Ok(client
                .find_device_by_udid(udid)?
                .map(cached_device_from_provisioning)),
        }
    }

    fn create_device(
        &mut self,
        name: &str,
        udid: &str,
        platform: DevicePlatform,
    ) -> Result<CachedDevice> {
        let platform = create_device_platform(platform);
        match self {
            Self::ApiKey(client) => {
                let device = client.create_device(name, udid, platform)?;
                Ok(CachedDevice {
                    id: device.id,
                    name: device.attributes.name,
                    udid: device.attributes.udid,
                    platform: normalize_device_platform(&device.attributes.platform),
                    status: device
                        .attributes
                        .status
                        .unwrap_or_else(|| "ENABLED".to_owned()),
                    device_class: device.attributes.device_class,
                    model: device.attributes.model,
                    created_at: device.attributes.added_date,
                })
            }
            Self::AppleId(client) => Ok(cached_device_from_provisioning(
                client.create_device(name, udid, platform)?,
            )),
        }
    }

    fn delete_device(&mut self, id: &str) -> Result<()> {
        match self {
            Self::ApiKey(client) => client.delete_device(id),
            Self::AppleId(client) => client.delete_device(id),
        }
    }
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
    let mut client = DeviceClient::connect(app)?;
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

    if let Some(existing) = client.find_device_by_udid(&imported.udid)? {
        println!(
            "reused\t{}\t{}\t{}\t{}",
            existing.id, existing.platform, existing.udid, existing.name
        );
    } else {
        let created = client.create_device(&imported.name, &imported.udid, args.platform)?;
        println!(
            "created\t{}\t{}\t{}\t{}",
            created.id, created.platform, created.udid, created.name
        );
    }

    let _ = refresh_cache(app)?;
    Ok(())
}

pub fn import_devices(app: &AppContext, args: &ImportDevicesArgs) -> Result<()> {
    let mut client = DeviceClient::connect(app)?;
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
        if client.find_device_by_udid(&device.udid)?.is_none() {
            let created = client.create_device(
                &device.name,
                &device.udid,
                imported_device_platform(&device.platform)?,
            )?;
            println!(
                "created\t{}\t{}\t{}\t{}",
                created.id, created.platform, created.udid, created.name
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
    let mut client = DeviceClient::connect(app)?;
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

    client.delete_device(&target.id)?;
    println!("removed\t{}", target.id);
    let _ = refresh_cache(app)?;
    Ok(())
}

pub fn refresh_cache(app: &AppContext) -> Result<DeviceCache> {
    let mut client = DeviceClient::connect(app)?;
    let mut devices = client.list_devices()?;
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
    if path.extension().and_then(|value| value.to_str()) == Some("json")
        && let Ok(items) = serde_json::from_str::<Vec<ImportedDevice>>(&contents)
    {
        return Ok(items);
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

pub(crate) fn current_machine_provisioning_udid(platform: DevicePlatform) -> Result<String> {
    Ok(current_machine_device(platform)?.udid)
}

fn resolve_team_id(_app: &AppContext) -> Result<String> {
    std::env::var("ORBIT_APPLE_TEAM_ID")
        .ok()
        .or_else(|| manifest_team_id(_app).ok().flatten())
        .context("device management requires an Apple team selection; set ORBIT_APPLE_TEAM_ID")
}

fn manifest_team_id(app: &AppContext) -> Result<Option<String>> {
    let manifest_path = match app.resolve_manifest_path_for_dispatch(None) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let manifest = crate::manifest::read_manifest_value(&manifest_path, app.manifest_env())?;
    Ok(manifest
        .get("team_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned))
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

fn cached_device_from_provisioning(device: ProvisioningDevice) -> CachedDevice {
    CachedDevice {
        id: device.id,
        name: device.name,
        udid: device.udid,
        platform: normalize_device_platform(&device.platform),
        status: device.status.unwrap_or_else(|| "UNKNOWN".to_owned()),
        device_class: device.device_class,
        model: device.model,
        created_at: device.created_at,
    }
}

fn platform_value(platform: DevicePlatform) -> &'static str {
    match platform {
        DevicePlatform::Ios => "IOS",
        DevicePlatform::MacOs => "MAC_OS",
        DevicePlatform::Universal => "UNIVERSAL",
    }
}

fn create_device_platform(platform: DevicePlatform) -> &'static str {
    match platform {
        DevicePlatform::Ios => "IOS",
        DevicePlatform::MacOs | DevicePlatform::Universal => "MAC_OS",
    }
}

fn imported_device_platform(platform: &str) -> Result<DevicePlatform> {
    match platform {
        "IOS" => Ok(DevicePlatform::Ios),
        "MAC_OS" | "MACOS" | "UNIVERSAL" => Ok(DevicePlatform::MacOs),
        other => bail!("unsupported imported device platform `{other}`"),
    }
}

fn normalize_device_platform(platform: &str) -> String {
    match platform {
        "UNIVERSAL" | "MACOS" => "MAC_OS".to_owned(),
        other => other.to_owned(),
    }
}

fn find_registered_device_by_udid(
    client: &mut DeviceClient,
    udid: &str,
) -> Result<Option<CachedDevice>> {
    client.find_device_by_udid(udid)
}

fn find_registered_device_by_id(
    client: &mut DeviceClient,
    id: &str,
) -> Result<Option<CachedDevice>> {
    Ok(client
        .list_devices()?
        .into_iter()
        .find(|device| device.id == id))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::manifest_team_id;
    use crate::context::{AppContext, GlobalPaths};

    #[test]
    fn manifest_team_id_reads_project_team_selection() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("project");
        let data_dir = temp.path().join("data");
        let cache_dir = temp.path().join("cache");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&cache_dir).unwrap();
        fs::write(
            root.join("orbit.json"),
            serde_json::to_vec_pretty(&json!({
                "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
                "name": "ExampleMacApp",
                "bundle_id": "dev.orbit.examples.macos",
                "version": "0.1.0",
                "build": 1,
                "team_id": "TEAM123456",
                "platforms": { "macos": "14.0" },
                "sources": ["Sources/App"]
            }))
            .unwrap(),
        )
        .unwrap();

        let app = AppContext {
            cwd: root,
            interactive: false,
            verbose: false,
            manifest_env: None,
            global_paths: GlobalPaths {
                data_dir: data_dir.clone(),
                cache_dir,
                schema_dir: data_dir.join("schemas"),
                auth_state_path: data_dir.join("auth.json"),
                device_cache_path: data_dir.join("devices.json"),
                keychain_path: data_dir.join("orbit.keychain-db"),
            },
        };

        assert_eq!(
            manifest_team_id(&app).unwrap().as_deref(),
            Some("TEAM123456")
        );
    }
}
