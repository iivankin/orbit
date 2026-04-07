use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orbit::apple::asc_api::{AscClient, Resource};
use orbit::apple::auth::{ApiKeyAuth, resolve_user_auth_metadata};
use orbit::apple::capabilities::RemoteCapability;
use orbit::apple::provisioning::{
    ProvisioningAppGroup, ProvisioningBundleId, ProvisioningCertificate, ProvisioningClient,
    ProvisioningCloudContainer, ProvisioningDevice, ProvisioningMerchantId, ProvisioningProfile,
};
use orbit::context::AppContext;
use uuid::Uuid;

use super::orbit_bin;

#[derive(Clone)]
pub struct LiveAppleConfig {
    pub apple_id: String,
    pub team_id: String,
    pub provider_id: Option<String>,
    pub schema_path: PathBuf,
    pub bundle_prefix: String,
}

#[derive(Clone)]
pub struct LiveAscConfig {
    pub apple: LiveAppleConfig,
    pub api_key_path: PathBuf,
    pub key_id: String,
    pub issuer_id: String,
}

pub struct LiveCleanupGuard {
    workspace: PathBuf,
    config: LiveAppleConfig,
    mode: &'static str,
    enabled: bool,
}

impl LiveAppleConfig {
    pub fn unique_app_identity(&self, label: &str) -> (String, String) {
        let normalized_label = normalize_label(label);
        let suffix = Uuid::new_v4().simple().to_string();
        let short_suffix = &suffix[..12];
        let name = format!("Orbit{normalized_label}{short_suffix}");
        let bundle_id = format!(
            "{}.{}.{}",
            self.bundle_prefix,
            normalized_label.to_ascii_lowercase(),
            short_suffix.to_ascii_lowercase()
        );
        (name, bundle_id)
    }

    pub fn orbit_data_dir(workspace: &Path) -> PathBuf {
        workspace.join(".live-orbit-data")
    }

    pub fn orbit_cache_dir(workspace: &Path) -> PathBuf {
        workspace.join(".live-orbit-cache")
    }
}

impl LiveCleanupGuard {
    pub fn remote_and_local(workspace: &Path, config: &LiveAppleConfig) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            config: config.clone(),
            mode: "--all",
            enabled: true,
        }
    }

    pub fn local_only(workspace: &Path, config: &LiveAppleConfig) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            config: config.clone(),
            mode: "--local",
            enabled: true,
        }
    }

    pub fn disarm(&mut self) {
        self.enabled = false;
    }
}

impl Drop for LiveCleanupGuard {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        let manifest_path = self.workspace.join("orbit.json");
        if !manifest_path.exists() {
            return;
        }
        let output = live_command(&self.workspace, &self.config)
            .args([
                "--non-interactive",
                "--manifest",
                manifest_path.to_str().unwrap(),
                "clean",
                self.mode,
            ])
            .output();
        match output {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                eprintln!(
                    "best-effort live cleanup failed ({}): {}",
                    self.mode,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(error) => {
                eprintln!("best-effort live cleanup failed to start: {error}");
            }
        }
    }
}

pub fn require_live_apple_config(enable_env: &str) -> LiveAppleConfig {
    require_live_config(enable_env, true)
}

pub fn require_live_asc_config(enable_env: &str) -> LiveAscConfig {
    let apple = require_live_config(enable_env, false);
    LiveAscConfig {
        apple,
        api_key_path: env_path("ORBIT_ASC_API_KEY_PATH"),
        key_id: required_env("ORBIT_ASC_KEY_ID"),
        issuer_id: required_env("ORBIT_ASC_ISSUER_ID"),
    }
}

