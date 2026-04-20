# Orbi App Updater Spec

Status: draft  
Owner: Orbi Updater
Last updated: 2026-04-02

## 1. Summary

Orbi should ship its own desktop app updater as a shared Rust-first system.

The updater must support:

- macOS direct-distributed apps
  - non-sandboxed
  - sandboxed direct-distributed, with a tiny private XPC installer wrapper only for the final apply step if required
- GTK4 Linux apps distributed as self-managed app directories
- WinUI 3 Windows apps distributed as non-Store `MSIX` / `MSIXBundle`

The updater must not own UI.

Instead:

- Rust exposes a very small programmatic API
- apps build their own UI on top of returned state and progress events
- Swift and C# get thin integration kits that only wrap Rust calls and platform-specific host requirements
- upgrades and downgrades share the same core release-install pipeline

This spec replaces the earlier CLI-specific updater design. The new target is a reusable desktop app updater stack, not `orbi self-update` for a single binary.

## 2. Goals

- One shared Rust updater core across macOS, Linux, and Windows.
- Fully programmatic integration.
- No required built-in menus, dialogs, windows, or system-style update UX.
- Small public Rust API.
- Thin Swift and C# wrappers over Rust.
- Support channels, phased rollouts, mirrors, release notes, delta updates, signing, downgrade/rollback, crash-safe resume, and progress reporting.
- Keep platform-specific code limited to installation and trust verification.
- For platform-managed installs, support native version checks and update handoff when available, plus external updater redirect or launch fallback.

## 3. Non-Goals

- No custom Mac App Store package install path.
- No custom Flatpak, Snap, AppImageUpdate, or distro package manager package install path in v1.
- No mandatory background daemon or always-on service.
- No cross-language business logic in Swift or C#.
- No requirement that the app itself be written in Rust.

Those platforms and package managers are externally managed and should stay externally managed. For Apple App Store, Microsoft Store, Flatpak, and Snap apps, Orbi provides companion integration features only, not custom package installation.

## 4. Supported Install Models

The updater supports two modes:

1. full self-managed update mode
2. limited platform-managed companion mode

### 4.1 macOS

Supported:

- direct-distributed `.app` bundle
- notarized and signed outside the Mac App Store
- app bundle installed in a normal filesystem location

Supported with extra constraints:

- sandboxed direct-distributed `.app`
- app must obtain write access to its install root through user-granted access stored as a security-scoped bookmark
- tiny private XPC installer wrapper may perform the final swap if the main process cannot

Platform-managed companion support:

- Mac App Store apps
- version check:
  - best-effort only
  - may use configured App Store lookup strategy or app-provided store-version endpoint
- update trigger:
  - product-page redirect only
  - no custom install path

### 4.2 Linux

Supported:

- GTK4 app installed as a self-managed app directory under a user or system prefix
- direct-distributed portable bundle extracted under a known install root

Platform-managed companion support:

- Flatpak apps
- passive version and update check through Flatpak update-monitor APIs when available
- native update trigger through Flatpak update-monitor APIs when allowed by the platform
- fallback to system tools when the update requires broader permissions than the currently installed app
- Snap apps
- pending-refresh awareness through `snapctl refresh --pending` when available in the snapped host environment
- user handoff to system tools or Snap Store when configured
- no Orbi-owned package apply path

Not supported in self-managed mode:

- distro-managed package installs

### 4.3 Windows

Supported:

- WinUI 3 app distributed as non-Store `MSIX` / `MSIXBundle`
- deployment driven through `App Installer` or Windows package deployment APIs
- per-user or per-machine direct distribution

Platform-managed companion support:

- Microsoft Store apps
- version check through Microsoft Store APIs when available
- update handoff through Microsoft Store APIs when available
- fallback product-page redirect

Not supported:

- unpackaged WinUI 3 installs

### 4.4 Support Levels

`SelfManagedFull`:

- full `check()`
- full `prepare()`
- full `install()`
- full `rollback()`

`PlatformManagedCompanion`:

- `check_managed_update()`
- `trigger_managed_update()` when the platform exposes it
- `open_managed_update_target()` when an external target is configured or derivable
- no custom package apply
- no Orbi-managed rollback

## 5. Product Shape

The public integration surface is one Rust crate:

- `orbi-updater`

Internal implementation is allowed to use a small workspace:

- `orbi-updater-core`
- `orbi-updater-platform`
- `orbi-updater-ffi`
- `orbi-updater-worker`
- `orbi-updater-macos-xpc`

But app developers should think of it as one updater product.

## 6. High-Level Architecture

The design splits into five layers:

1. Policy and protocol
2. Transport and storage
3. Package and delta handling
4. Platform installation backend
5. Host integration kits

### 6.1 Policy And Protocol

Shared across all platforms:

- update catalog selection
- channel selection
- rollout eligibility
- manifest signing
- artifact selection
- mirror failover
- release notes
- downgrade history metadata
- update journaling

### 6.2 Transport And Storage

Shared across all platforms:

- HTTPS fetch
- resumable downloads
- local cache
- state and journal files
- staged package storage

### 6.3 Package And Delta Handling

Shared across all platforms:

- full package handling
- delta package handling
- final package digest verification
- release artifact metadata parsing

### 6.4 Platform Installation Backend

Platform-specific:

- apply the platform-appropriate final install or package deployment operation
- verify platform code signing
- handle permissions and elevation
- relaunch

### 6.5 Host Integration Kits

Thin wrappers only:

- Swift package for macOS apps
- .NET package for WinUI 3 apps

