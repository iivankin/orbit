# Orbi

Orbi turns an Apple app into one readable manifest and one batteries-included
local CLI.

No hand-maintained Xcode project graph. No signing maze. The everyday app
toolchain is built in: lint, format, tests, SwiftUI `#Preview` screenshots, UI
automation, trace capture, signing, and App Store Connect submission. Orbi reads
`orbi.json` and drives the whole loop from one CLI.

## Install

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/iivankin/orbit/master/install.sh | bash
```

Windows:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/iivankin/orbit/master/install.ps | iex"
```

The installer puts `orbi` under `~/.local/bin` on macOS/Linux and
`%USERPROFILE%\.local\bin` on Windows.

## Why Orbi

- **One manifest for the product.** Describe the app, platforms, resources,
  entitlements, extensions, watch app, App Clip, tests, hooks, and ASC signing
  state in `orbi.json`.
- **Local-first builds without Xcode project ceremony.** Orbi drives the Apple
  toolchain directly and keeps generated state under `.orbi/`.
- **Quality tooling out of the box.** `orbi lint` and `orbi format` are part of
  the product workflow, with Orbi-owned defaults and manifest-driven config.
- **SwiftUI `#Preview` support.** List previews and render screenshot PNGs from
  the same CLI you use for builds and tests.
- **Signing and submission are part of the workflow.** Build development,
  App Store, TestFlight, Developer ID, and Mac App Store artifacts from the same
  CLI surface.
- **UI testing is built in.** Scaffold JSON flows, run them on iOS simulators or
  macOS, drive direct UI actions, dump accessibility trees, collect screenshots,
  and run final trace passes.
- **Preview, lint, format, run, build, submit.** The common loop is one command
  away instead of a pile of per-project scripts.

## Use It In A Few Commands

```bash
# Create the project scaffold and starter orbi.json.
orbi init

# Check the manifest, sources, dependencies, and formatting.
orbi lint
orbi format --write

# Run tests and inspect SwiftUI #Preview screenshots.
orbi test
orbi preview list --platform ios
orbi preview shot Basic --platform ios

# Launch locally, then build a release artifact.
orbi run --platform ios --simulator
orbi build --platform ios --distribution app-store --release

# Submit the latest matching receipt when the artifact is ready.
orbi submit --platform ios --wait
```

For UI flows:

```bash
orbi ui init Tests/UI/login.json
orbi test --ui --platform ios --trace
orbi ui dump-tree --platform ios
```

## Mental Model

- One `orbi.json` describes one product.
- Environment overlays live next to it as `orbi.<env>.json` and apply on top of the base manifest when you pass `--env <env>`.
- The root object is the app itself, not an Xcode-style target graph.
- Embedded pieces live under the app:
  - `extensions`
  - `watch`
  - `app_clip`
- If two apps do not share the same product identity, they should use different `orbi.json` files.
- If your sources are not truly multi-platform, split them into separate app manifests instead of adding Xcode-like config branches.

## The Shape

Minimal app:

```json
{
  "$schema": "https://orbitstorage.dev/schemas/apple-app.v1-orbi-0.1.0.json",
  "name": "ExampleApp",
  "bundle_id": "dev.orbi.examples.app",
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
  "$schema": "https://orbitstorage.dev/schemas/apple-app.v1-orbi-0.1.0.json",
  "name": "Orbi VPN",
  "display_name": "Orbi",
  "bundle_id": "dev.orbi.vpn",
  "version": "1.2.3",
  "build": 42,
  "xcode": "26.4",
  "platforms": {
    "ios": "18.0",
    "macos": "15.0"
  },
  "sources": ["Sources/App"],
  "resources": ["Resources"],
  "dependencies": {
    "OrbiGreeting": { "path": "Packages/OrbiGreeting" },
    "NetworkExtension": { "framework": true }
  },
  "info": {
    "extra": {
      "NSCameraUsageDescription": "Scan QR codes"
    }
  },
  "entitlements": {
    "app_groups": ["group.dev.orbi.vpn.shared"],
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
        "app_groups": ["group.dev.orbi.vpn.shared"],
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

New manifests created by `orbi init` point at a version-pinned published schema on `https://orbitstorage.dev/schemas/`. Install local copies with `./scripts/install-schemas.sh` if you want editor or offline validation against `~/.orbi/schemas/`; set `ORBI_SCHEMA_DIR` before running the script if you need a different install location.

