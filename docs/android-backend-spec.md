# Orbi Android Backend Spec

Date: April 1, 2026
Status: Draft for implementation
Owner: Orbi core

## Goal

Implement an Android backend for Orbi that matches the current product shape of the Apple backend:

- app-centric manifest, not a Gradle project
- local-first build, run, test, sign, and submit flows
- reproducible dependency resolution with a lockfile
- receipts for later submit/re-run operations
- no Android Studio or Gradle requirement for normal Orbi usage

The backend should feel like "the Android version of Orbi", not "Orbi shells out to Gradle".

## Repo Reality

Current Orbi is an Apple-only backend with these product surfaces:

- backend dispatch from manifest schema
- `orbi init`
- `orbi lint`
- `orbi format`
- `orbi test`
- `orbi ui ...`
- `orbi deps update`
- `orbi ide ...`
- `orbi bsp`
- `orbi run`
- `orbi build`
- `orbi submit`
- `orbi clean`
- backend-specific admin utilities under `orbi apple ...`

There is already an architectural seam for Android:

- [`src/manifest.rs`](/Users/ilyai/Developer/personal/orbi2/src/manifest.rs) defines `ManifestBackend::Android`
- [`src/commands/mod.rs`](/Users/ilyai/Developer/personal/orbi2/src/commands/mod.rs) dispatches by manifest backend

This spec uses that seam and mirrors the Apple module layout with a new `src/android/` tree.

## Product Decisions

### 1. Native backend, not Gradle orchestration

Orbi Android will use official Android command-line primitives directly:

- `sdkmanager`
- `avdmanager`
- `emulator`
- `adb`
- `logcat`
- `aapt2`
- `d8`
- `zipalign`
- `apksigner`
- `bundletool`
- `jarsigner`

It will not require a generated Gradle wrapper for build, run, test, or submit.

### 2. Modern-only baseline

V1 supports modern Android apps only:

- minimum supported `min_sdk`: 26
- default `compile_sdk`: 36
- default `target_sdk`: 35

Rationale:

- avoids legacy multidex and most desugaring complexity
- matches Orbi’s existing modern-platform bias on Apple
- stays Play-compliant as of April 1, 2026

Play policy fact to encode in validation:

- since August 31, 2025, new mobile app submissions and updates must target Android 15 / API 35 or higher
- existing mobile apps must target at least API 34 to remain discoverable to new users on newer Android versions

### 3. Kotlin-first backend

V1 app sources are Kotlin-only.

This is intentional:

- Jetpack Compose is Android’s recommended modern UI toolkit
- Compose app templates are Kotlin-only
- it avoids the mixed Kotlin/Java command-line compilation edge cases in the first release

Phase 2 can add Java sources. KAPT, KSP, Data Binding, View Binding, and NDK/CMake are out of scope for V1.

### 4. No embedded Android equivalent for `watch` or `app_clip`

Apple concepts that do not map cleanly should not be forced into fake parity:

- `watch`: no embedded companion block in V1
- `app_clip`: no Android equivalent in V1
- `extensions`: no dedicated top-level block in V1

Android replacements:

- reusable modular delivery is modeled with Play Feature Delivery feature modules
- Wear OS is a separate future backend/form-factor flow, not an embedded child bundle
- manifest-declared components like services, receivers, providers, widgets, and aliases stay in AndroidManifest authoring instead of a special Orbi DSL

Google Play tracks already use form-factor-prefixed track names like `wear:production`. That is a separate distribution concern, not an embedded app packaging model.

### 5. No remote destructive cleanup

`orbi clean` for Android only removes local Orbi state and local signing material.

It must not delete:

- Play Console apps
- Play tracks/releases
- service accounts
- Google Cloud projects
- Firebase projects

There is no safe Android equivalent to Apple’s remote cleanup behavior.

## External Facts We Are Designing Around

As of research date:

- Android SDK Platform 36 is stable and available via SDK Manager.
- `sdkmanager` installs SDK packages and licenses.
- `avdmanager` creates AVDs and `emulator` starts them from the command line.
- `adb` is the standard install/run/shell bridge and `logcat` is the standard log stream.
- `aapt2` is the standalone resource compiler/packager used by Android Studio and AGP.
- `d8` is the standalone dexer.
- `bundletool` is the underlying tool used by Android Studio, AGP, and Google Play for app bundles and APK sets.
- AABs must be signed with `jarsigner`, not `apksigner`.
- APKs should be `zipalign`ed before `apksigner`.
- Play App Signing is mandatory for new Play apps.
- Play submit flows are modeled through the Edits API: create edit, upload bundle, update track, commit.
- Internal app sharing has dedicated upload endpoints for bundles and APKs.
- Dynamic feature delivery is the correct Android modular delivery model.
- Local unit tests run on the local JVM.
- Instrumented tests run on devices or emulators.
- UI Automator is the correct framework for device-level UI automation.

## Manifest Schema

Introduce `schemas/android-app.v1.json`.

Schema URL:

- `https://orbi.dev/schemas/android-app.v1.json`

Schema filename:

- `android-app.v1.json`

### Top-level shape

```json
{
  "$schema": "/Users/you/.orbi/schemas/android-app.v1.json",
  "name": "ExampleApp",
  "display_name": "Example",
  "bundle_id": "dev.orbi.example",
  "namespace": "dev.orbi.example",
  "version": "1.0.0",
  "build": 1,
  "platforms": {
    "android": {
      "min_sdk": 26,
      "target_sdk": 35,
      "compile_sdk": 36
    }
  },
  "sources": ["Sources/App"],
  "resources": ["Resources"],
  "assets": ["Assets"],
  "manifest": {
    "path": "Manifest/AndroidManifest.xml",
    "application": {
      "icon": "@mipmap/ic_launcher",
      "round_icon": "@mipmap/ic_launcher_round",
      "theme": "@style/Theme.Example"
    }
  },
  "entry": {
    "launcher_activity": "dev.orbi.example.MainActivity"
  },
  "dependencies": {
    "androidx-core-ktx": {
      "maven": "androidx.core:core-ktx",
      "version": "1.18.0",
      "repository": "google"
    },
    "activity-compose": {
      "maven": "androidx.activity:activity-compose",
      "version": "1.12.4",
      "repository": "google"
    },
    "vendor-sdk": {
      "aar": "Vendor/VendorSdk.aar"
    }
  },
  "signing": {
    "release": {
      "keystore": "Signing/release.jks",
      "alias": "release"
    }
  },
  "hooks": {
    "before_build": ["./scripts/generate-assets.sh"]
  },
  "tests": {
    "unit": {
      "format": "junit4",
      "sources": ["Tests/Unit"]
    },
    "ui": {
      "format": "maestro",
      "sources": ["Tests/UI"]
    }
  },
  "quality": {
    "format": {
      "tool": "ktfmt",
      "ignore": ["build/**"]
    }
  }
}
```

### Field decisions

- Reuse `bundle_id`, `version`, and `build`.
  - `bundle_id` maps to Android `applicationId`
  - `version` maps to `versionName`
  - `build` maps to `versionCode`
- `namespace` is optional and defaults to `bundle_id`
- `platforms.android` is an object, not a string
- `sources`, `resources`, `assets` stay path arrays
- `manifest.path` is optional
- `manifest.application` is a small structured overlay for common app-level attributes
- `entry.launcher_activity` is required unless the manifest path already declares a single launcher activity and Orbi can resolve it unambiguously

### Deliberate omissions in V1

No V1 support for:

- product flavors
- build variants beyond debug/release
- KAPT/KSP
- NDK/CMake
- data binding / view binding codegen
- app widgets DSL
- raw arbitrary XML snippets inside JSON
- Play Asset Delivery
- Wear OS child packaging

### Feature module schema

Reserve this for Phase 2:

```json
"features": {
  "sell": {
    "delivery": "on-demand",
    "instant": false,
    "sources": ["Features/Sell"],
    "resources": ["Features/SellResources"],
    "manifest": {
      "path": "Features/Sell/AndroidManifest.xml"
    },
    "dependencies": {}
  }
}
```

Supported delivery values:

- `install-time`
- `on-demand`
- `conditional`

Conditional delivery supports:

- `min_sdk`
- `locales`
- `device_features`
- `countries` is out of scope for V1

## Dependency Model

Android needs a real repository-driven dependency system.

### Supported dependency sources in V1

`dependencies` is a dictionary keyed by Orbi-local dependency name.

Each value is exactly one of:

- Maven artifact
- local `.aar`
- local `.jar`

Examples:

```json
"dependencies": {
  "core-ktx": {
    "maven": "androidx.core:core-ktx",
    "version": "1.18.0",
    "repository": "google"
  },
  "okio": {
    "maven": "com.squareup.okio:okio",
    "version": "3.16.1",
    "repository": "maven-central"
  },
  "vendor-sdk": {
    "aar": "Vendor/VendorSdk.aar"
  }
}
```

### Repository support

Default repository set:

- Google Maven
- Maven Central

Optional custom repository support:

```json
{
  "maven": "com.example:private-sdk",
  "version": "2.4.1",
  "repository": {
    "url": "https://maven.example.com/releases"
  }
}
```

No dynamic versions:

- reject `+`
- reject ranges
- require exact versions

### Lockfile

Reuse `.orbi/orbi.lock`.

For Android, it records:

- resolved direct dependency coordinates
- resolved transitive dependency graph
- repository source for each artifact
- artifact SHA-256
- POM SHA-256
- packaging type (`aar`, `jar`, `pom`)

`orbi deps update` behavior:

- query repository metadata
- rewrite exact version in `orbi.json`
- rewrite resolved graph in `.orbi/orbi.lock`
- default update policy: newest stable within same major

### Why no direct AAR-only ecosystem

AARs do not carry enough identity/version/dependency context by themselves. Repository-backed Maven resolution is the default path. Direct `.aar` and `.jar` remain escape hatches.

## Build Profiles And Outputs

### Android distributions

Extend backend validation to accept these Android distribution kinds:

- `development`
- `sideload`
- `play`

Meaning:

- `development`
  - debug APK
  - Orbi-managed debug signing
  - installable
  - not submit-eligible
- `sideload`
  - release universal APK
  - release keystore signing
  - installable
  - not Play-submit-eligible
- `play`
  - release AAB
  - upload-key signing
  - submit-eligible

Validation:

- `play` requires `--release`
- `play` rejects `--simulator` and `--device`
- `sideload` should default to release even if `--release` is omitted, but the CLI should still encourage explicit `--release`

### Artifact outputs

Default artifact paths:

- debug APK: `.orbi/artifacts/<id>-android-development-debug.apk`
- release APK: `.orbi/artifacts/<id>-android-sideload-release.apk`
- Play bundle: `.orbi/artifacts/<id>-android-play-release.aab`

Receipts live in `.orbi/receipts/`.

### Build receipts

Add Android receipt type:

```json
{
  "id": "20260401-123456",
  "target": "ExampleApp",
  "platform": "android",
  "configuration": "release",
  "distribution": "play",
  "destination": "bundle",
  "application_id": "dev.orbi.example",
  "artifact_type": "aab",
  "artifact_path": "/abs/path/.orbi/artifacts/ExampleApp.aab",
  "merged_manifest_path": "/abs/path/.orbi/build/android/release/merged/AndroidManifest.xml",
  "created_at_unix": 1775000000,
  "submit_eligible": true
}
```

Do not force Apple and Android into the same backend-specific receipt payload.

Implementation recommendation:

- keep a shared receipt envelope
- add backend-specific payload under `metadata`

## Build Pipeline

### Toolchain inputs

Required external tooling:

- JDK 17+
- Android SDK root
- platform-tools
- command-line tools
- build-tools matching selected `compile_sdk`
- `platforms;android-<compile_sdk>`

Orbi-managed cached tools:

- `bundletool`
- `ktfmt`

### Direct APK pipeline

For `development` and `sideload`:

1. Resolve manifest and lockfile.
2. Resolve Android SDK:
   - `android.jar`
   - build-tools path
3. Resolve dependencies:
   - download POMs and artifacts
   - unpack AARs
4. Merge manifests:
   - app manifest path, if present
   - generated Orbi overlays
   - library manifests
5. Compile resources with `aapt2 compile`.
6. Link resources with `aapt2 link`:
   - generate resource table
   - generate `R.java`
   - generate `proguard.txt` if present
   - produce unsigned resource APK shell
7. Compile Kotlin app sources plus generated sources to `.class`.
8. Convert classes and dependency bytecode to DEX with `d8`.
9. Assemble unsigned APK:
   - base resource APK
   - `classes.dex`
   - assets
   - native libs from AARs
10. `zipalign`
11. `apksigner sign`
12. `apksigner verify`
13. write receipt

### AAB pipeline

For `play`:

1. Perform steps 1 through 8 from the APK pipeline.
2. Materialize base module bundle directory:
   - `base/manifest/AndroidManifest.xml`
   - `base/dex/*.dex`
   - `base/res/*`
   - `base/lib/*`
   - `base/assets/*`
   - `BundleConfig.pb`
3. If feature modules are enabled in a later phase, emit one top-level module directory per feature.
4. Use `bundletool build-bundle` from precompiled modules.
5. Sign the resulting `.aab` with `jarsigner`.
6. Optionally verify by attempting a local `bundletool build-apks` dry run.
7. write receipt

### Resource and manifest policy

V1 app resources and library resources must use standard Android merge behavior as closely as practical.

Required merge rules:

- app manifest has highest priority
- library manifests merge into app manifest
- conflict errors on incompatible attributes
- support `tools:replace`, `tools:node`, and `tools:remove`
- preserve `<intent-filter>` uniqueness semantics

If implementing a full merge-rule engine is too large for Phase 1, reduce scope to:

- merge library manifests with documented priority rules
- error when a `tools:` marker is encountered
- clearly document that limitation

### Compose support

V1 should support Compose app sources.

Required behavior:

- Kotlin 2.x only
- Orbi injects the Compose compiler plugin when `build_features.compose` or Compose dependencies are detected
- template apps are Compose-based

This backend should not depend on Gradle’s Compose plugin. Orbi must wire the compiler plugin directly into `kotlinc`.

## Runtime

### `orbi run --platform android --simulator`

Interpret existing `--simulator` as Android emulator for now.

Flow:

1. build debug APK
2. locate running emulator or boot one
3. `adb install -r`
4. launch resolved activity via `adb shell am start -n <applicationId>/<activity>`
5. stream `logcat`

### `orbi run --platform android --device`

Flow:

1. build debug APK
2. resolve physical device from `adb devices`
3. `adb install -r`
4. launch activity
5. stream `logcat`

### `--debug`

V1 behavior:

- launch with `am start -D`
- detect the target PID
- `adb forward tcp:<local> jdwp:<pid>`
- print attach instructions and forwarded port

No IDE-specific debugger integration in V1.

## Testing

### Unit tests

`orbi test` on Android means local JVM unit tests.

Implementation:

- compile app Kotlin sources against `android.jar`
- compile test sources against app outputs, JUnit 4, and Kotlin test runtime
- run locally on the host JVM

Constraints:

- tests must not rely on real Android framework behavior
- V1 ships JUnit 4 only

### UI tests

`orbi test --ui --platform android` uses a generated instrumentation harness, not shell-scripted taps.

Implementation:

1. build/install debug app
2. generate Orbi UI test APK containing:
   - AndroidJUnitRunner
   - UI Automator runtime
   - Orbi YAML flow executor