They must not implement update policy.

## 7. Public Rust API

The Rust API must stay small and explicit.

### 7.1 Core Types

```rust
pub struct Updater;
pub struct UpdaterConfig;

pub enum UpdateCatalog {
    FullUpdateServer(FullUpdateServerCatalog),
    GitHubReleases(GitHubReleasesCatalog),
}

pub enum InstallManagement {
    SelfManaged(SelfManagedConfig),
    PlatformManaged(PlatformManagedConfig),
}

pub struct CheckResult {
    pub current: InstalledRelease,
    pub channel: ChannelName,
    pub available: Option<AvailableRelease>,
}

pub struct AvailableRelease {
    pub id: String,
    pub version: String,
    pub channel: ChannelName,
    pub title: String,
    pub published_at: String,
    pub release_notes: ReleaseNotes,
    pub rollout: RolloutDecision,
}

pub struct PreparedUpdate {
    pub release_id: String,
    pub version: String,
    pub package_path: PathBuf,
    pub install_plan: InstallPlan,
}

pub struct InstallResult {
    pub previous_version: String,
    pub current_version: String,
    pub restart_required: bool,
}

pub struct RollbackResult {
    pub previous_version: String,
    pub current_version: String,
    pub source: RollbackSource,
}

pub struct HistoricalRelease {
    pub id: String,
    pub version: String,
    pub channel: ChannelName,
    pub published_at: String,
    pub rollback_blocked: bool,
}

pub struct ManagedCheckResult {
    pub platform: ManagedPlatformKind,
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
    pub current_revision: Option<String>,
    pub latest_revision: Option<String>,
    pub update_available: Option<bool>,
    pub restart_required: Option<bool>,
    pub can_trigger_update: bool,
    pub can_open_target: bool,
    pub target_url: Option<String>,
}

pub enum ManagedPlatformKind {
    AppleAppStore,
    MicrosoftStore,
    Flatpak,
    Snap,
}

pub struct ManagedUpdateResult {
    pub platform: ManagedPlatformKind,
    pub action: ManagedUpdateAction,
    pub update_available: Option<bool>,
    pub target_url: Option<String>,
}

pub enum ManagedUpdateAction {
    TriggeredNativeUpdate,
    OpenedExternalTarget,
}
```

### 7.2 Catalog Configuration

`UpdaterConfig` must include:

- app id
- install identity and state root
- selected install management mode
- selected `UpdateCatalog` for self-managed mode
- selected channel for self-managed mode
- platform install backend configuration

Platform-managed config:

```rust
pub enum PlatformManagedConfig {
    AppleAppStore(AppleAppStoreConfig),
    MicrosoftStore(MicrosoftStoreConfig),
    Flatpak(FlatpakConfig),
    Snap(SnapConfig),
}

pub struct AppleAppStoreConfig {
    pub app_store_url: String,
    pub app_store_id: Option<String>,
    pub version_check: AppleStoreVersionCheck,
}

pub enum AppleStoreVersionCheck {
    Disabled,
    AppStoreLookupBestEffort,
    CustomEndpoint(String),
}

pub struct MicrosoftStoreConfig {
    pub product_id: String,
    pub store_url: Option<String>,
}

pub struct FlatpakConfig {
    pub app_id: Option<String>,
    pub software_center_uri: Option<String>,
}

pub struct SnapConfig {
    pub snap_name: Option<String>,
    pub snap_store_url: Option<String>,
}

pub struct ManagedCheckOptions;

pub struct ManagedUpdateOptions {
    pub allow_target_fallback: bool,
}
```

Catalog types:

```rust
pub struct FullUpdateServerCatalog {
    pub base_url: String,
}

pub struct GitHubReleasesCatalog {
    pub owner: String,
    pub repo: String,
    pub api_base_url: Option<String>,
    pub token: Option<String>,
}
```

Channel rules:

- `FullUpdateServer`
  - supports `stable`, `beta`, and custom channels
- `GitHubReleases`
  - supports only `release` and `prerelease`
- `PlatformManaged`
  - does not use Orbi update channels

### 7.3 Main Methods

```rust
impl Updater {
    pub fn new(config: UpdaterConfig) -> Result<Self>;

    pub fn state(&self) -> Result<UpdaterState>;

    pub fn check(&self, options: CheckOptions) -> Result<CheckResult>;

    pub fn list_release_history(
        &self,
        options: ReleaseHistoryOptions,
    ) -> Result<Vec<HistoricalRelease>>;

    pub fn check_managed_update(
        &self,
        options: ManagedCheckOptions,
    ) -> Result<ManagedCheckResult>;

    pub fn trigger_managed_update(
        &self,
        options: ManagedUpdateOptions,
        observer: &mut dyn ProgressObserver,
    ) -> Result<ManagedUpdateResult>;

    pub fn open_managed_update_target(&self) -> Result<()>;

    pub fn prepare(
        &self,
        release_id: &str,
        observer: &mut dyn ProgressObserver,
    ) -> Result<PreparedUpdate>;

    pub fn install(
        &self,
        prepared: PreparedUpdate,
        options: InstallOptions,
        observer: &mut dyn ProgressObserver,
    ) -> Result<InstallResult>;

    pub fn rollback(
        &self,
        options: RollbackOptions,
        observer: &mut dyn ProgressObserver,
    ) -> Result<RollbackResult>;

    pub fn resume_pending(
        &self,
        observer: &mut dyn ProgressObserver,
    ) -> Result<Option<ResumeOutcome>>;
}
```

