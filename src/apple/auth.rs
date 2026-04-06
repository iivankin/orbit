use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::apple::developer_services::{DeveloperServicesClient, DeveloperServicesTeam};
use crate::context::{AppContext, ProjectContext};
use crate::util::{
    CliSpinner, command_output, command_output_allow_failure, print_success, prompt_input,
    prompt_password, prompt_select, read_json_file_if_exists, write_json_file,
};

const APPLE_PASSWORD_SERVICE: &str = "dev.orbit.cli.apple-password";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AuthState {
    last_mode: Option<StoredAuthMode>,
    user: Option<UserAuth>,
    api_key: Option<ApiKeyAuth>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredAuthMode {
    User,
    ApiKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAuth {
    pub apple_id: String,
    pub team_id: Option<String>,
    pub provider_id: Option<String>,
    pub provider_name: Option<String>,
    pub last_validated_at_unix: Option<u64>,
}

impl UserAuth {
    fn new(apple_id: String) -> Self {
        Self {
            apple_id,
            team_id: None,
            provider_id: None,
            provider_name: None,
            last_validated_at_unix: None,
        }
    }

    fn with_selected_ids(mut self, team_id: Option<String>, provider_id: Option<String>) -> Self {
        if let Some(team_id) = team_id {
            self.team_id = Some(team_id);
        }
        if let Some(provider_id) = provider_id {
            self.provider_id = Some(provider_id);
        }
        self
    }
}

#[derive(Debug, Clone)]
pub struct UserAuthWithPassword {
    pub user: UserAuth,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyAuth {
    pub api_key_path: PathBuf,
    pub key_id: String,
    pub issuer_id: String,
    pub team_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct EnsureUserAuthRequest {
    pub apple_id: Option<String>,
    pub team_id: Option<String>,
    pub provider_id: Option<String>,
    pub prompt_for_missing: bool,
}

#[derive(Debug, Clone)]
struct ResolvedUserInputs {
    user: UserAuth,
    password: Option<String>,
}

pub fn ensure_project_authenticated(project: &ProjectContext) -> Result<()> {
    let app = &project.app;
    if resolve_api_key_auth(app)?.is_some() {
        return Ok(());
    }

    let request = project_user_auth_request(project, app.interactive);
    let mut user = ensure_user_identity(app, &request)?;
    let developer_services = auth_progress_step(
        "Apple auth: Refreshing GrandSlam and Developer Services session",
        |_| "Apple auth: Refreshed GrandSlam and Developer Services session.".to_owned(),
        || DeveloperServicesClient::authenticate(app),
    )?;
    let teams = auth_progress_step(
        "Apple auth: Loading Apple Developer teams",
        |teams: &Vec<DeveloperServicesTeam>| {
            format!(
                "Apple auth: Loaded {} Apple Developer team(s).",
                teams.len()
            )
        },
        || developer_services.list_teams(),
    )?;
    let selected_team = select_developer_services_team(app, &user, &request, teams)?;
    let DeveloperServicesTeam { team_id, name, .. } = selected_team;
    user.team_id = Some(team_id);
    user.provider_name = Some(name);
    user.last_validated_at_unix = Some(current_unix_time());
    persist_user_state(app, &user)?;
    persist_project_auth_selection(project, &user)
}

pub fn resolve_api_key_auth(app: &AppContext) -> Result<Option<ApiKeyAuth>> {
    let env_path = env_path("ORBIT_ASC_API_KEY_PATH")?;
    let env_key_id = env_string("ORBIT_ASC_KEY_ID");
    let env_issuer_id = env_string("ORBIT_ASC_ISSUER_ID");
    let env_team_id = env_string("ORBIT_APPLE_TEAM_ID");

    if let (Some(api_key_path), Some(key_id), Some(issuer_id)) =
        (env_path, env_key_id, env_issuer_id)
    {
        return Ok(Some(ApiKeyAuth {
            api_key_path,
            key_id,
            issuer_id,
            team_id: env_team_id,
        }));
    }

    Ok(load_state(app)?.api_key)
}

pub fn resolve_user_auth_metadata(app: &AppContext) -> Result<Option<UserAuth>> {
    let state = load_state(app)?;
    let apple_id = env_string("ORBIT_APPLE_ID");
    let team_id = env_string("ORBIT_APPLE_TEAM_ID");
    let provider_id = env_string("ORBIT_APPLE_PROVIDER_ID");

    if let Some(apple_id) = apple_id {
        let user = state
            .user
            .filter(|user| user.apple_id == apple_id)
            .unwrap_or_else(|| UserAuth::new(apple_id.clone()))
            .with_selected_ids(team_id, provider_id);
        return Ok(Some(user));
    }

    Ok(state.user)
}

pub(crate) fn ensure_user_identity(
    app: &AppContext,
    request: &EnsureUserAuthRequest,
) -> Result<UserAuth> {
    let state = load_state(app)?;
    let inputs = resolve_user_inputs(app, &state, request)?;
    let user = inputs.user;
    persist_user_state(app, &user)?;
    Ok(user)
}

pub(crate) fn ensure_user_auth_with_password(
    app: &AppContext,
    request: &EnsureUserAuthRequest,
) -> Result<UserAuthWithPassword> {
    let state = load_state(app)?;
    let inputs = resolve_user_inputs(app, &state, request)?;
    let user = inputs.user;
    let password = if let Some(password) = inputs.password {
        password
    } else {
        if !request.prompt_for_missing || !app.interactive {
            bail!(
                "missing Apple ID password for `{}` in env or Keychain",
                user.apple_id
            );
        }
        let password = prompt_password("Apple password")?;
        store_secret(APPLE_PASSWORD_SERVICE, &user.apple_id, &password)?;
        password
    };

    persist_user_state(app, &user)?;
    Ok(UserAuthWithPassword { user, password })
}

fn load_state(app: &AppContext) -> Result<AuthState> {
    Ok(normalize_auth_state(
        read_json_file_if_exists(&app.global_paths.auth_state_path)?.unwrap_or_default(),
    ))
}

fn save_state(app: &AppContext, state: &AuthState) -> Result<()> {
    write_json_file(&app.global_paths.auth_state_path, state)
}

fn persist_user_state(app: &AppContext, user: &UserAuth) -> Result<()> {
    let mut state = load_state(app)?;
    state.user = Some(strip_project_selection(user.clone()));
    state.last_mode = Some(StoredAuthMode::User);
    save_state(app, &state)
}

fn normalize_auth_state(mut state: AuthState) -> AuthState {
    if let Some(user) = state.user.take() {
        state.user = Some(strip_project_selection(user));
    }
    state
}

fn strip_project_selection(mut user: UserAuth) -> UserAuth {
    // Apple team selection belongs to the project manifest, not the global auth cache.
    user.team_id = None;
    user.provider_name = None;
    user
}

fn resolve_user_inputs(
    app: &AppContext,
    state: &AuthState,
    request: &EnsureUserAuthRequest,
) -> Result<ResolvedUserInputs> {
    let apple_id = request
        .apple_id
        .clone()
        .or_else(|| env_string("ORBIT_APPLE_ID"))
        .or_else(|| state.user.as_ref().map(|user| user.apple_id.clone()))
        .or_else(|| {
            if request.prompt_for_missing && app.interactive {
                prompt_input(
                    "Apple ID",
                    state.user.as_ref().map(|user| user.apple_id.as_str()),
                )
                .ok()
            } else {
                None
            }
        })
        .context("Apple ID is required")?;

    let user = state
        .user
        .clone()
        .filter(|user| user.apple_id == apple_id)
        .unwrap_or_else(|| UserAuth::new(apple_id.clone()))
        .with_selected_ids(
            env_string("ORBIT_APPLE_TEAM_ID").or_else(|| request.team_id.clone()),
            env_string("ORBIT_APPLE_PROVIDER_ID").or_else(|| request.provider_id.clone()),
        );

    let password = match env_string("ORBIT_APPLE_PASSWORD") {
        Some(password) => Some(password),
        None => load_secret(APPLE_PASSWORD_SERVICE, &apple_id)?,
    };

    Ok(ResolvedUserInputs { user, password })
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn project_user_auth_request(
    project: &ProjectContext,
    prompt_for_missing: bool,
) -> EnsureUserAuthRequest {
    EnsureUserAuthRequest {
        apple_id: None,
        team_id: project.resolved_manifest.team_id.clone(),
        provider_id: project.resolved_manifest.provider_id.clone(),
        prompt_for_missing,
    }
}

fn select_developer_services_team(
    app: &AppContext,
    user: &UserAuth,
    request: &EnsureUserAuthRequest,
    teams: Vec<DeveloperServicesTeam>,
) -> Result<DeveloperServicesTeam> {
    if teams.is_empty() {
        bail!(
            "Apple account `{}` has no accessible developer teams",
            user.apple_id
        );
    }

    if let Some(team_id) = user.team_id.as_deref() {
        return teams
            .into_iter()
            .find(|team| team.team_id == team_id)
            .with_context(|| {
                format!(
                    "configured Apple team `{team_id}` is not accessible to `{}`",
                    user.apple_id
                )
            });
    }

    if teams.len() == 1 {
        return Ok(teams
            .into_iter()
            .next()
            .expect("one team must exist when len() == 1"));
    }

    if !request.prompt_for_missing || !app.interactive {
        bail!(
            "multiple Apple teams are available for `{}`; set `team_id` in orbit.json or ORBIT_APPLE_TEAM_ID",
            user.apple_id
        );
    }

    let labels = teams
        .iter()
        .map(|team| format!("{} ({})", team.name, team.team_id))
        .collect::<Vec<_>>();
    let index = prompt_select("Select an Apple team", &labels)?;
    teams
        .into_iter()
        .nth(index)
        .context("selected Apple team is out of range")
}

fn auth_progress_step<T, F, G>(
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

fn persist_project_auth_selection(project: &ProjectContext, user: &UserAuth) -> Result<()> {
    if env_string("ORBIT_APPLE_TEAM_ID").is_some()
        || env_string("ORBIT_APPLE_PROVIDER_ID").is_some()
    {
        return Ok(());
    }

    let normalized_team_id = user
        .team_id
        .as_deref()
        .filter(|value| looks_like_apple_team_id(value))
        .map(ToOwned::to_owned);
    let normalized_provider_id = user
        .provider_id
        .as_deref()
        .filter(|value| looks_like_provider_id(value))
        .map(ToOwned::to_owned);

    let changed = persist_auth_selection_fields(
        &project.manifest_path,
        normalized_team_id.as_deref(),
        normalized_provider_id.as_deref(),
    )?;
    if !changed {
        return Ok(());
    }
    print_success(format!(
        "Saved Apple team/provider selection to {}.",
        project.manifest_path.display()
    ));
    Ok(())
}

fn persist_auth_selection_fields(
    manifest_path: &std::path::Path,
    team_id: Option<&str>,
    provider_id: Option<&str>,
) -> Result<bool> {
    let mut manifest: JsonValue = crate::util::read_json_file(manifest_path)?;
    let object = manifest
        .as_object_mut()
        .context("manifest file must contain a top-level object")?;
    let mut changed = false;
    changed |= sync_optional_string_field(object, "team_id", team_id, looks_like_apple_team_id);
    changed |=
        sync_optional_string_field(object, "provider_id", provider_id, looks_like_provider_id);
    if changed {
        write_json_file(manifest_path, &manifest)?;
    }
    Ok(changed)
}

fn sync_optional_string_field(
    object: &mut JsonMap<String, JsonValue>,
    key: &str,
    normalized_value: Option<&str>,
    validator: fn(&str) -> bool,
) -> bool {
    let current_value = object.get(key).and_then(JsonValue::as_str);
    let current_value = current_value.filter(|value| validator(value));

    if current_value == normalized_value {
        return false;
    }

    match normalized_value {
        Some(value) => {
            object.insert(key.to_owned(), JsonValue::String(value.to_owned()));
        }
        None => {
            object.remove(key);
        }
    }
    true
}

fn looks_like_apple_team_id(value: &str) -> bool {
    value.len() == 10
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn looks_like_provider_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn env_path(key: &str) -> Result<Option<PathBuf>> {
    let Some(value) = env_string(key) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.exists() {
        bail!(
            "configured API key path `{}` does not exist",
            path.display()
        );
    }
    Ok(Some(path))
}

fn keychain_enabled() -> bool {
    env_string("ORBIT_NO_KEYCHAIN").is_none()
}

fn store_secret(service: &str, account: &str, secret: &str) -> Result<()> {
    if !keychain_enabled() {
        return Ok(());
    }
    let mut command = Command::new("security");
    command.args([
        "add-generic-password",
        "-U",
        "-a",
        account,
        "-s",
        service,
        "-w",
        secret,
    ]);
    command_output(&mut command).map(|_| ())
}

fn load_secret(service: &str, account: &str) -> Result<Option<String>> {
    if !keychain_enabled() {
        return Ok(None);
    }
    let mut command = Command::new("security");
    command.args(["find-generic-password", "-w", "-a", account, "-s", service]);
    let (success, stdout, _) = command_output_allow_failure(&mut command)?;
    if success {
        return Ok(Some(stdout.trim().to_owned()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        UserAuth, persist_auth_selection_fields, persist_user_state, resolve_user_auth_metadata,
    };
    use crate::context::{AppContext, GlobalPaths};

    fn test_app() -> (tempfile::TempDir, AppContext) {
        let temp = tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let cache_dir = temp.path().join("cache");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&cache_dir).unwrap();
        let app = AppContext {
            cwd: temp.path().to_path_buf(),
            interactive: false,
            verbose: false,
            global_paths: GlobalPaths {
                data_dir: data_dir.clone(),
                cache_dir,
                schema_dir: data_dir.join("schemas"),
                auth_state_path: data_dir.join("auth.json"),
                device_cache_path: data_dir.join("devices.json"),
                keychain_path: data_dir.join("orbit.keychain-db"),
            },
        };
        (temp, app)
    }

    #[test]
    fn persisting_auth_selection_keeps_authoring_manifest_shape() {
        let temp = tempdir().unwrap();
        let manifest_path = temp.path().join("orbit.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
                "name": "ExampleMacApp",
                "bundle_id": "dev.orbit.examples.macos",
                "version": "0.1.0",
                "build": 1,
                "platforms": { "macos": "14.0" },
                "sources": ["Sources/App"]
            }))
            .unwrap(),
        )
        .unwrap();

        let changed =
            persist_auth_selection_fields(&manifest_path, Some("TEAM123456"), Some("128120286"))
                .unwrap();
        assert!(changed);

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest.get("$schema").and_then(|value| value.as_str()),
            Some("/tmp/.orbit/schemas/apple-app.v1.json")
        );
        assert_eq!(
            manifest.get("bundle_id").and_then(|value| value.as_str()),
            Some("dev.orbit.examples.macos")
        );
        assert!(manifest.get("targets").is_none());
        assert_eq!(
            manifest.get("team_id").and_then(|value| value.as_str()),
            Some("TEAM123456")
        );
        assert_eq!(
            manifest.get("provider_id").and_then(|value| value.as_str()),
            Some("128120286")
        );
    }

    #[test]
    fn persisted_user_state_omits_project_scoped_team_selection() {
        let (_temp, app) = test_app();
        persist_user_state(
            &app,
            &UserAuth {
                apple_id: "dev@example.com".to_owned(),
                team_id: Some("TEAM123456".to_owned()),
                provider_id: Some("128120286".to_owned()),
                provider_name: Some("Example Team".to_owned()),
                last_validated_at_unix: Some(123),
            },
        )
        .unwrap();

        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(&app.global_paths.auth_state_path).unwrap()).unwrap();
        assert_eq!(state["user"]["apple_id"].as_str(), Some("dev@example.com"));
        assert_eq!(state["user"]["team_id"].as_str(), None);
        assert_eq!(state["user"]["provider_id"].as_str(), Some("128120286"));
        assert_eq!(state["user"]["provider_name"].as_str(), None);
    }

    #[test]
    fn resolve_user_auth_metadata_ignores_stale_global_team_selection() {
        let (_temp, app) = test_app();
        fs::write(
            &app.global_paths.auth_state_path,
            serde_json::to_vec_pretty(&json!({
                "last_mode": "user",
                "user": {
                    "apple_id": "dev@example.com",
                    "team_id": "TEAM123456",
                    "provider_id": "128120286",
                    "provider_name": "Example Team",
                    "last_validated_at_unix": 123
                },
                "api_key": null
            }))
            .unwrap(),
        )
        .unwrap();

        let user = resolve_user_auth_metadata(&app).unwrap().unwrap();
        assert_eq!(user.apple_id, "dev@example.com");
        assert_eq!(user.team_id, None);
        assert_eq!(user.provider_id.as_deref(), Some("128120286"));
        assert_eq!(user.provider_name, None);
    }
}