## Field Guide

### Identity

- `name`: canonical product name inside Orbi.
- `display_name`: optional launcher/home screen name. If omitted, Orbi uses `name`.
- `bundle_id`: root bundle identifier for the app.
- `version`: release version in Apple-friendly `x.y.z` form.
- `build`: integer build number.
- `xcode`: optional installed Xcode version, such as `26.4`. When set, Orbi uses that Xcode's developer directory and downloads the matching official simulator runtime if the selected Xcode is missing it. If that Xcode is not installed, Orbi asks you to install it manually or choose another installed Xcode for the current run.

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
  "OrbiGreeting": { "path": "Packages/OrbiGreeting" },
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
- set `version` to request an exact release tag, which Orbi resolves into `.orbi/orbi.lock`

Any command that resolves dependencies will materialize `.orbi/orbi.lock` automatically when needed.

`orbi deps update` refreshes git dependency intent and then rewrites `.orbi/orbi.lock`:

- to remote `HEAD` when there is no `version`
- to the latest remote semver tag in the same major when `version` is present, rewriting `version` in `orbi.json` and the resolved `revision` in `.orbi/orbi.lock`

```bash
orbi deps update
orbi deps update PinnedGreeting
```

Orbi should auto-detect whether a dependency needs embedding.

Expected behavior:

- static libraries and static frameworks are linked into the final binary and usually should not be embedded
- dynamic frameworks and dylibs that must ship inside the app bundle should be embedded automatically

Orbi can then place that payload in the app's `Frameworks` directory before the final signing step.

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
- custom plist keys that Orbi does not model yet

Do not put translations in `orbi.json`. Localize plist strings through `InfoPlist.strings` inside `resources`.

### Entitlements

`entitlements` is inline Orbi DSL, not a path to a separate `.entitlements` file.

Example:

```json
"entitlements": {
  "app_groups": ["group.dev.orbi.shared"],
  "sandbox": {
    "enabled": true,
    "network": ["client"],
    "files": ["user-selected.read-write"]
  }
}
```

Orbi should generate the underlying Apple entitlements file from this block.

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
- Orbi manages the app-side push capability and entitlements only. APNs server credentials stay outside Orbi.

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
- Orbi can use that key as the default bundle id suffix.
- `kind` is a closed Orbi DSL that maps to concrete Xcode application extension templates.
- Principal-class kinds such as `packet-tunnel`, `finder-sync`, `notification-service`, and `photo-project` require `entry.class`.
- Storyboard-backed kinds such as `share`, `action-ui`, `notification-content`, `messages`, `file-provider-ui`, and `account-authentication-modification` default to `entry.storyboard = "MainInterface"` and only need `entry` if you want a different storyboard name.
- `widget` and ExtensionKit kinds such as `app-intents`, `contact-provider`, `translation-provider`, `background-download`, and `file-system` omit `entry` entirely.
- The closed DSL covers concrete Xcode application extension templates. Placeholder `Generic Extension` and special sticker-pack product types are not modeled as ordinary `extensions`.

Examples of things that belong in `extensions`:

- packet tunnel extensions
- widgets
- share extensions
- Safari extensions

Examples:

```json
"extensions": {
  "tunnel": {
    "kind": "packet-tunnel",
    "sources": ["Sources/TunnelExtension"],
    "entry": {
      "class": "PacketTunnelProvider"
    }
  },
  "share": {
    "kind": "share",
    "sources": ["Sources/ShareExtension"]
  },
  "widget": {
    "kind": "widget",
    "sources": ["Sources/WidgetExtension"]
  },
  "intents": {
    "kind": "app-intents",
    "sources": ["Sources/AppIntentsExtension"]
  }
}
```

Several extension kinds also expose first-class config blocks for the
template-specific `Info.plist` keys that Xcode normally synthesizes:

- `action` for `share`, `action-ui`, `action-service`, and
  `broadcast-setup-ui`
- `account_authentication_modification` for
  `account-authentication-modification`