Managed-companion method rules:

- `check()` and `list_release_history()` are for `SelfManaged` installs only
- `check_managed_update()`, `trigger_managed_update()`, and `open_managed_update_target()` are for `PlatformManaged` installs only
- `check_managed_update()` may return `update_available = None` when the platform cannot produce a reliable passive answer
- `trigger_managed_update()` must never start a custom package install path
- if a native companion-trigger path is unavailable and target fallback is allowed, `trigger_managed_update()` opens the external target and returns `ManagedUpdateAction::OpenedExternalTarget`

### 7.4 Simple Usage

```rust
let updater = Updater::new(config)?;
let check = updater.check(CheckOptions::default())?;

if let Some(release) = check.available {
    let prepared = updater.prepare(&release.id, &mut observer)?;
    let result = updater.install(prepared, InstallOptions::default(), &mut observer)?;
    println!("updated to {}", result.current_version);
}
```

### 7.5 Progress Model

Progress is event-based.

```rust
pub trait ProgressObserver {
    fn on_event(&mut self, event: ProgressEvent);
}
```

`ProgressEvent` must include:

- `Checking`
- `ResolvingCatalog`
- `CheckingManagedUpdate`
- `FetchingChannel`
- `ListingCatalogReleases`
- `FetchingRelease`
- `EvaluatingRollout`
- `SelectingArtifact`
- `Downloading`
- `ResumingDownload`
- `VerifyingManifestSignature`
- `VerifyingPackageDigest`
- `VerifyingPlatformSignature`
- `ApplyingDelta`
- `Staging`
- `WaitingForAppExit`
- `Installing`
- `WritingHistoryState`
- `Downgrading`
- `OpeningManagedUpdateTarget`
- `TriggeringManagedUpdate`
- `Relaunching`
- `Completed`
- `Failed`

Each event may include:

- phase name
- bytes downloaded
- total bytes if known
- package or release id
- human-readable detail string

### 7.6 User-Action Requirements

The public API must never open UI.

If the install needs host intervention, Rust returns a structured requirement:

- `InstallAccessRequired::MacOsInstallRootBookmark`
- `InstallAccessRequired::WritableInstallRoot`
- `InstallAccessRequired::Elevation`
- `InstallAccessRequired::AppRestartPermission`

The host app satisfies the requirement, updates config or install options, then retries.

## 8. Host Kits

### 8.1 Swift Kit

Ship a Swift package:

- `OrbiUpdaterKit`

Responsibilities:

- wrap FFI into Swift `async` and callback-friendly APIs
- convert progress callbacks into Swift structs
- expose helper utilities for:
  - obtaining and refreshing security-scoped bookmarks for the current app install root
  - launching the private macOS XPC installer path when the Rust core requests it

Non-goals:

- no SwiftUI views
- no menu items
- no release-note UI
- no policy logic

### 8.2 C# Kit

Ship a .NET package:

- `OrbiUpdaterKit.Windows`

Responsibilities:

- wrap FFI into `Task`-based APIs
- expose `IProgress<T>` friendly progress events
- provide small helpers for:
  - app restart coordination
  - optional elevated relaunch for apply if the install root is not writable

Non-goals:

- no WinUI controls
- no notification UI
- no rollout logic

## 9. FFI Boundary

The FFI boundary should be intentionally small and stable.

### 9.1 FFI Design

Use a C ABI with:

- opaque updater handle
- JSON request and response payloads for complex models
- callback-based progress events

Reason:

- keeps Swift and C# wrappers small
- avoids duplicating complex structs in two foreign-language bindings
- preserves a typed Rust API internally

### 9.2 FFI Scope

Expose only:

- create updater
- destroy updater
- get state
- check
- list release history
- check managed update
- trigger managed update
- open managed update target
- prepare
- install
- rollback
- resume pending

Do not expose low-level manifest parsing, signature verification, or installer internals as public FFI.

## 10. Update Catalogs

The updater supports exactly two catalog types:

1. `FullUpdateServer`
2. `GitHubReleases`

`FullUpdateServer` is the canonical and most capable catalog.

`GitHubReleases` is a constrained catalog mode with only two channels:

- `release`
- `prerelease`

Both catalog types still use Orbi-signed release manifests as the source of truth for:

- trust
- artifact metadata
- mirrors
- deltas
- rollout
- downgrade history behavior

### 10.1 Catalog Feature Matrix

`FullUpdateServer` supports:

- mutable channel pointers
- arbitrary named channels
- signed channel history indexes
- signed immutable release manifests
- full rollouts
- mirrors
- deltas
- downgrade

`GitHubReleases` supports:

- GitHub release list as the catalog index
- channels `release` and `prerelease` only
- signed per-release Orbi manifests attached as GitHub release assets
- full rollouts
- mirrors
- deltas
- downgrade

`GitHubReleases` does not support:

- arbitrary custom channel names
- a separate mutable channel pointer document

Platform-managed apps do not use Orbi update catalogs for install delivery.

Instead:

- Apple App Store mode uses configured App Store integration data
- Microsoft Store mode uses Microsoft Store APIs and configured product identity
- Flatpak mode uses Flatpak update-monitor APIs and configured external target data
- Snap mode uses snapd/snapctl companion data and configured external target data

### 10.2 Full Update Server Protocol

The full server uses a two-stage signed JSON protocol:

1. mutable channel pointer
2. immutable release manifest

### 10.2.1 Channel Pointer

Path:

```text
/v1/channels/{channel}.json
/v1/channels/{channel}.sig
```