3. install test APK
4. run instrumentation with `adb shell am instrument -w`
5. collect artifacts

Artifact capture:

- screenshots
- per-flow JSON report
- optional MP4 via host-managed `adb shell screenrecord`
- test logs

### YAML command set

Re-use the existing Maestro-style subset where practical.

Android V1 command support:

- `launchApp`
- `stopApp`
- `clearState`
- `tapOn`
- `doubleTapOn`
- `longPressOn`
- `swipe`
- `scroll`
- `scrollUntilVisible`
- `inputText`
- `pressKey`
- `hideKeyboard`
- `assertVisible`
- `assertNotVisible`
- `takeScreenshot`
- `startRecording`
- `stopRecording`
- `openLink`
- `runFlow`
- `repeat`
- `retry`

Commands intentionally deferred:

- location spoofing
- contacts overwrite
- crash-log management
- dylib install equivalent
- profiler/instruments equivalent

### UI inspection commands

Support in V1:

- `orbi ui dump-tree --platform android`
- `orbi ui describe-point --platform android --x ... --y ...`
- `orbi ui logs --platform android`
- `orbi ui open --platform android URL`
- `orbi ui add-media --platform android`
- `orbi ui doctor --platform android`

Backend details:

- dump tree and point hit-testing use the instrumentation harness plus UI Automator accessibility tree access
- logs use `adb logcat`
- open uses `adb shell am start -a android.intent.action.VIEW -d`
- add-media uses `adb push` plus media rescan

## Signing

### Debug signing

Orbi manages a debug keystore under Orbi local data.

### Release signing

V1 supports a single release keystore per app.

Manifest shape:

```json
"signing": {
  "release": {
    "keystore": "Signing/release.jks",
    "alias": "release"
  }
}
```

Passwords:

- never stored in `orbi.json`
- loaded from env or Orbi local encrypted store

Environment overrides:

- `ORBI_ANDROID_KEYSTORE_PASSWORD`
- `ORBI_ANDROID_KEY_PASSWORD`

### Signing utilities

Introduce:

- `orbi android signing export`
- `orbi android signing import`

These export/import:

- keystore file
- alias metadata
- non-secret manifest snippet

They do not print secrets.

## Submit

### Authentication

Use Google Play Developer API service accounts.

V1 auth sources:

- `ORBI_PLAY_SERVICE_ACCOUNT_PATH`
- later: Orbi-managed imported credentials

### Play submit

`orbi submit --platform android` from a `play` receipt:

1. create edit
2. upload bundle with `edits.bundles.upload`
3. update target track with `edits.tracks.update`
4. commit edit
5. if `--wait`, poll until processing state is stable enough to report success or a blocking failure

Add Android-specific submit flags:

- `--track <name>`
- `--rollout <0.0-1.0>`
- `--draft`
- `--release-name <string>`
- `--internal-sharing`

Track defaults:

- if omitted, default to `qa`

Supported track values:

- `qa`
- `beta`
- `production`
- arbitrary closed testing track name

Future form-factor tracks use:

- `wear:production`
- `tv:qa`
- `automotive:beta`

### Internal app sharing

`orbi submit --platform android --internal-sharing`

Behavior:

- accepts APK or AAB receipts
- uploads with internal app sharing endpoint
- prints returned download URL

### Submit validation

Before Play submit, Orbi must block on:

- `target_sdk` below current Play minimum
- unsigned or invalid AAB
- missing service account credentials
- missing release/upload keystore

## Quality

### Format

V1 supports:

- `orbi format`
- `orbi format --write`

Formatter:

- `ktfmt`

Coverage:

- `*.kt`
- `*.kts`

### Lint

Android lint parity is Phase 2.

Reason:

- good Android lint support requires a stable custom project model for merged manifests, resources, bytecode, and libraries

Until then:

- `orbi lint` on Android should fail with a clear "not implemented yet" message
- do not ship a fake lint mode that only checks formatting

## IDE And BSP

### V1

Support:

- `orbi ide dump-args --platform android`

It returns:

- `kotlinc` args
- `aapt2` args
- `d8` args
- source roots
- generated source roots
- classpath

### Phase 2

Add:

- `orbi ide install-build-server`
- Android BSP server

Languages:

- `kotlin`
- `java`

Do not block V1 on BSP.

## Clean

`orbi clean --local`:

- remove `.orbi`
- remove Orbi-managed debug keystore
- optionally remove cached build intermediates for this manifest

`orbi clean --all` on Android is equivalent to local cleanup only.

There is no Android remote cleanup mode in V1.

## Init Templates

Add Android to `orbi init`.

V1 Android templates:

- `Android app`
  - single-target Compose app
  - Kotlin only
  - launcher activity entry
  - resources/assets layout
  - sample unit and UI test roots

Suggested scaffold:

```text
Manifest/
  AndroidManifest.xml
Resources/
  values/
    strings.xml
  drawable/
  mipmap/
Sources/
  App/
    MainActivity.kt
    App.kt
Tests/
  Unit/
  UI/
orbi.json
```

Defaults written by `init` as of April 1, 2026:

- `compile_sdk: 36`
- `target_sdk: 35`
- `min_sdk: 26`

## Code Layout In This Repo

Add:

```text
src/android/mod.rs
src/android/manifest/{mod.rs,authoring.rs,normalize.rs}
src/android/deps.rs
src/android/build/{mod.rs,toolchain.rs,pipeline.rs,receipt.rs,signing.rs}
src/android/testing/{mod.rs,unit.rs,ui.rs}
src/android/submit/{mod.rs,play_api.rs}
src/android/runtime.rs
src/android/ui.rs
src/android/quality.rs
src/android/clean.rs
schemas/android-app.v1.json
examples/android-compose-app/...
tests/e2e_android_*.rs
```

Top-level plumbing changes:

- [`src/manifest.rs`](/Users/ilyai/Developer/personal/orbi2/src/manifest.rs): add Android schema detection and loading
- [`src/commands/mod.rs`](/Users/ilyai/Developer/personal/orbi2/src/commands/mod.rs): dispatch Android backend
- [`src/cli.rs`](/Users/ilyai/Developer/personal/orbi2/src/cli.rs): add Android platform and Android submit/signing flags
- [`src/commands/init.rs`](/Users/ilyai/Developer/personal/orbi2/src/commands/init.rs): add Android ecosystem/template

## Delivery Plan

### Phase 1: Useful core backend

- schema detection and project loading
- Android init template
- Maven/AAR/JAR dependency resolution and lockfile
- direct APK build for debug and sideload release
- AAB build for Play
- run on emulator/device
- logs, open URL, add media
- local JVM unit tests
- instrumentation UI tests with UI Automator harness
- signing import/export
- Play submit and internal app sharing
- local clean
- `ide dump-args`
- `format` with ktfmt

### Phase 2: Deeper parity

- Android lint integration
- Java source support
- feature modules / Play Feature Delivery
- closed-track ergonomics
- staged rollout resume/halt helpers
- BSP server
- better debugger attach UX
- imported Play credentials in Orbi state

### Phase 3: Expanded Android surface

- Wear OS form-factor flow
- Android TV / Automotive / XR track helpers
- richer UI backend commands
- code shrinking / R8
- native build support

## Explicit Non-Goals For V1

- Gradle plugin compatibility layer
- arbitrary AGP DSL support
- KAPT/KSP support
- NDK/CMake support
- Play Asset Delivery
- Google Play Instant / App Clip equivalent
- remote cleanup of Play assets
- 1:1 recreation of every Apple-only command on Android

## Risks

### 1. Manifest merge complexity

This is the largest technical risk for AAR compatibility.

Mitigation:

- start with well-behaved AndroidX dependencies
- document unsupported `tools:` markers if Phase 1 cannot support them
- add focused e2e cases with real AARs

### 2. Compose compiler plugin wiring