fn require_live_config(enable_env: &str, require_apple_id: bool) -> LiveAppleConfig {
    assert_eq!(
        std::env::var(enable_env).as_deref(),
        Ok("1"),
        "set {enable_env}=1 to run this live Apple account test"
    );

    let saved_user = AppContext::new(true, false, None)
        .ok()
        .and_then(|app| resolve_user_auth_metadata(&app).ok().flatten());
    let apple_id = std::env::var("ORBIT_APPLE_ID")
        .ok()
        .or_else(|| saved_user.as_ref().map(|user| user.apple_id.clone()))
        .or_else(|| (!require_apple_id).then(String::new))
        .unwrap_or_else(|| required_env("ORBIT_APPLE_ID"));
    let team_id = std::env::var("ORBIT_APPLE_TEAM_ID")
        .ok()
        .or_else(|| saved_user.as_ref().and_then(|user| user.team_id.clone()))
        .unwrap_or_else(|| required_env("ORBIT_APPLE_TEAM_ID"));
    let provider_id = std::env::var("ORBIT_APPLE_PROVIDER_ID").ok().or_else(|| {
        saved_user
            .as_ref()
            .and_then(|user| user.provider_id.clone())
    });
    let bundle_prefix = std::env::var("ORBIT_LIVE_TEST_BUNDLE_PREFIX")
        .unwrap_or_else(|_| "dev.orbit.livee2e".to_owned());
    let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join("apple-app.v1.json");
    assert!(
        schema_path.exists(),
        "missing local schema at {}",
        schema_path.display()
    );

    LiveAppleConfig {
        apple_id,
        team_id,
        provider_id,
        schema_path,
        bundle_prefix,
    }
}

pub fn create_live_workspace(
    root: &Path,
    config: &LiveAppleConfig,
    app_name: &str,
    bundle_id: &str,
) -> PathBuf {
    let mut manifest = serde_json::json!({
        "$schema": config.schema_path,
        "name": app_name,
        "bundle_id": bundle_id,
        "version": "1.0.0",
        "build": 1,
        "team_id": config.team_id,
        "platforms": {
            "ios": "18.0"
        },
        "sources": [
            "Sources/App"
        ]
    });

    if let Some(provider_id) = &config.provider_id {
        manifest["provider_id"] = serde_json::Value::String(provider_id.clone());
    }

    create_live_workspace_with_manifest(root, app_name, &manifest)
}

pub fn create_live_workspace_with_manifest(
    root: &Path,
    app_name: &str,
    manifest: &serde_json::Value,
) -> PathBuf {
    let workspace = root.join(app_name);
    fs::create_dir_all(workspace.join("Sources/App")).unwrap();
    fs::write(
        workspace.join("Sources/App/App.swift"),
        format!(
            "import SwiftUI\n@main struct {app_name}: App {{ var body: some Scene {{ WindowGroup {{ Text(\"{app_name}\") }} }} }}\n"
        ),
    )
    .unwrap();

    fs::write(
        workspace.join("orbit.json"),
        serde_json::to_vec_pretty(manifest).unwrap(),
    )
    .unwrap();
    workspace
}

pub fn live_command(workspace: &Path, config: &LiveAppleConfig) -> Command {
    live_command_impl(workspace, config, true)
}

pub fn live_command_without_team_state(workspace: &Path, config: &LiveAppleConfig) -> Command {
    live_command_impl(workspace, config, false)
}

pub fn live_asc_command(workspace: &Path, config: &LiveAscConfig) -> Command {
    let orbit_data_dir = LiveAppleConfig::orbit_data_dir(workspace);
    let orbit_cache_dir = LiveAppleConfig::orbit_cache_dir(workspace);
    fs::create_dir_all(&orbit_data_dir).unwrap();
    fs::create_dir_all(&orbit_cache_dir).unwrap();

    let mut command = Command::new(orbit_bin());
    command.current_dir(workspace);
    command.env("ORBIT_APPLE_TEAM_ID", &config.apple.team_id);
    command.env("ORBIT_DATA_DIR", &orbit_data_dir);
    command.env("ORBIT_CACHE_DIR", &orbit_cache_dir);
    command.env("ORBIT_ASC_API_KEY_PATH", &config.api_key_path);
    command.env("ORBIT_ASC_KEY_ID", &config.key_id);
    command.env("ORBIT_ASC_ISSUER_ID", &config.issuer_id);
    command.env_remove("ORBIT_APPLE_ID");
    command.env_remove("ORBIT_APPLE_PROVIDER_ID");
    command
}

fn live_command_impl(workspace: &Path, config: &LiveAppleConfig, seed_team_state: bool) -> Command {
    let orbit_data_dir = LiveAppleConfig::orbit_data_dir(workspace);
    let orbit_cache_dir = LiveAppleConfig::orbit_cache_dir(workspace);
    fs::create_dir_all(&orbit_data_dir).unwrap();
    fs::create_dir_all(&orbit_cache_dir).unwrap();
    seed_live_auth_state(&orbit_data_dir);
    if seed_team_state {
        seed_live_team_state(&orbit_data_dir, &config.team_id);
    }

    let mut command = Command::new(orbit_bin());
    command.current_dir(workspace);
    command.env("ORBIT_APPLE_ID", &config.apple_id);
    command.env("ORBIT_APPLE_TEAM_ID", &config.team_id);
    command.env("ORBIT_DATA_DIR", &orbit_data_dir);
    command.env("ORBIT_CACHE_DIR", &orbit_cache_dir);
    if let Some(provider_id) = &config.provider_id {
        command.env("ORBIT_APPLE_PROVIDER_ID", provider_id);
    }
    command.env_remove("ORBIT_ASC_API_KEY_PATH");
    command.env_remove("ORBIT_ASC_KEY_ID");
    command.env_remove("ORBIT_ASC_ISSUER_ID");
    command
}

