# Orbit

Orbit is a local-first Apple app build, signing, and submission CLI.

## Mental Model

- One `orbit.json` describes one product.
- The root object is the app itself, not an Xcode-style target graph.
- Embedded pieces live under the app:
  - `extensions`
  - `watch`
  - `app_clip`
- If two apps do not share the same product identity, they should use different `orbit.json` files.
- If your sources are not truly multi-platform, split them into separate app manifests instead of adding Xcode-like config branches.

## The Shape

Minimal app:

```json
{
  "$schema": "/Users/your-user/.orbit/schemas/apple-app.v1.json",
  "name": "ExampleApp",
  "bundle_id": "dev.orbit.examples.app",
  "version": "1.0.0",
  "build": 1,
  "platforms": {
    "ios": "18.0"
  },
  "sources": ["Sources/App"],
  "resources": ["Resources"]
}
```

More complete app:

```json
{
  "$schema": "/Users/your-user/.orbit/schemas/apple-app.v1.json",
  "name": "Orbit VPN",
  "display_name": "Orbit",
  "bundle_id": "dev.orbit.vpn",
  "version": "1.2.3",
  "build": 42,
  "team_id": "TEAM123456",
  "provider_id": "128120286",
  "platforms": {
    "ios": "18.0",
    "macos": "15.0"
  },
  "sources": ["Sources/App"],
  "resources": ["Resources"],
  "dependencies": {
    "OrbitGreeting": { "path": "Packages/OrbitGreeting" },
    "NetworkExtension": { "framework": true }
  },
  "info": {
    "extra": {
      "NSCameraUsageDescription": "Scan QR codes"
    }
  },
  "entitlements": {
    "app_groups": ["group.dev.orbit.vpn.shared"],
    "sandbox": {
      "enabled": true,
      "network": ["client"]
    }
  },
  "pushBroadcastForLiveActivities": false,
  "extensions": {
    "tunnel": {
      "kind": "packet-tunnel",
      "platforms": ["ios", "macos"],
      "sources": ["Sources/TunnelExtension"],
      "dependencies": {
        "NetworkExtension": { "framework": true }
      },
      "entry": {
        "class": "PacketTunnelProvider"
      },
      "entitlements": {
        "app_groups": ["group.dev.orbit.vpn.shared"],
        "network_extensions": ["packet-tunnel-provider"]
      }
    }
  },
  "watch": {
    "sources": ["Sources/WatchApp"],
    "extension": {
      "sources": ["Sources/WatchExtension"],
      "entry": {
        "class": "WatchExtensionDelegate"
      }
    }
  },
  "app_clip": {
    "sources": ["Sources/AppClip"]
  }
}
```

Orbit manifests should point at `~/.orbit/schemas/`. Install them with `./scripts/install-schemas.sh`, then use `orbit init` to write that local absolute schema path into new manifests. Set `ORBIT_SCHEMA_DIR` before running the script if you need a different install location.

## Field Guide

### Identity

- `name`: canonical product name inside Orbit.
- `display_name`: optional launcher/home screen name. If omitted, Orbit uses `name`.
- `bundle_id`: root bundle identifier for the app.
- `version`: release version in Apple-friendly `x.y.z` form.
- `build`: integer build number.
- `team_id`: optional default Apple Developer team for this product.
- `provider_id`: optional default App Store Connect provider for this product.

### Platforms

`platforms` is a simple deployment target map:

```json
"platforms": {
  "ios": "18.0",
  "macos": "15.0"
}
```

Keep it simple. If the same `sources` are not valid for every listed platform, use separate manifests instead of inventing per-platform build branches.

### Sources And Resources

- `sources`: app source roots.
- `resources`: bundle resources copied or compiled into the app.

Use `resources` for things that should be available via `Bundle.main`, including:

- asset catalogs
- app icons
- launch screen resources
- storyboards and xibs
- `.strings`
- `InfoPlist.strings`
- Core Data models
- privacy manifests and other bundle files

Suggested layout:

```text
Resources/
  Assets.xcassets/
  Base.lproj/
    LaunchScreen.storyboard
  en.lproj/
    InfoPlist.strings
  ru.lproj/
    InfoPlist.strings
```

### Dependencies

`dependencies` is package.json-style: a dictionary keyed by dependency name.

Example:

```json
"dependencies": {
  "OrbitGreeting": { "path": "Packages/OrbitGreeting" },
  "PinnedGreeting": {
    "git": "https://github.com/example/PinnedGreeting.git",
    "version": "1.2.0"
  },
  "NetworkExtension": { "framework": true },
  "VendorSDK": { "xcframework": "Vendor/VendorSDK.xcframework" }
}
```

This keeps the public schema cleaner than separate arrays for `swift_packages`, `frameworks`, `weak_frameworks`, and `xcframeworks`.

For git-backed Swift packages:

- omit `version` to pin directly to an exact `revision`
- set `version` to request an exact release tag, which Orbit resolves into `.orbit/orbit.lock`

Any command that resolves dependencies will materialize `.orbit/orbit.lock` automatically when needed.

`orbit deps update` refreshes git dependency intent and then rewrites `.orbit/orbit.lock`:

- to remote `HEAD` when there is no `version`
- to the latest remote semver tag in the same major when `version` is present, rewriting `version` in `orbit.json` and the resolved `revision` in `.orbit/orbit.lock`

```bash
orbit deps update
orbit deps update PinnedGreeting
```

Orbit should auto-detect whether a dependency needs embedding.

Expected behavior:

- static libraries and static frameworks are linked into the final binary and usually should not be embedded
- dynamic frameworks and dylibs that must ship inside the app bundle should be embedded automatically

Orbit can then place that payload in the app's `Frameworks` directory before the final signing step.

`embed` should exist only as an optional override for rare edge cases.

Example override:

```json
"dependencies": {
  "VendorSDK": {
    "xcframework": "Vendor/VendorSDK.xcframework",
    "embed": false
  }
}
```

### Info.plist

Use `info` for app metadata that belongs in `Info.plist`.

For v1, the main escape hatch is `info.extra`:

```json
"info": {
  "extra": {
    "NSCameraUsageDescription": "Scan QR codes"
  }
}
```

Use this for:

- usage descriptions like `NSCameraUsageDescription`
- custom plist keys that Orbit does not model yet

Do not put translations in `orbit.json`. Localize plist strings through `InfoPlist.strings` inside `resources`.

### Entitlements

`entitlements` is inline Orbit DSL, not a path to a separate `.entitlements` file.

Example:

```json
"entitlements": {
  "app_groups": ["group.dev.orbit.shared"],
  "sandbox": {
    "enabled": true,
    "network": ["client"],
    "files": ["user-selected.read-write"]
  }
}
```

Orbit should generate the underlying Apple entitlements file from this block.

### Push

Push stays at the top level:

```json
"entitlements": {
  "push_notifications": true
},
"pushBroadcastForLiveActivities": false
```

- `entitlements.push_notifications`: enables ordinary Push Notifications capability.
- `pushBroadcastForLiveActivities`: optional advanced flag for Broadcast Push Notifications for Live Activities.
- Orbit manages the app-side push capability and entitlements only. APNs server credentials stay outside Orbit.

### Extensions

Ordinary embedded extensions live under `extensions`.

```json
"extensions": {
  "tunnel": {
    "kind": "packet-tunnel",
    "sources": ["Sources/TunnelExtension"],
    "entry": {
      "class": "PacketTunnelProvider"
    }
  }
}
```

Rules:

- The object key is the extension's stable local id.
- Orbit can use that key as the default bundle id suffix.
- `kind` is Orbit vocabulary, not a raw Apple plist key.
- `entry.class` is the extension entry class when that extension type needs one.

Examples of things that belong in `extensions`:

- packet tunnel extensions
- widgets
- share extensions
- Safari extensions

### Watch

`watch` is a special block for a companion watch app that ships with the main iOS app.

