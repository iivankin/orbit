# Contributing

## Development checks

Run the standard local checks before opening a change:

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

CI runs the same checks in [`.github/workflows/ci.yml`](/Users/ilyai/Developer/personal/orbit2/.github/workflows/ci.yml).

## Test layers

Orbit uses three test layers:

1. Unit tests inside `src/`
2. Mocked integration/e2e tests in `tests/`
3. Manual live Apple-account e2e tests in [`tests/e2e_live_apple.rs`](/Users/ilyai/Developer/personal/orbit2/tests/e2e_live_apple.rs)

The live Apple-account tests are intentionally `#[ignore]` and are never meant to run in CI.

## Running mocked e2e tests

Mocked e2e coverage is included in normal `cargo test`.

If you want to run only the integration suite:

```bash
cargo test --test e2e_run
cargo test --test e2e_signing
cargo test --test e2e_submit
cargo test --test e2e_lifecycle
```

## Running live Apple-account tests

These tests create real Apple Developer resources. Use a disposable namespace and a real Apple account you control.

The live suite is Apple ID only. The helpers explicitly clear `ORBIT_ASC_*` variables and run each test with isolated `ORBIT_DATA_DIR` / `ORBIT_CACHE_DIR` paths so a previously cached API key cannot leak into the test process.

Required environment:

```bash
export ORBIT_APPLE_ID=you@example.com
export ORBIT_APPLE_TEAM_ID=...
```

Optional:

```bash
export ORBIT_APPLE_PROVIDER_ID=...
export ORBIT_LIVE_TEST_BUNDLE_PREFIX=dev.orbit.livee2e
```

Orbit now authenticates through GrandSlam/AuthKit and Developer Services. In practice that means:

1. Orbit needs a saved Apple ID identity plus a password in Keychain or `ORBIT_APPLE_PASSWORD`
2. On the first interactive auth challenge, complete Apple's verification flow once and let Orbit persist the credentials
3. Normal build/sign/submit/notary commands should then reuse fresh derived auth material silently

The live tests intentionally run without `ORBIT_ASC_*` credentials. They are exercising the Apple ID path, not the public ASC API key path.

### Build / sign / provision / clean

Runs a real iOS App Store build, signs it, creates a provisioning profile, then runs `orbit clean --all`.

What it verifies:

- GrandSlam/AuthKit login works
- Developer Services bundle/profile/certificate provisioning works
- the artifact is built and receipt is written
- `orbit clean --all` removes Orbit-managed remote resources
- `orbit clean --all` removes local `.orbit` state without revoking remote signing certificates

```bash
ORBIT_RUN_LIVE_APPLE_E2E=1 \
cargo test --test e2e_live_apple live_build_sign_provision_and_clean_remote_state -- --ignored --nocapture
```

### Entitlements change

Builds once with `Associated Domains`, updates `orbit.json`, then builds again without the entitlement.

What it verifies:

- the first build enables `ASSOCIATED_DOMAINS` remotely
- the second build succeeds after the local entitlement is removed

This test does not require the remote capability to be force-disabled. Current Xcode behavior removes the local entitlement but does not emit a matching Developer Services disable mutation for `Associated Domains`.

```bash
ORBIT_RUN_LIVE_APPLE_E2E=1 \
cargo test --test e2e_live_apple live_entitlements_change_updates_remote_capabilities -- --ignored --nocapture
```

### Push notifications capability

Builds an iOS app with `entitlements.push_notifications` and verifies that the remote bundle ID has `PUSH_NOTIFICATIONS` enabled.

```bash
ORBIT_RUN_LIVE_APPLE_E2E=1 \
cargo test --test e2e_live_apple live_push_notifications_capability_syncs_to_bundle_id -- --ignored --nocapture
```

### macOS + Developer ID

Builds a real macOS Developer ID artifact and submits it through Orbit's Xcode-like notarization path.

What it verifies:

- Developer ID signing works through the Apple ID auth path
- notarization submission can be created with the current account
- post-build verification checks `codesign`, `pkgutil`, and Gatekeeper on the produced package
- after an accepted notarization, Orbit validates the stapled package again

If the team is not configured for notarization, Apple will reject the submission with status code `7000`.

```bash
ORBIT_RUN_LIVE_APPLE_E2E=1 \
cargo test --test e2e_live_apple live_macos_developer_id_build_and_submit -- --ignored --nocapture
```

This scenario is only compiled on macOS hosts.

### Submit

Builds and uploads a real App Store build. This is intentionally separate from the cleanup scenario.

What it verifies:

- App Store Connect app-record bootstrap works
- content-delivery upload auth works
- the build upload is accepted by Apple

```bash
ORBIT_RUN_LIVE_APPLE_SUBMIT_E2E=1 \
cargo test --test e2e_live_apple live_submit_uses_real_app_store_connect_account -- --ignored --nocapture
```

## Cleanup expectations

Live tests use a best-effort cleanup guard:

- most live tests attempt `orbit clean --all` on drop
- the submit test only runs `orbit clean --local`

This split is intentional. After a real submit, Apple may keep the App Store Connect app record or explicit App ID, so full remote rollback is not always possible.

`orbit clean --all` is intentionally conservative:

- it removes Orbit-managed profiles, bundle IDs, app groups, merchant IDs, and iCloud containers
- it removes local signing material from `.orbit`
- it does not revoke remote signing certificates