Example:

```json
{
  "schema_version": 1,
  "app_id": "dev.orbi.example",
  "channel": "stable",
  "release_id": "2026.04.02+1.8.0",
  "version": "1.8.0",
  "history_url": "/v1/channels/stable-history.json",
  "rollout": {
    "kind": "percentage",
    "percentage": 100,
    "seed": "stable-1.8.0"
  },
  "published_at": "2026-04-02T09:00:00Z"
}
```

Purpose:

- cheap to fetch
- mutable
- channel-specific
- points to the current release and the fetchable release-history index

### 10.2.2 Channel History Index

Path:

```text
/v1/channels/{channel}-history.json
/v1/channels/{channel}-history.sig
```

Example:

```json
{
  "schema_version": 1,
  "app_id": "dev.orbi.example",
  "channel": "stable",
  "releases": [
    {
      "release_id": "2026.04.02+1.8.0",
      "version": "1.8.0",
      "published_at": "2026-04-02T09:00:00Z",
      "rollback_blocked": false
    },
    {
      "release_id": "2026.03.20+1.7.0",
      "version": "1.7.0",
      "published_at": "2026-03-20T09:00:00Z",
      "rollback_blocked": false
    }
  ]
}
```

Purpose:

- lets the host app show downgrade UI when it wants to
- keeps the main channel pointer small
- gives `rollback()` and downgrade flows a signed release-history source

### 10.2.3 Release Manifest

Path:

```text
/v1/releases/{release_id}.json
/v1/releases/{release_id}.sig
```

Example:

```json
{
  "schema_version": 1,
  "app_id": "dev.orbi.example",
  "release_id": "2026.04.02+1.8.0",
  "version": "1.8.0",
  "title": "Orbi 1.8",
  "published_at": "2026-04-02T09:00:00Z",
  "minimum_updater_version": "1.0.0",
  "notes": {
    "default_locale": "en",
    "entries": {
      "en": {
        "markdown": "## Fixes\n...",
        "url": "https://example.com/releases/1.8.0"
      }
    }
  },
  "artifacts": [
    {
      "artifact_id": "macos-aarch64-full",
      "platform": "macos",
      "target": "aarch64-apple-darwin",
      "kind": "app_bundle_full",
      "container": "tar.zst",
      "primary_url": "https://cdn.example.com/releases/1.8.0/macos-aarch64-full.tar.zst",
      "mirrors": [
        "https://mirror1.example.com/releases/1.8.0/macos-aarch64-full.tar.zst"
      ],
      "sha256": "HEX",
      "size": 123456789,
      "install_signer": {
        "platform": "macos",
        "team_id": "TEAMID1234"
      },
      "deltas": [
        {
          "from_version": "1.7.0",
          "kind": "file_tree_delta",
          "url": "https://cdn.example.com/releases/1.8.0/macos-aarch64-from-1.7.0.delta",
          "sha256": "HEX",
          "size": 3456789
        }
      ]
    }
  ],
  "rollback_blocked": false
}
```

### 10.3 GitHub Releases Catalog

The GitHub catalog uses the GitHub Releases API as the index, but it still requires Orbi-signed per-release metadata.

Selection rules:

- `release` channel
  - select from GitHub releases where:
    - `draft == false`
    - `prerelease == false`
- `prerelease` channel
  - select from GitHub releases where:
    - `draft == false`
    - `prerelease == true`

The updater must query releases newest-first and choose the newest eligible release after evaluating:

- manifest signature
- rollout eligibility
- platform artifact compatibility
- rollback blocking rules when relevant

Each GitHub release used by Orbi must contain:

- `orbi-release.json`
- `orbi-release.sig`

`orbi-release.json` uses the same schema as the full-server release manifest in section 10.2.3.

The GitHub release body and title may be used as fallback release-note text only if `orbi-release.json` omits notes.

Release history in GitHub mode is synthesized from the GitHub releases list filtered by channel and validated by parsing the attached Orbi release manifests.

GitHub API notes:

- token authentication is optional but recommended
- without a token, callers are subject to GitHub REST API unauthenticated rate limits
- the updater must not trust GitHub API metadata alone; signed Orbi release manifests remain mandatory

## 11. Signing And Trust Model

The updater must not trust transport alone.

Trust chain:

1. fetch signed channel pointer
2. verify signature
3. fetch signed release manifest
4. verify signature
5. choose artifact
6. download full or delta payload
7. verify payload digest
8. reconstruct staged install image
9. verify platform signer rules
10. apply install

### 11.1 Updater Manifest Signature

Use Ed25519 for channel and release manifest signatures.

Embedded in the client:

- current public key set
- key ids
- allowed signature algorithm

Reasons:

- compact
- easy key rotation
- independent from platform signing systems

### 11.2 Platform Signer Verification

In addition to Orbi manifest verification:

- macOS: require expected Apple code-signing team id
- Windows: require expected Authenticode signer thumbprint or subject match
- Linux: no universal OS signer requirement in v1; rely on Orbi manifest signing plus package digest, with optional app-defined signer verification hook

### 11.3 Key Rotation

The client embeds multiple accepted manifest public keys.

Rules:

- each `.sig` file includes `key_id`
- clients may trust multiple active keys
- a key may be removed only after all supported released clients trust the replacement key

## 12. Channel And Rollout Model

Supported channels depend on catalog type.

`FullUpdateServer`:

- `stable`
- `beta`
- custom named channels

`GitHubReleases`:

- `release`
- `prerelease`

Rollout eligibility is deterministic per install.

