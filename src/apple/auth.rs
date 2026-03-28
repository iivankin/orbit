use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::apple::apple_id::{self, AppleIdError, StoredAppleSession};
use crate::context::{AppContext, ProjectContext};
use crate::util::{
    command_output, command_output_allow_failure, print_success, prompt_confirm, prompt_input,
    read_json_file_if_exists, write_json_file,
};

const APPLE_PASSWORD_SERVICE: &str = "dev.orbit.cli.apple-password";
const APPLE_SESSION_SERVICE: &str = "dev.orbit.cli.apple-session";

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

#[derive(Debug, Clone)]
pub struct UserAuthWithPassword {
    pub user: UserAuth,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct PortalAuth {
    pub user: UserAuth,
    pub session: StoredAppleSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyAuth {
    pub api_key_path: PathBuf,
    pub key_id: String,
    pub issuer_id: String,
    pub team_id: Option<String>,
    pub team_type: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SubmitAuth {
    ApiKey {
        key_id: String,
        issuer_id: String,
        api_key_path: PathBuf,
    },
    AppleId {
        apple_id: String,
        password: String,
        team_id: Option<String>,
        provider_id: Option<String>,
    },
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
    session: Option<StoredAppleSession>,
    password: Option<String>,
    password_source: Option<PasswordSource>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PasswordSource {
    Env,
    Keychain,
    Prompt,
}

fn process_auth_cache() -> &'static Mutex<Option<UserAuth>> {
    static CACHE: OnceLock<Mutex<Option<UserAuth>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn cached_authenticated_user(request: &EnsureUserAuthRequest) -> Result<Option<UserAuth>> {
    let cache = process_auth_cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("Apple auth cache is poisoned"))?;
    Ok(cache
        .as_ref()
        .filter(|user| request_matches_user(request, user))
        .cloned())
}

fn cache_authenticated_user(user: &UserAuth) -> Result<()> {
    let mut cache = process_auth_cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("Apple auth cache is poisoned"))?;
    *cache = Some(user.clone());
    Ok(())
}

fn request_matches_user(request: &EnsureUserAuthRequest, user: &UserAuth) -> bool {
    request
        .apple_id
        .as_deref()
        .is_none_or(|apple_id| apple_id == user.apple_id)
        && request
            .team_id
            .as_deref()
            .is_none_or(|team_id| user.team_id.as_deref() == Some(team_id))
        && request
            .provider_id
            .as_deref()
            .is_none_or(|provider_id| user.provider_id.as_deref() == Some(provider_id))
}

pub fn best_effort_app_store_authenticate(project: &ProjectContext) -> Result<()> {
    let app = &project.app;
    if resolve_api_key_auth(app)?.is_some() {
        return Ok(());
    }
    if !app.interactive {
        return Ok(());
    }

    let request = project_user_auth_request(project, false);
    if cached_authenticated_user(&request)?.is_some() {
        return Ok(());
    }
    if let Some(user) = resolve_user_auth_metadata(app)? {
        if let Some(session) = load_user_session(&user.apple_id)? {
            let mut authenticated_user = user.clone();
            if let Some(authenticated) = apple_id::restore_session(
                &session,
                request.team_id.as_deref(),
                request.provider_id.as_deref(),
                app.interactive,
            )? {
                apply_authenticated_user(&mut authenticated_user, &authenticated);
                persist_user_state(app, &authenticated_user, Some(&authenticated.session))?;
                persist_project_auth_selection(project, &authenticated_user)?;
                print_success(auth_success_message(
                    "Reused saved Apple session",
                    &authenticated_user,
                ));
                return Ok(());
            }
        }
    }

    Ok(())
}

pub fn ensure_user_authenticated(
    app: &AppContext,
    request: EnsureUserAuthRequest,
) -> Result<UserAuth> {
    let state = load_state(app)?;
    let resolved = resolve_user_inputs(app, &state, &request)?;
    let mut user = resolved.user;
    let mut password = resolved.password;
    let mut password_source = resolved.password_source;

    if let Some(cached) = cached_authenticated_user(&EnsureUserAuthRequest {
        apple_id: Some(user.apple_id.clone()),
        team_id: user.team_id.clone(),
        provider_id: user.provider_id.clone(),
        prompt_for_missing: false,
    })? {
        return Ok(cached);
    }

    if let Some(session) = resolved.session {
        if let Some(authenticated) = apple_id::restore_session(
            &session,
            user.team_id.as_deref(),
            user.provider_id.as_deref(),
            app.interactive,
        )? {
            apply_authenticated_user(&mut user, &authenticated);
            persist_user_state(app, &user, Some(&authenticated.session))?;
            print_success(auth_success_message("Reused saved Apple session", &user));
            return Ok(user);
        }
    }

    loop {
        if password.is_none() {
            if !request.prompt_for_missing || !app.interactive {
                bail!(
                    "missing Apple ID password for `{}` in env or Keychain",
                    user.apple_id
                );
            }
            password = Some(prompt_input_password()?);
            password_source = Some(PasswordSource::Prompt);
        }

        match apple_id::login_with_password(
            &user.apple_id,
            password
                .as_deref()
                .expect("password must be set before login"),
            user.team_id.as_deref(),
            user.provider_id.as_deref(),
            app.interactive,
        ) {
            Ok(authenticated) => {
                apply_authenticated_user(&mut user, &authenticated);
                if matches!(password_source, Some(PasswordSource::Prompt)) {
                    store_secret(
                        APPLE_PASSWORD_SERVICE,
                        &user.apple_id,
                        password.as_deref().expect("password must still be set"),
                    )?;
                }
                persist_user_state(app, &user, Some(&authenticated.session))?;
                print_success(auth_success_message(
                    "Logged in and verified Apple account",
                    &user,
                ));
                return Ok(user);
            }
            Err(error)
                if error
                    .downcast_ref::<AppleIdError>()
                    .is_some_and(is_invalid_credentials) =>
            {
                if matches!(password_source, Some(PasswordSource::Keychain)) {
                    let _ = delete_secret(APPLE_PASSWORD_SERVICE, &user.apple_id);
                }
                if !request.prompt_for_missing
                    || !app.interactive
                    || !prompt_confirm("Apple credentials were rejected. Try again?", true)?
                {
                    return Err(error);
                }
                password = None;
                password_source = None;
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn ensure_project_authenticated(project: &ProjectContext) -> Result<()> {
    let app = &project.app;
    if resolve_api_key_auth(app)?.is_some() {
        return Ok(());
    }

    let user = ensure_user_authenticated(app, project_user_auth_request(project, app.interactive))?;
    persist_project_auth_selection(project, &user)
}

pub fn resolve_submit_auth(project: &ProjectContext) -> Result<SubmitAuth> {
    let app = &project.app;
    if let Some(api_key) = resolve_api_key_auth(app)? {
        return Ok(SubmitAuth::ApiKey {
            key_id: api_key.key_id,
            issuer_id: api_key.issuer_id,
            api_key_path: api_key.api_key_path,
        });
    }

    let user = ensure_user_authenticated(app, project_user_auth_request(project, app.interactive))?;
    persist_project_auth_selection(project, &user)?;

    let password = resolve_submit_password(app, &user)?;
    Ok(SubmitAuth::AppleId {
        apple_id: user.apple_id,
        password,
        team_id: user.team_id,
        provider_id: user.provider_id,
    })
}

pub fn ensure_portal_authenticated(
    app: &AppContext,
    request: EnsureUserAuthRequest,
) -> Result<PortalAuth> {
    let user = ensure_user_authenticated(app, request)?;
    let session = load_user_session(&user.apple_id)?
        .with_context(|| format!("missing Apple session for `{}` after login", user.apple_id))?;
    Ok(PortalAuth { user, session })
}

pub fn resolve_api_key_auth(app: &AppContext) -> Result<Option<ApiKeyAuth>> {
    let env_path = env_path(["ORBIT_ASC_API_KEY_PATH", "EXPO_ASC_API_KEY_PATH"])?;
    let env_key_id = env_string(["ORBIT_ASC_KEY_ID", "EXPO_ASC_KEY_ID"]);
    let env_issuer_id = env_string(["ORBIT_ASC_ISSUER_ID", "EXPO_ASC_ISSUER_ID"]);
    let env_team_id = env_string(["ORBIT_APPLE_TEAM_ID", "EXPO_APPLE_TEAM_ID"]);
    let env_team_type = env_string(["ORBIT_APPLE_TEAM_TYPE", "EXPO_APPLE_TEAM_TYPE"]);

    if let (Some(api_key_path), Some(key_id), Some(issuer_id)) =
        (env_path, env_key_id, env_issuer_id)
    {
        return Ok(Some(ApiKeyAuth {
            api_key_path,
            key_id,
            issuer_id,
            team_id: env_team_id,
            team_type: env_team_type,
        }));
    }

    Ok(load_state(app)?.api_key)
}

pub fn resolve_user_auth_metadata(app: &AppContext) -> Result<Option<UserAuth>> {
    let state = load_state(app)?;
    let apple_id = env_string(["ORBIT_APPLE_ID", "EXPO_APPLE_ID"]);
    let team_id = env_string(["ORBIT_APPLE_TEAM_ID", "EXPO_APPLE_TEAM_ID"]);
    let provider_id = env_string(["ORBIT_APPLE_PROVIDER_ID", "EXPO_APPLE_PROVIDER_ID"]);

    if let Some(apple_id) = apple_id {
        let mut user = state
            .user
            .filter(|user| user.apple_id == apple_id)
            .unwrap_or(UserAuth {
                apple_id: apple_id.clone(),
                team_id: None,
                provider_id: None,
                provider_name: None,
                last_validated_at_unix: None,
            });
        user.apple_id = apple_id;
        if team_id.is_some() {
            user.team_id = team_id;
        }
        if provider_id.is_some() {
            user.provider_id = provider_id;
        }
        return Ok(Some(user));
    }

    Ok(state.user)
}

fn resolve_user_auth_with_password(app: &AppContext) -> Result<Option<UserAuthWithPassword>> {
    let Some(user) = resolve_user_auth_metadata(app)? else {
        return Ok(None);
    };
    let password = match env_string(["ORBIT_APPLE_PASSWORD", "EXPO_APPLE_PASSWORD"]) {
        Some(password) => password,
        None => load_secret(APPLE_PASSWORD_SERVICE, &user.apple_id)?.with_context(|| {
            format!(
                "missing password for Apple ID `{}` in env or Keychain",
                user.apple_id
            )
        })?,
    };
    Ok(Some(UserAuthWithPassword { user, password }))
}

fn resolve_submit_password(app: &AppContext, user: &UserAuth) -> Result<String> {
    if let Some(password) = env_string([
        "ORBIT_APPLE_APP_SPECIFIC_PASSWORD",
        "EXPO_APPLE_APP_SPECIFIC_PASSWORD",
    ]) {
        return Ok(password);
    }
    resolve_user_auth_with_password(app)?
        .filter(|credentials| credentials.user.apple_id == user.apple_id)
        .map(|credentials| credentials.password)
        .with_context(|| {
            format!(
                "submit requires an App Store Connect API key or Apple ID credentials for `{}`",
                user.apple_id
            )
        })
}

fn load_state(app: &AppContext) -> Result<AuthState> {
    Ok(read_json_file_if_exists(&app.global_paths.auth_state_path)?.unwrap_or_default())
}

fn save_state(app: &AppContext, state: &AuthState) -> Result<()> {
    write_json_file(&app.global_paths.auth_state_path, state)
}

fn persist_user_state(
    app: &AppContext,
    user: &UserAuth,
    session: Option<&StoredAppleSession>,
) -> Result<()> {
    if let Some(session) = session {
        store_user_session(&user.apple_id, session)?;
    }

    let mut state = load_state(app)?;
    state.user = Some(user.clone());
    state.last_mode = Some(StoredAuthMode::User);
    save_state(app, &state)?;
    cache_authenticated_user(user)
}

fn resolve_user_inputs(
    app: &AppContext,
    state: &AuthState,
    request: &EnsureUserAuthRequest,
) -> Result<ResolvedUserInputs> {
    let apple_id = request
        .apple_id
        .clone()
        .or_else(|| env_string(["ORBIT_APPLE_ID", "EXPO_APPLE_ID"]))
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

    let mut user = state
        .user
        .clone()
        .filter(|user| user.apple_id == apple_id)
        .unwrap_or(UserAuth {
            apple_id: apple_id.clone(),
            team_id: None,
            provider_id: None,
            provider_name: None,
            last_validated_at_unix: None,
        });
    user.apple_id = apple_id.clone();
    user.team_id = env_string(["ORBIT_APPLE_TEAM_ID", "EXPO_APPLE_TEAM_ID"])
        .or_else(|| request.team_id.clone());
    user.provider_id = env_string(["ORBIT_APPLE_PROVIDER_ID", "EXPO_APPLE_PROVIDER_ID"])
        .or_else(|| request.provider_id.clone());

    let (password, password_source) =
        match env_string(["ORBIT_APPLE_PASSWORD", "EXPO_APPLE_PASSWORD"]) {
            Some(password) => (Some(password), Some(PasswordSource::Env)),
            None => match load_secret(APPLE_PASSWORD_SERVICE, &apple_id)? {
                Some(password) => (Some(password), Some(PasswordSource::Keychain)),
                None => (None, None),
            },
        };

    Ok(ResolvedUserInputs {
        user,
        session: load_user_session(&apple_id)?,
        password,
        password_source,
    })
}

fn apply_authenticated_user(user: &mut UserAuth, authenticated: &apple_id::AppleAuthResponse) {
    user.team_id = authenticated
        .team_id
        .clone()
        .or_else(|| user.team_id.clone());
    user.provider_id = authenticated
        .provider_id
        .clone()
        .filter(|value| looks_like_provider_id(value))
        .or_else(|| {
            user.provider_id
                .clone()
                .filter(|value| looks_like_provider_id(value))
        });
    user.provider_name = authenticated
        .provider_name
        .clone()
        .or_else(|| user.provider_name.clone());
    user.last_validated_at_unix = Some(current_unix_time());
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn auth_success_message(prefix: &str, user: &UserAuth) -> String {
    let masked_apple_id = mask_apple_id(&user.apple_id);
    match user.provider_name.as_deref() {
        Some(provider_name) => format!("{prefix} for {masked_apple_id} on {provider_name}."),
        None => format!("{prefix} for {masked_apple_id}."),
    }
}

fn mask_apple_id(apple_id: &str) -> String {
    let Some((local_part, domain)) = apple_id.split_once('@') else {
        let prefix = apple_id.chars().take(4).collect::<String>();
        return if apple_id.chars().count() > prefix.chars().count() {
            format!("{prefix}…")
        } else {
            prefix
        };
    };

    let visible_local = local_part.chars().take(4).collect::<String>();
    if local_part.chars().count() > visible_local.chars().count() {
        format!("{visible_local}…@{domain}")
    } else {
        format!("{visible_local}@{domain}")
    }
}

fn is_invalid_credentials(error: &AppleIdError) -> bool {
    matches!(error, AppleIdError::InvalidCredentials)
}

fn prompt_input_password() -> Result<String> {
    crate::util::prompt_password("Apple password")
}

fn project_user_auth_request(
    project: &ProjectContext,
    prompt_for_missing: bool,
) -> EnsureUserAuthRequest {
    EnsureUserAuthRequest {
        apple_id: None,
        team_id: project.manifest.team_id.clone(),
        provider_id: project.manifest.provider_id.clone(),
        prompt_for_missing,
    }
}

fn persist_project_auth_selection(project: &ProjectContext, user: &UserAuth) -> Result<()> {
    if env_string(["ORBIT_APPLE_TEAM_ID", "EXPO_APPLE_TEAM_ID"]).is_some()
        || env_string(["ORBIT_APPLE_PROVIDER_ID", "EXPO_APPLE_PROVIDER_ID"]).is_some()
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
        "Saved Apple provider selection to {}.",
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
    changed |= sync_optional_string_field(object, "provider_id", provider_id, looks_like_provider_id);
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

fn env_string<const N: usize>(keys: [&str; N]) -> Option<String> {
    keys.into_iter().find_map(|key| std::env::var(key).ok())
}

fn env_path<const N: usize>(keys: [&str; N]) -> Result<Option<PathBuf>> {
    let Some(value) = env_string(keys) else {
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
    env_string(["ORBIT_NO_KEYCHAIN", "EXPO_NO_KEYCHAIN"]).is_none()
}

fn store_user_session(apple_id: &str, session: &StoredAppleSession) -> Result<()> {
    if !keychain_enabled() {
        return Ok(());
    }
    let encoded = serde_json::to_string(session)?;
    store_secret(APPLE_SESSION_SERVICE, apple_id, &encoded)
}

fn load_user_session(apple_id: &str) -> Result<Option<StoredAppleSession>> {
    if !keychain_enabled() {
        return Ok(None);
    }
    let Some(encoded) = load_secret(APPLE_SESSION_SERVICE, apple_id)? else {
        return Ok(None);
    };
    let session = serde_json::from_str(&encoded)
        .with_context(|| format!("failed to parse the stored Apple session for `{apple_id}`"))?;
    Ok(Some(session))
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

fn delete_secret(service: &str, account: &str) -> Result<()> {
    if !keychain_enabled() {
        return Ok(());
    }
    let mut command = Command::new("security");
    command.args(["delete-generic-password", "-a", account, "-s", service]);
    let _ = command_output_allow_failure(&mut command)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        EnsureUserAuthRequest, UserAuth, persist_auth_selection_fields, request_matches_user,
    };

    #[test]
    fn request_match_allows_unspecified_fields() {
        let user = UserAuth {
            apple_id: "user@example.com".to_owned(),
            team_id: Some("TEAM123456".to_owned()),
            provider_id: Some("123456789".to_owned()),
            provider_name: Some("Example Team".to_owned()),
            last_validated_at_unix: Some(1),
        };

        assert!(request_matches_user(
            &EnsureUserAuthRequest {
                apple_id: Some("user@example.com".to_owned()),
                ..Default::default()
            },
            &user
        ));
        assert!(request_matches_user(
            &EnsureUserAuthRequest::default(),
            &user
        ));
    }

    #[test]
    fn request_match_rejects_mismatched_team_or_provider() {
        let user = UserAuth {
            apple_id: "user@example.com".to_owned(),
            team_id: Some("TEAM123456".to_owned()),
            provider_id: Some("123456789".to_owned()),
            provider_name: None,
            last_validated_at_unix: None,
        };

        assert!(!request_matches_user(
            &EnsureUserAuthRequest {
                team_id: Some("TEAM999999".to_owned()),
                ..Default::default()
            },
            &user
        ));
        assert!(!request_matches_user(
            &EnsureUserAuthRequest {
                provider_id: Some("987654321".to_owned()),
                ..Default::default()
            },
            &user
        ));
    }

    #[test]
    fn persisting_auth_selection_keeps_authoring_manifest_shape() {
        let temp = tempdir().unwrap();
        let manifest_path = temp.path().join("orbit.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "$schema": "../../schemas/apple-app.v1.json",
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

        let changed = persist_auth_selection_fields(
            &manifest_path,
            Some("TEAM123456"),
            Some("128120286"),
        )
        .unwrap();
        assert!(changed);

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest.get("$schema").and_then(|value| value.as_str()),
            Some("../../schemas/apple-app.v1.json")
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
}