- `core_spotlight_delegate` for `core-spotlight-delegate`
- `broadcast_upload` for `broadcast-upload`
- `file_provider` for `file-provider`
- `file_provider_ui` for `file-provider-ui`
- `intents` for `intents` and `intents-ui`
- `keyboard` for `custom-keyboard`
- `message_filter` for `message-filter`
- `notification_content` for `notification-content`
- `persistent_token` for `persistent-token`
- `photo_project` for `photo-project`
- `quick_look_preview` for `quick-look-preview`
- `spotlight_import` for `spotlight-import`
- `thumbnail` for `thumbnail`
- `unwanted_communication_reporting` for
  `unwanted-communication-reporting`
- `accessory_setup` for `accessory-setup`
- `accessory_data_transport` for `accessory-data-transport`
- `background_resource_upload` for `background-resource-upload`

Examples:

```json
"extensions": {
  "provider": {
    "kind": "file-provider",
    "sources": ["Sources/FileProviderExtension"],
    "entry": {
      "class": "FileProviderExtension"
    },
    "entitlements": {
      "app_groups": ["group.dev.orbi.files"]
    },
    "file_provider": {
      "document_group": "group.dev.orbi.files",
      "supports_enumeration": true
    }
  },
  "provider-ui": {
    "kind": "file-provider-ui",
    "sources": ["Sources/FileProviderUIExtension"],
    "file_provider_ui": {
      "actions": [
        {
          "identifier": "dev.orbi.files.share",
          "name": "Share",
          "activation_rule": "TRUEPREDICATE"
        }
      ]
    }
  },
  "notification": {
    "kind": "notification-content",
    "sources": ["Sources/NotificationContentExtension"],
    "notification_content": {
      "categories": ["comment", "follow"],
      "initial_content_size_ratio": 1.0
    }
  }
}
```

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

If the watch app is a standalone product, give it its own `orbi.json`.

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

Hook commands run from the project root. Orbi exports context such as `ORBI_HOOK`,
`ORBI_TARGET_NAME`, `ORBI_PLATFORM`, `ORBI_DISTRIBUTION`, `ORBI_CONFIGURATION`,
`ORBI_DESTINATION`, `ORBI_BUNDLE_PATH`, `ORBI_ARTIFACT_PATH`, and `ORBI_RECEIPT_PATH`
when they are available for the current lifecycle step.

Avoid recreating Xcode build phases. Only add hooks for clearly justified project steps.

### Tests

Orbi runs `tests.unit` through Swift Testing by synthesizing a temporary SwiftPM
package and invoking `swift test`.

```json
"tests": {
  "unit": ["Tests/Unit"],
  "ui": ["Tests/UI"]
}
```

Run unit tests with:

```sh
orbi test
```

Run UI tests with:

```sh
orbi test --ui --platform ios
orbi test --ui --platform macos --focus
```

`tests.ui` currently supports an Orbi-native runner that executes JSON UI flow
files with a required `$schema` and `steps` object shape against iOS simulators.
Common commands such as
`assertVisible`, `assertNotVisible`, and `tapOn` include built-in polling, and
each top-level flow writes an `.mp4` screen recording alongside screenshots and
the JSON report. Orbi also streams app logs to the terminal during
`orbi test --ui`, `orbi run --platform ios --simulator`, and iOS device runs.
Use `orbi ui init Tests/UI/login.json` to scaffold a new flow file. The accepted
flow grammar is defined by each flow file's `$schema`.

