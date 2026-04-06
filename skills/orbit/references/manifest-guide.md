# Manifest Guide

## Core Shape

Orbit is manifest-first. The shape of `orbit.json` drives build, signing,
resources, tests, and packaging.

Core fields usually include:

- `$schema`
- `name`
- `bundle_id`
- `version`
- `build`
- `platforms`
- `sources`
- `resources`

## Identity And Platforms

- `name` is Orbit's canonical product name.
- `display_name` is optional launcher/home-screen name.
- `bundle_id` is the root bundle identifier.
- `version` should stay Apple-friendly, usually `x.y.z`.
- `build` is an integer build number.
- `platforms` is a simple deployment-target map such as:

```json
"platforms": {
  "ios": "18.0",
  "macos": "15.0"
}
```

If the same source roots are not valid on every listed platform, split the app
into separate manifests instead of building complex per-platform branching.

## Sources And Resources

- `sources` are source roots.
- `resources` are bundle resources copied or compiled into the product.

Use `resources` for things that should be available from the bundle, including:

- asset catalogs
- storyboards and xibs
- localized strings
- `InfoPlist.strings`
- Core Data models
- privacy manifests

Keep translations in resources, not in `orbit.json`.

## Dependencies

`dependencies` is a dictionary keyed by dependency name.

Supported shapes include:

- local package path
- git-backed Swift package
- Apple framework
- xcframework

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

Orbit materializes `.orbit/orbit.lock` when dependency resolution is needed.
If you change git-backed dependencies, `orbit deps update` is usually the next
step. Do not hand-edit the lock unless the user explicitly asks.

## Info And Entitlements

- Use `info.extra` for raw `Info.plist` escape hatches.
- Use Orbit's inline `entitlements` DSL instead of a separate entitlements file.

Example:

```json
"info": {
  "extra": {
    "NSCameraUsageDescription": "Scan QR codes"
  }
},
"entitlements": {
  "app_groups": ["group.dev.orbit.shared"],
  "sandbox": {
    "enabled": true,
    "network": ["client"]
  }
}
```

## Additional Target Shapes

Orbit nests related targets inside the app manifest:

- `extensions`
- `watch`
- `app_clip`

Use those only when they are part of the same product identity. If they are
standalone products, give them their own manifest.

## Tests

Tests live under `tests` in the manifest.

Typical shape:

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

- `orbit test` runs `tests.unit` through Swift Testing.
- `orbit test --ui --platform <platform>` runs `tests.ui`.

For flow syntax and backend caveats, read:

- [ui-tests-overview.md](ui-tests-overview.md)
- [ui-test-yaml.md](ui-test-yaml.md)
- [ui-test-platforms.md](ui-test-platforms.md)

## Hooks

Hooks should stay small and explicit.

Example:

```json
"hooks": {
  "before_build": ["./scripts/generate-assets.sh"],
  "before_run": ["./scripts/prepare-local-fixture.sh"],
  "after_sign": ["./scripts/verify-bundle.sh"]
}
```

Hook commands run from the project root. Orbit exports context such as:

- `ORBIT_HOOK`
- `ORBIT_PROJECT_ROOT`
- `ORBIT_MANIFEST_PATH`
- `ORBIT_TARGET_NAME`
- `ORBIT_PLATFORM`
- `ORBIT_DISTRIBUTION`
- `ORBIT_CONFIGURATION`
- `ORBIT_DESTINATION`
- `ORBIT_BUNDLE_PATH`
- `ORBIT_ARTIFACT_PATH`
- `ORBIT_RECEIPT_PATH`

Do not recreate a large Xcode build-phase system through hooks unless the
project explicitly needs it.

## Authoring Rules

- Prefer hard cutovers over compatibility branches.
- Keep the manifest simple and product-shaped.
- Avoid inventing extra schema layers when Orbit already models the concept.
- Make the smallest manifest change that matches the product intent.
