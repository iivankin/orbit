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
  "$schema": "https://orbit.dev/schemas/apple-app.v1.json",
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
  "$schema": "https://orbit.dev/schemas/apple-app.v1.json",
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
  "push": {
    "environment": "production",
    "credential": "auth-key"
  },
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
  "NetworkExtension": { "framework": true },
  "VendorSDK": { "xcframework": "Vendor/VendorSDK.xcframework" }
}
```

This keeps the public schema cleaner than separate arrays for `swift_packages`, `frameworks`, `weak_frameworks`, and `xcframeworks`.

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
"push": {
  "environment": "development",
  "credential": "auth-key"
}
```

- `environment`: how the app should be signed for APNs, usually `development` or `production`.
- `credential`: which APNs credential Orbit should manage, such as `auth-key` or `certificate`.

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
  "after_sign": ["./scripts/verify-bundle.sh"]
}
```

Avoid recreating Xcode build phases. Only add hooks for clearly justified project steps.

### Tests

Tests can live next to the app manifest when Orbit gains first-class test support.

```json
"tests": {
  "unit": {
    "sources": ["Tests/Unit"]
  },
  "ui": {
    "sources": ["Tests/UI"]
  }
}
```

## CLI

Build intent comes from CLI flags.

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

### Submit

Submit the latest matching receipt:

```sh
orbit submit --platform ios --wait
```

Submit a specific receipt:

```sh
orbit submit --receipt .orbit/receipts/abc123.json --wait
```

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

Sync signing assets:

```sh
orbit apple signing sync --platform ios --distribution development
```

Export signing materials:

```sh
orbit apple signing export --platform ios --output-dir ./signing
```

Export APNs credentials:

```sh
orbit apple signing export-push --output ./AuthKey_ORBIT.p8
```