pub fn remote_capabilities_for_bundle_id(
    config: &LiveAppleConfig,
    bundle_id: &str,
) -> Vec<RemoteCapability> {
    wait_for_remote_bundle_id(config, bundle_id).capabilities
}

pub fn remote_app_groups_for_identifier(
    config: &LiveAppleConfig,
    identifier: &str,
) -> Vec<ProvisioningAppGroup> {
    let app = AppContext::new(true, false, None).unwrap();
    let mut provisioning = ProvisioningClient::authenticate(&app, config.team_id.clone()).unwrap();
    provisioning
        .list_app_groups()
        .unwrap()
        .into_iter()
        .filter(|group| group.identifier == identifier)
        .collect()
}

pub fn wait_for_remote_app_group_count(
    config: &LiveAppleConfig,
    identifier: &str,
    expected_count: usize,
) -> Vec<ProvisioningAppGroup> {
    wait_for_remote_resource_count(
        || remote_app_groups_for_identifier(config, identifier),
        format!("app group `{identifier}`"),
        expected_count,
    )
}

pub fn remote_merchant_ids_for_identifier(
    config: &LiveAppleConfig,
    identifier: &str,
) -> Vec<ProvisioningMerchantId> {
    let app = AppContext::new(true, false, None).unwrap();
    let mut provisioning = ProvisioningClient::authenticate(&app, config.team_id.clone()).unwrap();
    provisioning
        .list_merchant_ids()
        .unwrap()
        .into_iter()
        .filter(|merchant| merchant.identifier == identifier)
        .collect()
}

pub fn wait_for_remote_merchant_id_count(
    config: &LiveAppleConfig,
    identifier: &str,
    expected_count: usize,
) -> Vec<ProvisioningMerchantId> {
    wait_for_remote_resource_count(
        || remote_merchant_ids_for_identifier(config, identifier),
        format!("merchant id `{identifier}`"),
        expected_count,
    )
}

pub fn remote_cloud_containers_for_identifier(
    config: &LiveAppleConfig,
    identifier: &str,
) -> Vec<ProvisioningCloudContainer> {
    let app = AppContext::new(true, false, None).unwrap();
    let mut provisioning = ProvisioningClient::authenticate(&app, config.team_id.clone()).unwrap();
    provisioning
        .list_cloud_containers()
        .unwrap()
        .into_iter()
        .filter(|container| container.identifier == identifier)
        .collect()
}

pub fn remote_certificates_for_type(
    config: &LiveAppleConfig,
    certificate_type: &str,
) -> Vec<ProvisioningCertificate> {
    let app = AppContext::new(true, false, None).unwrap();
    let mut provisioning = ProvisioningClient::authenticate(&app, config.team_id.clone()).unwrap();
    provisioning.list_certificates(certificate_type).unwrap()
}

pub fn remote_asc_certificates_for_type(
    config: &LiveAscConfig,
    certificate_type: &str,
) -> Vec<Resource<orbit::apple::asc_api::CertificateAttributes>> {
    let client = AscClient::new(ApiKeyAuth {
        api_key_path: config.api_key_path.clone(),
        key_id: config.key_id.clone(),
        issuer_id: config.issuer_id.clone(),
        team_id: Some(config.apple.team_id.clone()),
    })
    .unwrap();
    client.list_certificates(certificate_type).unwrap()
}

pub fn wait_for_remote_cloud_container_count(
    config: &LiveAppleConfig,
    identifier: &str,
    expected_count: usize,
) -> Vec<ProvisioningCloudContainer> {
    wait_for_remote_resource_count(
        || remote_cloud_containers_for_identifier(config, identifier),
        format!("cloud container `{identifier}`"),
        expected_count,
    )
}