Input:

- `install_id`
- channel rollout seed

Formula:

```text
bucket = sha256("{install_id}:{seed}") % 100
eligible if bucket < percentage
```

Rules:

- rollout is evaluated after signature verification
- rollout affects availability, not trust
- apps may override the channel programmatically
- channel validation happens against the selected catalog type

## 13. Artifact And Package Model

The updater does not install raw binaries.

It installs complete application payloads.

### 13.1 Full Artifact Kinds

Supported full artifacts:

- `app_bundle_full`
  - macOS `.app` bundle packaged as `tar.zst`
- `app_dir_full`
  - Linux app directory packaged as `tar.zst`
- `msix_bundle`
  - Windows `MSIX` or `MSIXBundle`
- `appinstaller_manifest`
  - Windows `.appinstaller`

### 13.2 Delta Artifact Kinds

v1 delta kinds:

- `file_tree_delta`
  - macOS and Linux
- `package_native_delta`
  - Windows, when provided by the MSIX packaging toolchain and deployment path

Rules:

- Orbi-owned delta is file-level, not binary-patch-level
- file-tree deltas may add, replace, and delete files
- file-tree deltas reconstruct a complete staged target tree before platform verification
- Windows does not use Orbi-owned file-tree deltas
- if delta apply fails for any reason, retry with full artifact or platform-native full package install

Reason:

- easier to make reliable across app bundles and directory installs
- works with signed macOS app bundles because the final reconstructed tree is verified after assembly
- keeps Windows on the native MSIX deployment path

## 14. Mirrors

Each artifact may define:

- one primary URL
- zero or more mirror URLs

Mirror policy:

- try primary first
- fail over through mirrors in order
- mark mirror failures in local cache for a short cooldown window
- partial downloads may resume from the same URL if the server supports range requests

## 15. Local State

Per-install local updater state must include:

- `state.json`
- `journal.json`
- `history.json`
- cached manifests
- staged downloads

Minimum state fields:

```json
{
  "schema_version": 1,
  "install_id": "UUID",
  "channel": "stable",
  "current_version": "1.7.0",
  "last_checked_at": "2026-04-02T09:30:00Z",
  "last_seen_release_id": "2026.04.02+1.8.0"
}
```

### 15.1 Journal

`journal.json` tracks in-progress work so the updater can resume or clean up after crashes.

Phases:

- `downloaded`
- `delta_applied`
- `staged`
- `waiting_for_swap`
- `swapped`
- `history_ready`
- `downgrade_started`

`resume_pending()` must be able to:

- finish a safe pending install
- continue a pending downgrade
- recover journal state back to the last successfully committed release
- clean orphaned staging data

## 16. Downgrade And Rollback

Downgrade is mandatory on all supported platforms.

Rules:

- every release install is version-addressable
- the updater may install an older published release with the same `prepare()` + `install()` flow used for upgrades
- keep enough local metadata history to know the last successful release
- history data must include previous version, release id, source metadata, and digest or package identity information
- the updater must not keep retained local install images or package backups for downgrade
- rollback is a convenience operation meaning "downgrade to the last successful release"

Public API:

- `rollback()`

Behavior:

- downgrade to the last successful release
- resolve the target release from signed release history
- run the normal `prepare()` + `install()` flow for that older release
- write state and journal updates
- emit progress events

Platform details:

- macOS and Linux
  - always fetch and install the older published release through the normal flow
- Windows MSIX
  - always perform rollback as a package downgrade to an earlier published `MSIX` / `MSIXBundle`
  - requires deployment policy to allow downgrade, for example the equivalent of `ForceUpdateFromAnyVersion` when using the App Installer path
  - do not restore files from a private backup directory

Important:

- rollback of app bits does not imply rollback of user data
- apps with irreversible data migrations must be able to mark a release as `rollback_blocked`

## 17. Installation Ownership And External Management

The updater must explicitly detect external management and choose either refusal or platform companion behavior.

At minimum:

- macOS App Store
- Windows Store-managed installs
- Flatpak
- Snap

Optional detection:

- Homebrew cask
- distro package manager markers

If externally managed and no companion integration is configured:

- `check()` still works if configured
- `install()` returns `ExternallyManagedInstall`
- host app decides what UI to show

If the install is platform-managed and companion integration is configured:

- `install()` still returns `ExternallyManagedInstall`
- `rollback()` returns `RollbackUnavailable`
- `check_managed_update()` is available
- `trigger_managed_update()` is available when the platform exposes it
- `open_managed_update_target()` is available when an external target is configured or derivable

### 17.1 Platform-Managed Companion Semantics

Apple App Store mode:

- supports version checking only through configured best-effort lookup or an app-provided version endpoint
- `check_managed_update()` may return `update_available = None` when no passive source is configured or the passive source cannot be trusted
- `trigger_managed_update()` never performs a native install trigger
- the only allowed apply action is store-page redirect through `open_managed_update_target()` or target fallback inside `trigger_managed_update()`

Microsoft Store mode:

- supports passive version check and update discovery through `Windows.Services.Store.StoreContext` when the host environment exposes it
- supports native trigger through Microsoft Store update APIs for the current packaged app when available
- falls back to `open_managed_update_target()` when native trigger is unavailable or disabled by host policy
- reports platform-provided progress through the same `ProgressObserver` contract used by self-managed installs

Flatpak mode:

- supports passive version and update discovery through the Flatpak update-monitor APIs when the host environment exposes them
- treats Flatpak commit ids as the source of truth for installed and available revisions
- may surface `appdata-version` only as a display version string when available
- supports native trigger through `org.freedesktop.portal.Flatpak.UpdateMonitor.Update()` when the new update does not require broader permissions than the currently installed app
- if the update requires broader permissions and Flatpak rejects the in-app update request, Orbi must report native trigger unavailable and fall back to `open_managed_update_target()` when configured, otherwise return a structured error so the host can instruct the user to use system tools

Snap mode:

- supports pending-refresh detection through `snapctl refresh --pending` when available in the snapped host environment
- may surface pending `version`, `revision`, `channel`, and restart requirement when snapd exposes them
- should treat Snap `revision` as the source of truth for update state and use version strings only as display metadata
- does not promise a general native trigger path from the app itself
- may support privileged trigger paths only when the snap has the required `snap-refresh-control` capability and the host explicitly opts in
- otherwise `trigger_managed_update()` must fall back to `open_managed_update_target()` when configured, or return a structured error instructing the host to direct the user to system update tools

## 18. Platform Install Backends

### 18.1 macOS Backend

Name:

- `MacOsBundleInstaller`

Supported modes:

- non-sandboxed direct install
- sandboxed direct install with stored security-scoped bookmark

Responsibilities:

- stage new `.app` bundle in a temp directory
- verify code signing team id
- optionally verify Gatekeeper assessment if configured
- swap the bundle after the host app exits
- relaunch the app if requested

#### Non-sandboxed macOS

Preferred apply path:

- Rust stages bundle
- transient worker process performs final rename after main app exits

No XPC required.

#### Sandboxed macOS

Preferred apply path:

- host app obtains read-write access to the install root
- access is persisted as a security-scoped bookmark
- Rust prepares everything except the final swap
- tiny private XPC installer wrapper performs the final swap after app exit if in-process or generic worker install is not viable under sandbox constraints

Rules:

- the XPC wrapper must stay tiny
- no network logic in XPC
- no manifest parsing in XPC
- no rollout logic in XPC
- only final file operations, relaunch, and status reporting

### 18.2 Linux Backend

Name:

- `LinuxAppDirInstaller`

Supported mode:

- self-managed app directory

Responsibilities:

- stage full target app directory
- atomically replace install root using rename or directory switch strategy
- preserve launcher path
- relaunch if requested

Externally managed Linux installs must be refused.

### 18.3 Windows Backend

Name:

- `WindowsMsixInstaller`

Supported mode:

- non-Store `MSIX` / `MSIXBundle` WinUI 3 deployment

Responsibilities:

- select the target `MSIX` / `MSIXBundle` and `.appinstaller` deployment source
- verify Authenticode signer
- invoke Windows package deployment or App Installer update APIs
- support downgrade to an older published package version when policy allows it
- relaunch if requested

Because Windows owns the installed package, the backend must not replace app files directly.

If deployment policy requires elevation:

- return `InstallAccessRequired::Elevation`
- host may relaunch the install or deployment request elevated

## 19. Worker Process

The updater may use an internal transient worker process for final swap on:

- macOS non-sandboxed
- Linux

The worker is not a second public API surface.

Rules:

- launched only for final apply or downgrade apply
- no network access required
- consumes a signed and locally verified install plan
- exits when done

The macOS sandboxed path may use XPC instead of the generic worker for final swap. Windows MSIX does not use the generic worker swap path.

## 20. Release Notes

Release notes are data only.

The updater returns:

- title
- version
- publish time
- localized markdown
- external URL

The host app decides:

- whether to show notes
- how to render markdown
- whether to prefer inline text or web content

## 21. Install Options

`InstallOptions` must include:

- `allow_delta: bool`
- `allow_downgrade: bool`
- `relaunch: bool`
- `expected_install_root: Option<PathBuf>`
- `macos_security_scoped_bookmark: Option<Vec<u8>>`
- `windows_elevated_apply_token: Option<String>`

The public API should default to safe behavior:

- delta enabled
- downgrade enabled
- relaunch disabled unless requested

## 22. Error Model

The host app needs structured errors, not strings.

Core error classes:

- `ExternallyManagedInstall`
- `UnsupportedInstallLayout`
- `UnsupportedPlatform`
- `ManifestSignatureInvalid`
- `ManifestSchemaInvalid`
- `RolloutNotEligible`
- `NoCompatibleArtifact`
- `DownloadFailed`
- `DigestMismatch`
- `PlatformSignatureInvalid`
- `InstallAccessRequired`
- `InstallApplyFailed`
- `RollbackUnavailable`
- `DowngradeBlocked`
- `ManagedIntegrationUnavailable`
- `ManagedCheckUnavailable`
- `ManagedUpdateTriggerUnavailable`
- `ManagedTargetUnavailable`
- `ResumeFailed`

`InstallAccessRequired` carries one of the user-action requirements from section 7.5.

## 23. Concurrency And Locking

Only one updater operation may mutate a given install at a time.

Use a per-install lock file in updater state.

Rules:

- `check()` may run concurrently with other checks
- `prepare()`, `install()`, `rollback()`, and `resume_pending()` require the mutation lock

## 24. Security Rules

Mandatory rules:

- never install from an unsigned manifest
- never install without digest verification
- never skip platform signer verification on macOS or Windows
- never patch the live install tree directly
- always reconstruct and verify a staged target tree first
- always record downgrade history before final swap or platform deployment

## 25. Release Pipeline Requirements

Every published release must produce:

- signed release manifest
- full artifact per supported target
- optional delta artifacts from selected prior versions
- release notes payload