The common standalone flow actions are also exposed directly under `orbi ui`,
for example `orbi ui launch-app`, `orbi ui tap`, `orbi ui swipe`,
`orbi ui drag`, `orbi ui assert-visible`, `orbi ui set-location`, and
`orbi ui travel`. Existing utility commands such as `orbi ui open` and
`orbi ui add-media` cover the JSON flow `openLink` and `addMedia` actions.

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
orbi test
```

Run the manifest's `tests.ui` suite on an iOS simulator:

```sh
orbi test --ui --platform ios
```

Run the manifest's `tests.ui` suite on macOS:

```sh
orbi test --ui --platform macos
```

The macOS backend routes most input to the target app process, so concurrent
`orbi test --ui --platform macos` runs can overlap without a machine-wide UI test lock or
forcing the app under test to stay frontmost. Drag-and-drop remains a frontmost/HID fallback
because AppKit drop targets still require a foreground drag session.

Run only a selected UI flow by its configured `name`, file stem, file name, or path:

```sh
orbi test --ui --platform macos --flow onboarding-provider-setup
```

Keep the launched automation target frontmost when you want to watch the flow:

```sh
orbi test --ui --platform macos --focus
```

Run standalone UI actions directly from the CLI:

```sh
orbi ui launch-app --platform ios
orbi ui tap --platform ios --text Continue
orbi ui swipe --platform ios --direction left
```

Preflight macOS UI automation permissions and tooling:

```sh
orbi ui doctor --platform macos
```

Inspect the launched app's accessibility tree on an iOS simulator:

```sh
orbi ui dump-tree --platform ios
```

Inspect the accessibility element at a specific point:

```sh
orbi ui describe-point --platform ios --x 140 --y 142
```

Bring the simulator window to the foreground:

```sh
orbi ui focus --platform ios
```

Tail simulator logs through `idb log`:

```sh
orbi ui logs --platform ios -- --timeout 1s
```

Import media into the simulator camera roll:

```sh
orbi ui add-media --platform ios ./Tests/Fixtures/cat.jpg
```

Open a URL or deep link through `idb open`:

```sh
orbi ui open --platform ios https://example.com
```

Install a simulator dylib with `idb dylib install`:

```sh
orbi ui install-dylib --platform ios ./Tests/Fixtures/TestAgent.dylib
```

Run Instruments against the selected simulator:

```sh
orbi ui instruments --platform ios --template "Time Profiler" -- --operation-duration 5
```

Overwrite the simulator contacts database:

```sh
orbi ui update-contacts --platform ios ./Tests/Fixtures/contacts.sqlite
```

Inspect or delete crash logs:

```sh
orbi ui crash --platform ios list --bundle-id dev.orbi.fixture.ui
orbi ui crash --platform ios show mock-crash-1.ips
orbi ui crash --platform ios delete --all --since 1710000000
```

### Run

Run in the simulator:

```sh
orbi run --platform ios --simulator
```

Run on a device and attach a debugger:

```sh
orbi run --platform ios --device --debug
```

### Build

Development build:

```sh
orbi build --platform ios --distribution development
```

App Store build:

```sh
orbi build --platform ios --distribution app-store --release
```

macOS Developer ID build:

```sh
orbi build --platform macos --distribution developer-id --release
```

For macOS Developer ID builds, Orbi automatically verifies the produced artifact with:

- `codesign -dv --verbose=4` on the app bundle
- `codesign -dv --verbose=4` on the signed `.dmg`
- `spctl -a -vvv --type open` on the signed `.dmg`

Before notarization, Gatekeeper is expected to report `Unnotarized Developer ID`. Orbi treats that as the expected pre-notary state rather than a build failure.

### Submit

Submit the latest matching receipt:

```sh
orbi submit --platform ios --wait
```

Submit a specific receipt:

```sh
orbi submit --receipt .orbi/receipts/abc123.json --wait
```

### Clean

Remove local Orbi state and Orbi-managed Apple resources for the current manifest:

```sh
orbi clean --all
```

Remote cleanup is intentionally conservative:

- Orbi removes Orbi-managed provisioning profiles, bundle IDs, app groups, merchant IDs, and iCloud containers.
- Orbi does not revoke remote signing certificates during cleanup.
- Orbi always removes local signing material from `.orbi`, so a future build may still re-import or reuse an existing remote certificate.

### Operational Notes

- Apple account state now flows through the embedded `asc` config and `orbi asc ...` commands. Orbi no longer exposes the old `orbi apple ...` auth or device-management path.
- `Associated Domains` is removed from the local entitlement file when you delete it from `orbi.json`, but Orbi does not force-disable the remote App ID capability. This matches current Xcode behavior: the signed app stops carrying the entitlement, while the App ID service can remain enabled remotely.
- Xcode-like notarization works only if the Apple team is configured for notarization. A rejected submission with status code `7000` means the account still needs Apple-side notarization setup.
- `developer-id` builds export a signed `.dmg`.
- `mac-app-store` builds export a signed `.app` bundle.

### ASC Utilities

Put all App Store Connect state in the embedded `asc` section, including `asc.team_id`.

Sync the embedded ASC plan and import signing materials:

```sh
orbi asc validate
orbi asc plan
orbi asc apply
orbi asc signing import
orbi asc signing print-build-settings
```

Register the current machine into the embedded ASC config:

```sh
orbi asc device add-local --current-mac --apply
```