pub fn remote_devices_for_platform(
    config: &LiveAppleConfig,
    platform: &str,
) -> Vec<ProvisioningDevice> {
    let app = AppContext::new(true, false, None).unwrap();
    let mut provisioning = ProvisioningClient::authenticate(&app, config.team_id.clone()).unwrap();
    provisioning
        .list_devices()
        .unwrap()
        .into_iter()
        .filter(|device| device.platform == platform)
        .collect()
}

pub fn wait_for_remote_capability_state(
    config: &LiveAppleConfig,
    bundle_id: &str,
    capability_type: &str,
    present: bool,
) -> Vec<RemoteCapability> {
    for _ in 0..30 {
        let capabilities = remote_capabilities_for_bundle_id(config, bundle_id);
        let has_capability = capabilities.iter().any(|capability| {
            capability.capability_type == capability_type && capability.enabled.unwrap_or(true)
        });
        if has_capability == present {
            return capabilities;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    panic!(
        "timed out waiting for remote capability `{capability_type}` present={present} on `{bundle_id}`"
    );
}

fn wait_for_remote_resource_count<T, F>(
    mut fetch: F,
    resource_label: String,
    expected_count: usize,
) -> Vec<T>
where
    F: FnMut() -> Vec<T>,
{
    for _ in 0..30 {
        let resources = fetch();
        if resources.len() == expected_count {
            return resources;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    panic!("timed out waiting for {resource_label} count={expected_count}");
}

fn wait_for_remote_bundle_id(config: &LiveAppleConfig, bundle_id: &str) -> ProvisioningBundleId {
    let app = AppContext::new(true, false, None).unwrap();
    let mut provisioning = ProvisioningClient::authenticate(&app, config.team_id.clone()).unwrap();
    for _ in 0..30 {
        if let Some(bundle) = provisioning.find_bundle_id(bundle_id).unwrap() {
            return bundle;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    panic!("missing remote bundle id `{bundle_id}`");
}

pub fn remote_profiles_for_bundle_id(
    config: &LiveAppleConfig,
    bundle_id: &str,
    profile_type: Option<&str>,
) -> Vec<ProvisioningProfile> {
    let app = AppContext::new(true, false, None).unwrap();
    let mut provisioning = ProvisioningClient::authenticate(&app, config.team_id.clone()).unwrap();
    provisioning
        .list_profiles(profile_type)
        .unwrap()
        .into_iter()
        .filter(|profile| profile.bundle_id_identifier.as_deref() == Some(bundle_id))
        .collect()
}

pub fn wait_for_remote_profile_count(
    config: &LiveAppleConfig,
    bundle_id: &str,
    profile_type: Option<&str>,
    expected_count: usize,
) -> Vec<ProvisioningProfile> {
    wait_for_remote_resource_count(
        || remote_profiles_for_bundle_id(config, bundle_id, profile_type),
        format!("provisioning profiles for `{bundle_id}`"),
        expected_count,
    )
}

fn seed_live_auth_state(orbit_data_dir: &Path) {
    let Some(source_data_dir) = seed_source_data_dir(orbit_data_dir) else {
        return;
    };

    let source_auth = source_data_dir.join("auth.json");
    let destination_auth = orbit_data_dir.join("auth.json");
    if source_auth.exists() && !destination_auth.exists() {
        fs::create_dir_all(orbit_data_dir).unwrap();
        fs::copy(&source_auth, &destination_auth).unwrap();
    }
}

fn seed_live_team_state(orbit_data_dir: &Path, team_id: &str) {
    let Some(source_data_dir) = seed_source_data_dir(orbit_data_dir) else {
        return;
    };

    let source_team_dir = source_data_dir.join("teams").join(team_id);
    if !source_team_dir.exists() {
        return;
    }
    let destination_team_dir = orbit_data_dir.join("teams").join(team_id);
    if destination_team_dir.exists() {
        return;
    }
    copy_dir_recursive(&source_team_dir, &destination_team_dir);
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).unwrap();
        }
    }
}

fn seed_source_data_dir(orbit_data_dir: &Path) -> Option<PathBuf> {
    let Ok(source_app) = AppContext::new(true, false, None) else {
        return None;
    };
    let source_data_dir = source_app.global_paths.data_dir;
    (source_data_dir != orbit_data_dir).then_some(source_data_dir)
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing required env `{name}`"))
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(required_env(name))
}

fn normalize_label(label: &str) -> String {
    label.chars().filter(char::is_ascii_alphanumeric).collect()
}