Orbi must invoke the Kotlin Compose compiler plugin outside Gradle.

Mitigation:

- pin Kotlin version in one place
- write a dedicated integration test around a minimal Compose app

### 3. Android lint model complexity

Do not fake this. Ship it later if needed.

### 4. Device-side UI automation flakiness

Mitigation:

- use UI Automator waits and accessibility-tree stability checks
- keep the YAML subset small
- always capture screenshots and logs on failure

## Recommended First Slice

Implement in this order:

1. schema + init + example app
2. dependency resolver + lockfile
3. direct debug APK build
4. run on emulator/device
5. release APK + signing
6. AAB build + receipt
7. Play submit
8. unit tests
9. UI harness
10. formatter

## Sources

- Local Orbi repo sources:
  [`README.md`](/Users/ilyai/Developer/personal/orbi2/README.md),
  [`src/cli.rs`](/Users/ilyai/Developer/personal/orbi2/src/cli.rs),
  [`src/commands/mod.rs`](/Users/ilyai/Developer/personal/orbi2/src/commands/mod.rs),
  [`src/manifest.rs`](/Users/ilyai/Developer/personal/orbi2/src/manifest.rs),
  [`src/apple/`](/Users/ilyai/Developer/personal/orbi2/src/apple/)
- [sdkmanager](https://developer.android.com/tools/sdkmanager)
- [avdmanager](https://developer.android.com/tools/avdmanager)
- [Android Debug Bridge (adb)](https://developer.android.com/tools/adb)
- [Logcat command-line tool](https://developer.android.com/tools/logcat)
- [AAPT2](https://developer.android.com/tools/aapt2)
- [d8](https://developer.android.com/tools/d8)
- [zipalign](https://developer.android.com/tools/zipalign)
- [apksigner](https://developer.android.com/tools/apksigner)
- [bundletool](https://developer.android.com/tools/bundletool)
- [The Android App Bundle format](https://developer.android.com/guide/app-bundle/app-bundle-format)
- [Upload your app to the Play Console](https://developer.android.com/studio/publish/upload-bundle)
- [Sign your app / Play App Signing](https://developer.android.com/studio/publish/app-signing)
- [Google Play Developer API overview](https://developers.google.com/android-publisher)
- [Google Play Developer API getting started](https://developers.google.com/android-publisher/getting_started)
- [Edits resource](https://developers.google.com/android-publisher/api-ref/rest/v3/edits)
- [edits.bundles.upload](https://developers.google.com/android-publisher/api-ref/rest/v3/edits.bundles/upload)
- [edits.tracks.update](https://developers.google.com/android-publisher/api-ref/rest/v3/edits.tracks/update)
- [APKs and Tracks](https://developers.google.com/android-publisher/tracks)
- [internalappsharingartifacts.uploadbundle](https://developers.google.com/android-publisher/api-ref/rest/v3/internalappsharingartifacts/uploadbundle)
- [Target API level requirements for Google Play apps](https://support.google.com/googleplay/android-developer/answer/11926878)
- [Overview of Play Feature Delivery](https://developer.android.com/guide/playcore/feature-delivery)
- [Manage manifest files](https://developer.android.com/build/manage-manifests)
- [Manage remote repositories](https://developer.android.com/build/remote-repositories)
- [Create an Android library / AAR anatomy](https://developer.android.com/studio/projects/android-library)
- [Build local unit tests](https://developer.android.com/training/testing/local-tests)
- [Build instrumented tests](https://developer.android.com/training/testing/instrumented-tests)
- [Write automated tests with UI Automator](https://developer.android.com/training/testing/other-components/ui-automator)
- [Jetpack Compose setup and compiler](https://developer.android.com/develop/ui/compose/setup-compose-dependencies-and-compiler)
- [Compose to Kotlin compatibility map](https://developer.android.com/jetpack/androidx/releases/compose-kotlin)
- [Kotlin command-line compiler](https://kotlinlang.org/docs/command-line.html)