Additional requirements by catalog:

- `FullUpdateServer`
  - signed channel pointer
  - signed channel history index
- `GitHubReleases`
  - GitHub release with attached `orbi-release.json`
  - attached `orbi-release.sig`

### 25.1 macOS

Required:

- signed app bundle
- notarized app bundle
- packaged full artifact containing the `.app`

### 25.2 Linux

Required:

- packaged app directory artifact
- deterministic file manifest for digesting and delta generation

### 25.3 Windows

Required:

- signed `MSIX` / `MSIXBundle`
- `.appinstaller` when using the App Installer path
- published older package versions remain available when downgrade support is enabled

### 25.4 GitHub Catalog Publishing

When using `GitHubReleases`, each GitHub release must:

- be marked either release or prerelease
- include `orbi-release.json`
- include `orbi-release.sig`
- include all referenced artifacts or stable external URLs reachable from the manifest

If the app uses GitHub catalog mode, publishing must fail if:

- the release is draft
- the signed manifest asset is missing
- the release manifest references missing artifacts

### 25.5 Platform-Managed App Requirements

For Apple App Store mode:

- configure a stable product-page URL
- optionally configure an App Store identifier for best-effort lookup checks
- optionally configure a custom version endpoint when the app already has an authoritative backend
- never advertise programmatic install trigger support

For Microsoft Store mode:

- configure Store product identity
- support product-page redirect
- support version check and update handoff through Microsoft Store APIs when available in the host environment
- keep redirect fallback available even when native trigger is supported

For Flatpak mode:

- detect whether the app is running as a Flatpak and whether the update-monitor APIs are available
- configure an external target only when the host wants Orbi to open Software Center or another update surface
- treat commit ids as authoritative update identity
- treat `appdata-version` as display-only metadata

For Snap mode:

- detect whether the app is running as a Snap and whether `snapctl refresh --pending` is available
- configure an external target only when the host wants Orbi to open Snap Store or another update surface
- treat `revision` as authoritative update identity
- treat `version` as display-only metadata
- do not advertise native trigger support unless the snap explicitly has the required refresh-control capability and the host opted in

## 26. Testing

### 26.1 Unit Tests

Required coverage:

- manifest signature verification
- rollout bucketing
- mirror selection and failover
- delta planning and fallback
- journal recovery
- downgrade history metadata
- external-management detection

### 26.2 Integration Tests

Required mocked scenarios:

- update available
- no update available
- rollout not eligible
- primary mirror fails, fallback mirror succeeds
- delta succeeds
- delta fails, full package succeeds
- manifest signature invalid
- GitHub catalog selects the newest eligible non-draft release
- GitHub catalog channel filtering works for `release` and `prerelease`
- Apple App Store companion mode opens the configured store page
- Apple App Store companion mode best-effort version check parses configured lookup response
- Microsoft Store companion mode detects updates through Store APIs
- Microsoft Store companion mode triggers store update or reports redirect-only fallback
- Flatpak companion mode detects newer remote commit through update-monitor data
- Flatpak companion mode triggers native update when permissions are unchanged
- Flatpak companion mode reports fallback when the update requires broader permissions
- Snap companion mode detects pending refresh through `snapctl refresh --pending`
- Snap companion mode reports no native trigger by default and falls back to configured external target when available
- artifact digest mismatch
- macOS signer mismatch
- Windows signer mismatch
- rollback succeeds as downgrade to the previous successful release
- resume after interrupted staging succeeds

### 26.3 Platform Tests

Required platform-specific tests:

- macOS direct non-sandboxed swap
- macOS sandboxed path with bookmark + XPC wrapper
- Linux directory replacement
- Windows MSIX package deployment and downgrade path

## 27. Implementation Order

Recommended order:

1. Rust core types and signed manifest protocol
2. full artifact install path
3. downgrade history and journal
4. generic worker path
5. macOS non-sandboxed backend
6. Linux backend
7. Windows MSIX backend
8. delta support
9. Swift kit
10. C# kit
11. macOS sandboxed XPC wrapper

## 28. Open Decisions

These decisions still need explicit product agreement before implementation starts.

### 28.1 Windows Deployment Backend

Question:

- should Windows use `App Installer`
- direct `PackageManager` APIs
- or support both

Recommendation:

- support both
- make `PackageManager` the core backend
- optionally consume `.appinstaller` metadata when the app chooses that deployment model

Why:

- keeps Orbi in control of the public API and progress model
- avoids hard-binding the entire Windows story to one deployment entrypoint
- still allows `App Installer` features like update URIs and downgrade policy

### 28.2 macOS Sandboxed Install Contract

Question:

- what install roots are supported for sandboxed direct-distributed apps

Recommendation:

- support only install roots for which the host app can obtain and persist a read-write security-scoped bookmark
- require the host app to provide that bookmark before first install or first self-update
- refuse sandboxed apply without a valid bookmark

Why:

- keeps the updater deterministic
- avoids hidden install-location heuristics that break in production
- makes the tiny XPC layer stay tiny

### 28.3 Linux Install Scope

Question:

- should Linux support only user-owned install roots or also system prefixes

Recommendation:

- v1 supports user-owned self-managed install roots only
- defer system-prefix + elevation support to a later version

Why:

- cross-distro privilege elevation is messy
- the shared updater design is much simpler if Linux remains user-owned at first

### 28.4 Downgrade History Retention

Question:

- how many prior published releases must remain available

Recommendation:

- require at least the last `2` successful releases per channel to remain published
- allow apps to publish more history
- if a release is marked `rollback_blocked`, it stays in history but cannot be selected for `rollback()`

Why:

- enough headroom for one-step rollback and one additional safe target
- does not force indefinite artifact retention

### 28.5 Channel Crossing Rules

Question:

- can downgrade cross channels automatically

Recommendation:

- no automatic cross-channel downgrade
- `rollback()` stays within the current channel
- explicit downgrade UI may choose another channel only if the host app requests it

Why:

- keeps rollback predictable
- avoids surprising `beta -> stable` or `stable -> beta` policy jumps

### 28.6 Current Version Source Of Truth

Question:

- does Orbi read installed version from platform metadata or trust a host-provided version

Recommendation:

- platform metadata is the source of truth
- host-provided version may be used only as an optional hint for UI

Why:

- avoids drift between app code and installed package metadata
- required for reliable downgrade and recovery

### 28.7 Shutdown Behavior

Question:

- what happens if the app refuses to exit during install

Recommendation:

- updater never force-kills the app in v1
- return a structured error or requirement indicating that the host app must exit cleanly
- host-owned UI decides how to prompt the user

Why:

- avoids data-loss surprises
- keeps updater behavior safe and explainable

### 28.8 Relaunch Contract

Question:

- how should the updated app relaunch

Recommendation:

- host provides relaunch intent at install time
- default relaunch behavior is disabled
- platform backend relaunches only when explicitly requested

Why:

- lets the app decide whether restart is appropriate
- avoids reopening the app during scripted or headless flows

### 28.9 Windows Downgrade Policy

Question:

- should Orbi require downgrade support to be enabled for all Windows releases

Recommendation:

- yes for apps that expose `rollback()`
- publish-time validation should fail if Windows downgrade policy is disabled while rollback support is declared

Why:

- avoids a fake cross-platform rollback contract
- keeps Windows behavior aligned with macOS and Linux semantics

### 28.10 Linux Trust Extension

Question:

- should Linux support app-provided extra signer validation hooks

Recommendation:

- yes, but optional
- default trust remains Orbi manifest signature + artifact digest
- add a hook for apps that want vendor-specific extra verification

Why:

- Linux has no single universal app-signing rule
- some apps will want stricter local policy

### 28.11 Public API Shape

Question:

- should `prepare()` remain a first-class public step

Recommendation:

- expose both:
  - simple path: `install_release(release_id, ...)`
  - advanced path: `prepare()` then `install(prepared, ...)`

Why:

- simple apps should not need a two-step model
- advanced apps still need explicit staging for custom UI and preflight

### 28.12 Polling Model

Question:

- should v1 include automatic background polling

Recommendation:

- no autonomous background updater in v1
- support explicit checks and host-triggered periodic checks only

Why:

- keeps the core API smaller
- avoids hidden network behavior and scheduling complexity

### 28.13 Mandatory Updates

Question:

- should v1 support mandatory updates

Recommendation:

- no hard-block mandatory updates in v1
- allow manifests to mark releases as `critical`, but leave policy enforcement to the host app

Why:

- forced-update UX is product-specific
- different platforms and apps have very different tolerance for activation blocking

## 29. Decision

Build Orbi’s updater as:

- one small public Rust API
- one shared signed JSON update protocol
- one shared package and downgrade engine
- one platform backend per OS
- one tiny Swift kit
- one tiny C# kit
- one tiny private macOS XPC installer path only where sandboxed final apply requires it

Do not build platform UI into the updater.

Do not use Sparkle or any platform-managed updater as the core architecture.

## 30. External References

- Apple XPC services overview: <https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingXPCServices.html>
- Apple App Sandbox and security-scoped access overview: <https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox>
- Apple App Store Review Guidelines: <https://developer.apple.com/app-store/review/guidelines/>
- Apple App Store marketing tools and product links: <https://developer.apple.com/app-store/marketing/guidelines/>
- Apple iTunes Search API overview: <https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/iTuneSearchAPI/index.html>
- Microsoft WinUI 3 deployment overview: <https://learn.microsoft.com/windows/apps/windows-app-sdk/deploy-overview>
- Microsoft packaged deployment overview: <https://learn.microsoft.com/windows/apps/windows-app-sdk/deploy-packaged-apps>
- Microsoft App Installer auto-update and repair overview: <https://learn.microsoft.com/windows/msix/app-installer/auto-update-and-repair--overview>
- Microsoft Store package update APIs: <https://learn.microsoft.com/windows/uwp/packaging/self-install-package-updates>
- Microsoft Store URI launch syntax: <https://learn.microsoft.com/windows/apps/develop/launch/launch-store-app>
- Microsoft app update APIs for non-Store apps: <https://learn.microsoft.com/windows/msix/non-store-developer-updates>
- Microsoft Store product links: <https://learn.microsoft.com/windows/apps/publish/link-to-your-app>
- GitHub REST API releases: <https://docs.github.com/rest/releases/releases>
- GitHub REST API rate limits: <https://docs.github.com/rest/rate-limit>
- Flatpak documentation: <https://docs.flatpak.org/>
- Flatpak libflatpak and portal API reference: <https://docs.flatpak.org/en/latest/reference.html>
- Snap update management: <https://snapcraft.io/docs/how-to-guides/manage-snaps/manage-updates>
- Snap refresh awareness: <https://snapcraft.io/docs/refresh-awareness>
- Snap `snapctl` refresh control: <https://forum.snapcraft.io/t/using-the-snapctl-tool/15002>