```json
"watch": {
  "sources": ["Sources/WatchApp"],
  "extension": {
    "sources": ["Sources/WatchExtension"],
    "entry": {
      "class": "WatchExtensionDelegate"
    }
  }
}
```

Use `watch` only for a companion watch app attached to the main product.

If the watch app is a standalone product, give it its own `orbit.json`.

### App Clip

`app_clip` is a special block for an iOS App Clip attached to the host app.

```json
"app_clip": {
  "sources": ["Sources/AppClip"]
}
```

Use App Clips for lightweight, install-free entry points tied to the full iOS app, such as:

- QR-driven flows
- quick checkout
- instant booking or activation
- one-task guest experiences

### Hooks

Hooks should stay small and explicit.

```json
"hooks": {
  "before_build": ["./scripts/generate-assets.sh"],
  "before_run": ["./scripts/prepare-local-fixture.sh"],
  "after_sign": ["./scripts/verify-bundle.sh"]
}
```

Hook commands run from the project root. Orbit exports context such as `ORBIT_HOOK`,
`ORBIT_TARGET_NAME`, `ORBIT_PLATFORM`, `ORBIT_DISTRIBUTION`, `ORBIT_CONFIGURATION`,
`ORBIT_DESTINATION`, `ORBIT_BUNDLE_PATH`, `ORBIT_ARTIFACT_PATH`, and `ORBIT_RECEIPT_PATH`
when they are available for the current lifecycle step.

Avoid recreating Xcode build phases. Only add hooks for clearly justified project steps.

### Tests

Orbit runs `tests.unit` through Swift Testing by synthesizing a temporary SwiftPM
package and invoking `swift test`.

```json
"tests": {
  "unit": {
    "sources": ["Tests/Unit"]
  },
  "ui": {
    "format": "maestro",
    "sources": ["Tests/UI"]
  }
}
```

Run unit tests with:

```sh
orbit test
```

Run UI tests with:

```sh
orbit test --ui --platform ios
```

`tests.ui` currently supports an Orbit-native runner that executes a
Maestro-style YAML subset against iOS simulators. Common commands such as
`assertVisible`, `assertNotVisible`, and `tapOn` include built-in polling, and
each top-level flow writes an `.mp4` screen recording alongside screenshots and
the JSON report. Orbit also streams app logs to the terminal during
`orbit test --ui`, `orbit run --platform ios --simulator`, and iOS device runs.

Current iOS simulator command support includes:
`launchApp`, `stopApp`, `killApp`, `clearState`, `clearKeychain`, `tapOn`,
`tapOnPoint`, `doubleTapOn`, `longPressOn`, `swipe`, `scroll`,
`scrollUntilVisible`, `inputText`, `pasteText`, `setClipboard`,
`copyTextFrom`, `eraseText`, `pressKey`, `pressKeyCode`, `keySequence`,
`pressButton`, `hideKeyboard`, `assertVisible`, `assertNotVisible`,
`extendedWaitUntil`, `waitForAnimationToEnd`, `takeScreenshot`,
`startRecording`, `stopRecording`, `openLink`, `setLocation`,
`setPermissions`, `travel`, `addMedia`, `runFlow`, `repeat`, and `retry`.

Current macOS backend coverage includes `launchApp`, `stopApp`, `clearState`,
`tapOn`, `hoverOn`, `rightClickOn`, `dragAndDrop`, `swipe`, `scroll`,
`scrollUntilVisible`, `inputText`, `assertVisible`, `takeScreenshot`,
window-scoped video recording, `openLink`, and `logs`. Modified keyboard
shortcuts on macOS are not yet documented as stable.

## CLI

Build intent comes from CLI flags.

### Test

Run the manifest's `tests.unit` suite with Swift Testing:

```sh
orbit test
```

Run the manifest's `tests.ui` suite on an iOS simulator:

```sh
orbit test --ui --platform ios
```

Run the manifest's `tests.ui` suite on macOS:

```sh
orbit test --ui --platform macos
```

