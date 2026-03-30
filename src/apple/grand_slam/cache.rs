use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{LiveLookupAuth, LiveProviderUploadAuth, XcodeNotaryAuth};
use crate::context::AppContext;
use crate::util::{read_json_file_if_exists, write_json_file};

const GRAND_SLAM_CACHE_SAFETY_WINDOW_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GrandSlamCacheState {
    xcode_notary_auth: Option<CachedXcodeNotaryAuth>,
    submit_auth: Vec<CachedSubmitAuth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedXcodeNotaryAuth {
    apple_id: String,
    expires_at_unix: u64,
    auth: XcodeNotaryAuth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CachedSubmitAuth {
    pub(super) apple_id: String,
    pub(super) team_id: Option<String>,
    pub(super) expires_at_unix: u64,
    pub(super) lookup: LiveLookupAuth,
    pub(super) upload: LiveProviderUploadAuth,
}

pub(super) fn cached_xcode_notary_auth(
    app: &AppContext,
    apple_id: &str,
) -> Result<Option<XcodeNotaryAuth>> {
    let state = load_grand_slam_cache_state(app)?;
    Ok(state
        .xcode_notary_auth
        .filter(|cached| {
            cached.apple_id == apple_id && grand_slam_cache_is_fresh(cached.expires_at_unix)
        })
        .map(|cached| cached.auth))
}

pub(super) fn store_cached_xcode_notary_auth(
    app: &AppContext,
    apple_id: &str,
    expires_at_unix: u64,
    auth: &XcodeNotaryAuth,
) -> Result<()> {
    let mut state = load_grand_slam_cache_state(app)?;
    state.xcode_notary_auth = Some(CachedXcodeNotaryAuth {
        apple_id: apple_id.to_owned(),
        expires_at_unix,
        auth: auth.clone(),
    });
    save_grand_slam_cache_state(app, &state)
}

pub(super) fn cached_submit_auth(
    app: &AppContext,
    apple_id: &str,
    team_id: Option<&str>,
) -> Result<Option<CachedSubmitAuth>> {
    let state = load_grand_slam_cache_state(app)?;
    Ok(state.submit_auth.into_iter().find(|cached| {
        cached.apple_id == apple_id
            && grand_slam_cache_is_fresh(cached.expires_at_unix)
            && cached.team_id.as_deref() == team_id
    }))
}

pub(super) fn store_cached_submit_auth(
    app: &AppContext,
    apple_id: &str,
    team_id: Option<&str>,
    expires_at_unix: u64,
    lookup: &LiveLookupAuth,
    upload: &LiveProviderUploadAuth,
) -> Result<()> {
    let mut state = load_grand_slam_cache_state(app)?;
    state.submit_auth.retain(|cached| {
        grand_slam_cache_is_fresh(cached.expires_at_unix)
            && !(cached.apple_id == apple_id && cached.team_id.as_deref() == team_id)
    });
    state.submit_auth.push(CachedSubmitAuth {
        apple_id: apple_id.to_owned(),
        team_id: team_id.map(ToOwned::to_owned),
        expires_at_unix,
        lookup: lookup.clone(),
        upload: upload.clone(),
    });
    save_grand_slam_cache_state(app, &state)
}

fn grand_slam_cache_path(app: &AppContext) -> PathBuf {
    app.global_paths.cache_dir.join("grand-slam-auth.json")
}

fn load_grand_slam_cache_state(app: &AppContext) -> Result<GrandSlamCacheState> {
    Ok(read_json_file_if_exists(&grand_slam_cache_path(app))?.unwrap_or_default())
}

fn save_grand_slam_cache_state(app: &AppContext, state: &GrandSlamCacheState) -> Result<()> {
    write_json_file(&grand_slam_cache_path(app), state)
}

fn grand_slam_cache_is_fresh(expires_at_unix: u64) -> bool {
    expires_at_unix > current_unix_time().saturating_add(GRAND_SLAM_CACHE_SAFETY_WINDOW_SECS)
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