Preflight macOS UI automation permissions and tooling:

```sh
orbit ui doctor --platform macos
```

Inspect the launched app's accessibility tree on an iOS simulator:

```sh
orbit ui dump-tree --platform ios
```

Inspect the accessibility element at a specific point:

```sh
orbit ui describe-point --platform ios --x 140 --y 142
```

Bring the simulator window to the foreground:

```sh
orbit ui focus --platform ios
```

Tail simulator logs through `idb log`:

```sh
orbit ui logs --platform ios -- --timeout 1s
```

Import media into the simulator camera roll:

```sh
orbit ui add-media --platform ios ./Tests/Fixtures/cat.jpg
```

Open a URL or deep link through `idb open`:

```sh
orbit ui open --platform ios https://example.com
```

Install a simulator dylib with `idb dylib install`:

```sh
orbit ui install-dylib --platform ios ./Tests/Fixtures/TestAgent.dylib
```

Run Instruments against the selected simulator:

```sh
orbit ui instruments --platform ios --template "Time Profiler" -- --operation-duration 5
```

Overwrite the simulator contacts database:

```sh
orbit ui update-contacts --platform ios ./Tests/Fixtures/contacts.sqlite
```

Inspect or delete crash logs:

```sh
orbit ui crash --platform ios list --bundle-id dev.orbit.fixture.ui
orbit ui crash --platform ios show mock-crash-1.ips
orbit ui crash --platform ios delete --all --since 1710000000
```

### Run

Run in the simulator:

```sh
orbit run --platform ios --simulator
```

Run on a device and attach a debugger:

```sh
orbit run --platform ios --device --debug
```

### Build

Development build:

```sh
orbit build --platform ios --distribution development
```

App Store build:

```sh
orbit build --platform ios --distribution app-store --release
```

macOS Developer ID build:

```sh
orbit build --platform macos --distribution developer-id --release
```

For macOS Developer ID builds, Orbit automatically verifies the produced artifact with:

- `codesign -dv --verbose=4` on the app bundle
- `pkgutil --check-signature` on the installer package
- `spctl -a -vvv --type install` on the installer package

Before notarization, Gatekeeper is expected to report `Unnotarized Developer ID`. Orbit treats that as the expected pre-notary state rather than a build failure.

### Submit

Submit the latest matching receipt:

```sh
orbit submit --platform ios --wait
```

Submit a specific receipt:

```sh
orbit submit --receipt .orbit/receipts/abc123.json --wait
```

### Clean

Remove local Orbit state and Orbit-managed Apple resources for the current manifest:

```sh
orbit clean --all
```

Remote cleanup is intentionally conservative:

- Orbit removes Orbit-managed provisioning profiles, bundle IDs, app groups, merchant IDs, and iCloud containers.
- Orbit does not revoke remote signing certificates during cleanup.
- Orbit always removes local signing material from `.orbit`, so a future build may still re-import or reuse an existing remote certificate.

### Operational Notes

- Apple ID auth now runs through GrandSlam/AuthKit and Developer Services. Orbit no longer relies on the old browser-cookie Apple login path for normal build, signing, submit, or notary flows.
- `Associated Domains` is removed from the local entitlement file when you delete it from `orbit.json`, but Orbit does not force-disable the remote App ID capability. This matches current Xcode behavior: the signed app stops carrying the entitlement, while the App ID service can remain enabled remotely.
- Xcode-like notarization works only if the Apple team is configured for notarization. A rejected submission with status code `7000` means the account still needs Apple-side notarization setup.
- After a successful notarization wait, Orbit automatically validates the stapled package with `xcrun stapler validate` and re-runs Gatekeeper/package-signature checks.

### Apple Device Management

List devices:

```sh
orbit apple device list --refresh
```

Register the current machine:

```sh
orbit apple device register --current-machine
```

Import devices from a file:

```sh
orbit apple device import --file ./devices.csv
```

### Signing Utilities

Export signing materials:

```sh
orbit apple signing export --platform ios --output-dir ./signing
```
