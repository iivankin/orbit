# Linux Application Platform Spec

## Decision

For Linux v1, Orbit should target:

- distribution: `Flatpak` first
- store/release channel: `Flathub`
- UI stack: `GTK4 + libadwaita`
- first-class app language/runtime: `Rust`

This is the most modern Linux desktop path with the best native UI story.

It aligns with:

- Wayland-first desktop behavior
- sandboxing + portals
- a centralized cross-distro distribution channel
- GNOME’s current design system and adaptive widgets

## Why This Stack

### Why `Flatpak` first

As of April 1, 2026, Flatpak is the strongest first target for a modern Linux desktop app pipeline:

- official Flatpak docs position it as the standard way to build and distribute desktop applications with runtimes, manifests, sandboxing, and desktop integration
- Flatpak’s sandbox/portal model is the current modern Linux permissions model
- Flathub is the central repository for Flatpak apps across major Linux distributions
- official Flathub guidance is built around high-quality, sandboxed graphical desktop applications

Why not `AppImage` first:

- no first-class sandbox model
- weaker desktop integration story
- no equivalent of Flathub’s centralized review/distribution workflow

Why not `deb`/`rpm` first:

- distro-fragmented
- pushes Orbit toward distro packaging rules immediately
- worse fit for a local-first, one-manifest, cross-distro developer workflow

### Why `GTK4 + libadwaita` first

If the goal is "most modern with great UI", the best first-class Linux UI stack is `GTK4 + libadwaita`.

Official GNOME docs describe:

- GTK as GNOME’s UI toolkit
- libadwaita as the library that implements standard GNOME design patterns
- libadwaita specifically as building blocks for modern adaptive GNOME applications
- the GNOME HIG as intended for recent GTK 4 and libadwaita apps

Why not `Qt6 + Kirigami` first:

- it is a valid second target and the official KDE docs make clear it supports convergent, adaptive apps
- Flatpak also has an official KDE runtime for Qt/KDE apps
- but if Orbit must pick one first-class Linux UI stack, GTK4/libadwaita is the cleaner “modern native desktop UI” default

Qt/Kirigami should be a later parallel backend, not a compromised v1 abstraction.

### Why `Rust` first

Orbit already lives in a Rust codebase, and Rust is a practical first-class language choice for:

- a generated, Orbit-owned build pipeline
- strong local tooling
- a clean dependency model
- good GTK4/libadwaita bindings

Most importantly, supporting arbitrary Linux build systems in v1 would make the scope too loose to implement well.

## Scope

This spec is for a Linux equivalent of Orbit’s app platform:

- app manifest
- scaffolding
- dependency resolution
- build
- run
- package
- metadata generation
- release submission workflow

This is not only about UI automation. Linux UI automation is a separate subsystem and should be treated as a later companion feature. The earlier draft in [linux-ui-automation-spec.md](/Users/ilyai/Developer/personal/orbit2/docs/linux-ui-automation-spec.md) remains relevant, but it is not the primary Linux app-platform spec.

## Product Goals

Orbit on Linux should feel structurally similar to Orbit on Apple:

- one manifest describes one product
- Orbit owns the app build graph
- Orbit hides the packaging boilerplate
- Orbit emits installable release artifacts
- Orbit can prepare a store/repository submission path

For Linux, the exact analogue is:

- one Orbit Linux manifest
- Orbit-generated Cargo workspace
- Orbit-generated Flatpak manifest
- Orbit-generated desktop metadata
- Orbit-generated Flathub-ready packaging repo contents

## Non-Goals

Linux v1 does not include:

- `AppImage`
- `deb`
- `rpm`
- Snap
- generic arbitrary build-system support
- non-native desktop wrappers
- Electron-first support
- Qt/Kirigami first-class support
- direct store upload APIs beyond Git-based Flathub workflow

## Linux Mental Model

- one `orbit.json` describes one Linux desktop application
- the root object is the app, not a distro package graph
- Orbit owns app metadata, launcher metadata, and sandbox metadata
- Orbit synthesizes the Flatpak manifest instead of asking the user to author it manually

Flatpak/Flathub files are build artifacts, not the primary source of truth.

## V1 Stack

### Build/runtime stack

- app language: Rust 2024 edition
- UI toolkit: GTK4
- adaptive/design system layer: libadwaita
- Rust bindings: `gtk4`, `glib`, `gio`, `libadwaita`
- packaging/runtime: `org.gnome.Platform` + `org.gnome.Sdk`
- Rust SDK extension/toolchain: Orbit-managed as part of Linux toolchain setup

### GTK authoring conventions

Linux v1 must not stop at "some Rust app that links GTK".

Orbit should generate and standardize on the current GNOME-native Rust shape:

- application root type: `adw::Application`
- primary top-level window type: `adw::ApplicationWindow`
- UI definition format in v1: GTK Builder XML (`.ui`)
- UI loading pattern in v1: compiled GResource + `#[derive(CompositeTemplate)]`
- action model: application/window actions exposed through `gio`

Rules:

- `adw::Application` is required for the generated template and first-class examples
- Orbit must set a deterministic resource base path derived from `app_id`, using slash-separated path segments
- `style.css` lives at the application resource base path so `AdwApplication` loads it automatically
- if the app defines a shortcuts dialog, it should live at `shortcuts-dialog.ui` under the application resource base path
- non-trivial windows, pages, and reusable widgets should be implemented as Rust types backed by composite templates
- direct imperative widget construction is allowed for small dynamic fragments, but it is not the default authoring model
- Blueprint is deferred; Builder XML is the canonical Orbit v1 UI format

Example resource base path:

- `dev.orbit.notes` -> `/dev/orbit/notes/`

### Architectures

Linux v1 must target:

- `x86_64`
- `aarch64`

Reason:

- Flathub builds both by default unless restricted
- these are the only architectures worth first-classing in v1

## Manifest Schema

Introduce a new schema:

- `schemas/linux-app.v1.json`
- schema URL: `https://orbit.dev/schemas/linux-app.v1.json`

Do not overload `apple-app.v1.json`.

### Top-level fields

Required:

- `$schema`
- `name`
- `app_id`
- `version`
- `build`
- `runtime`

Optional:

- `display_name`
- `architectures`
- `sources`
- `resources`
- `dependencies`
- `metadata`
- `sandbox`
- `hooks`
- `tests`
- `quality`
- `submit`

### Example manifest

```json
{
  "$schema": "/Users/your-user/.orbit/schemas/linux-app.v1.json",
  "name": "Orbit Notes",
  "display_name": "Orbit Notes",
  "app_id": "dev.orbit.notes",
  "version": "1.0.0",
  "build": 1,
  "runtime": {
    "family": "gnome",
    "version": "48"
  },
  "architectures": ["x86_64", "aarch64"],
  "sources": ["Sources/App"],
  "resources": ["Resources"],
  "dependencies": {
    "gtk": {
      "crate": {
        "package": "gtk4",
        "version": "0.0.0"
      }
    },
    "adw": {
      "crate": {
        "package": "libadwaita",
        "version": "0.0.0"
      }
    },
    "OrbitCore": {
      "crate": {
        "path": "Packages/OrbitCore"
      }
    }
  },
  "metadata": {
    "summary": "Local-first notes for Linux",
    "description": "Orbit Notes is a local-first notes app for Linux desktops.",
    "license": "MIT",
    "homepage": "https://orbit.dev",
    "categories": ["Office", "Utility"],
    "keywords": ["notes", "markdown", "offline"],
    "icon": "Resources/AppIcon.svg",
    "screenshots": [
      "https://orbit.dev/screenshots/notes-main.png"
    ]
  },
  "sandbox": {
    "network": false,
    "audio": {
      "playback": false,
      "record": false
    },
    "display": {
      "wayland": true,
      "x11": "fallback"
    },
    "files": {
      "portals": true,
      "read": [],
      "read_write": []
    },
    "devices": ["dri"],
    "session_bus": [],
    "system_bus": []
  },
  "tests": {
    "unit": {
      "sources": ["Tests/Unit"]
    }
  },
  "submit": {
    "channel": "flathub",
    "verified_domain": "orbit.dev"
  }
}
```

### Identity

Use `app_id`, not `bundle_id`.

Rules:

- reverse-DNS format
- must satisfy Flatpak/Flathub app-id rules
- must be stable over the life of the app
- becomes:
  - Flatpak app ID
  - desktop file prefix
  - metainfo ID
  - sandbox identity
  - runtime state directory name

Orbit must validate `app_id` up front against Flathub-compatible rules, not only generic reverse-DNS formatting.

### Runtime

`runtime` is a structured field:

```json
"runtime": {
  "family": "gnome",
  "version": "48"
}
```

Linux v1 supports only:

- `family = "gnome"`

`version` is the exact Flatpak runtime branch.

Orbit must pin it exactly and never silently float it.

### Sources

`sources` are Rust source roots.

Conventions for the first-class stack:

- `Sources/App/main.rs` is required
- `Sources/App/app.rs` defines the application bootstrap around `adw::Application`
- additional modules may live alongside it or under submodules
- the main window should use the Rust subclass pattern around `adw::ApplicationWindow`
- Orbit collects `*.rs` files from declared source roots

Do not require users to author a top-level `Cargo.toml` in v1.

Orbit synthesizes the Cargo workspace.

### Resources

`resources` contain Linux app resources, including:

- GTK Builder XML (`.ui`)
- Blueprint files (`.blp`) later, not required in v1
- CSS
- icons
- templates
- translation files
- static assets

Orbit behavior:

- compile declared resources into a GResource bundle
- install exportable assets needed for desktop integration
- expose app resources under a deterministic prefix based on `app_id`
- default the generated app shell to composite-template-backed `.ui` resources, not imperative widget trees

V1 rule:

- users do not author `.gresource.xml` directly
- Orbit generates it
- Builder XML is the canonical v1 resource format
- Orbit templates should load `.ui` files from compiled resources, not from loose filesystem paths at runtime

### Dependencies

Keep the existing Orbit dictionary model.

Linux dependency types:

- Cargo crates
- local Cargo crates
- git Cargo crates
- Flatpak module fragments for native libraries not in the runtime

Example shapes:

```json
"dependencies": {
  "serde": {
    "crate": {
      "version": "1.0.0"
    }
  },
  "OrbitCore": {
    "crate": {
      "path": "Packages/OrbitCore"
    }
  },
  "sqlcipher": {
    "module": "Modules/sqlcipher.yml"
  }
}
```

Rules:

- crate dependencies must resolve to exact locked revisions in generated lockfiles
- if a dependency exists in the GNOME runtime or SDK, prefer that over bundling
- if a dependency is not in the runtime, Orbit may bundle it as a Flatpak module

This follows Flatpak guidance that runtime dependencies should be reused when available and bundled modules should be minimized.

### Metadata

Use one high-level `metadata` section and generate:

- desktop file
- metainfo file
- exported icons

`metadata` fields:

- `summary`
- `description`
- `license`
- `homepage`
- `support_url`
- `issues_url`
- `categories`
- `keywords`
- `icon`
- `screenshots`
- `content_rating`
- `developer_name`

Rules:

- `metadata.icon` must point to an SVG or a high-resolution PNG
- generated desktop file name must be `<app_id>.desktop`
- generated metainfo file name must be `<app_id>.metainfo.xml`
- metainfo ID must exactly equal `app_id`

Orbit must lint this metadata against Flathub expectations before submit.

### Sandbox

`sandbox` is Linux’s equivalent of a high-level entitlement DSL.

Orbit should map it to Flatpak `finish-args`, but the manifest stays high-level.

Allowed v1 fields:

- `network: bool`
- `audio.playback: bool`
- `audio.record: bool`
- `display.wayland: bool`
- `display.x11: "none" | "fallback" | "direct"`
- `files.portals: bool`
- `files.read: []`
- `files.read_write: []`
- `devices: []`
- `session_bus: []`
- `system_bus: []`
- `background: bool`
- `notifications: bool`

Rules:

- default to portals over blanket filesystem access
- forbid raw `finish-args` escape hatches in v1
- lint requested permissions against Flathub guidelines
- if the app supports Wayland, generate `--socket=wayland` plus `--socket=fallback-x11`, not both full X11 and fallback modes

This follows Flatpak and Flathub guidance:

- sandbox by default
- portals preferred wherever possible
- broad static permissions should be minimized

## Generated Artifacts

Orbit must generate the following during Linux builds:

- generated Cargo workspace under `.orbit/build/linux/generated/`
- generated GResource XML
- generated desktop file
- generated metainfo file
- generated Flatpak manifest
- generated dependency manifest for Cargo sources
- local Flatpak repo
- optional single-file `.flatpak` bundle
- Linux build receipt

### Cargo dependency manifest

Flathub forbids network access during build and requires dependencies to be present in the manifest or submission.

Therefore Orbit must generate Cargo dependency source manifests automatically.

Required behavior:

- generate `Cargo.lock`
- vendor or enumerate Cargo sources for Flatpak builds
- materialize a Flathub-ready dependency manifest as part of the build output

This is not optional. Rust support is incomplete without it.

## Build Pipeline

### `orbit build --platform linux`

Build algorithm:

1. validate `linux-app.v1.json`
2. normalize metadata and sandbox config
3. materialize generated Cargo workspace
4. resolve Cargo dependencies and lock them
5. generate GResource manifest
6. generate desktop file and metainfo file
7. generate Flatpak manifest named after `app_id`
8. run `flatpak-builder`
9. produce a local OSTree repo
10. optionally emit a `.flatpak` bundle when requested through `--output`

Flatpak is not user-authored input here. It is a backend artifact.

### `orbit run --platform linux`

Run semantics:

- build if needed
- install/update the local user Flatpak ref from the generated local repo
- launch with `flatpak run <app_id>`

Orbit should run the app in its real sandbox by default.

Do not default to host-native unsandboxed execution in v1. That breaks parity with the shipped product.

### `orbit test --platform linux`

Linux v1 test support:

- `tests.unit` only

Behavior:

- run unit tests inside the selected SDK/toolchain environment
- use `cargo test` against the generated workspace

`tests.ui` is deferred to the later Linux UI automation subsystem.

## Submit Pipeline

### `orbit submit --platform linux`

For Linux, `submit` does not mean "upload a signed binary to a store API".

It means:

- materialize or update a Flathub-ready packaging repository
- run Flathub lints locally
- commit changes
- open a PR against the appropriate GitHub repository

### Submission modes

#### New app

For a new app:

- create a submission branch against the Flathub submission flow
- include:
  - manifest named after `app_id`
  - dependency manifest(s)
  - `flathub.json` when needed for arch restriction or automation

#### Existing Flathub app

For an existing app:

- update the existing app repository
- refresh manifest sources and versions
- run lints
- open a PR

### Verification

Orbit should support Flathub verification preparation:

- derive the verification domain from `submit.verified_domain` or the `app_id`
- verify that the declared domain matches the app identity
- emit exact verification instructions for the `.well-known` token flow

Orbit should not try to automate the full domain verification flow in v1.

### Required tooling for submit

- `git`
- GitHub integration or `gh`
- `flatpak`
- `flatpak-builder`
- `org.flatpak.Builder` available locally for linting

## CLI Changes

### Platform enum

Do not add `linux` to the Apple-only `TargetPlatform` and call it done.

Introduce a platform abstraction that can express:

- Apple app targets
- Linux app targets
- UI-only inspection targets separately if needed

At minimum:

```rust
pub enum ProductPlatform {
    Apple(ApplePlatform),
    Linux,
}
```

Use this in:

- `build`
- `run`
- `submit`
- `init`
- `test`

### Distribution enum

Do not reuse Apple distribution kinds on Linux.

Introduce:

```rust
pub enum LinuxDistributionKind {
    Development,
    Flatpak,
    Flathub
}
```

Rules:

- `run` uses `Development`
- `build` defaults to `Flatpak`
- `submit` requires `Flathub`

## `orbit init`

Add a first-class Linux template:

- label: `Linux GTK app`
- stack: Rust + GTK4 + libadwaita + Flatpak

Generated layout:

```text
orbit.json
Sources/
  App/
    main.rs
    app.rs
    window/
      mod.rs
      imp.rs
Resources/
  AppIcon.svg
  style.css
  ui/
    window.ui
Tests/
  Unit/
```

The template must look production-grade, not toy-like.

It should include:

- app window shell
- adaptive layout
- proper action wiring
- `adw::Application` bootstrap
- `adw::ApplicationWindow` implemented with `CompositeTemplate`
- GResource-backed Builder XML for the main window
- icon, metadata, categories
- narrow default sandbox

## Quality Commands

### `orbit lint --platform linux`

Linux v1 quality stack:

- `cargo clippy`
- metadata validation
- Flatpak manifest lint

### `orbit format --platform linux`

Linux v1 formatting stack:

- `rustfmt`

Keep the existing Orbit-owned `quality` section but map it to Rust tooling.

## Tooling Requirements

`orbit ui doctor` is not the right analogue for full Linux apps.

Add a Linux application doctor/preflight command in the build path, or reuse general preflight output from `build`.

It must validate:

- `flatpak`
- `flatpak-builder`
- Flathub remote configured
- required SDK/runtime installed or installable
- Rust SDK/toolchain extension present for the chosen runtime
- `org.flatpak.Builder` present when submit/lint is requested

## File Layout In The Codebase

Recommended modules:

- `src/linux/mod.rs`
- `src/linux/manifest/mod.rs`
- `src/linux/manifest/normalize.rs`
- `src/linux/build/mod.rs`
- `src/linux/build/cargo.rs`
- `src/linux/build/resources.rs`
- `src/linux/build/flatpak.rs`
- `src/linux/build/metadata.rs`
- `src/linux/runtime.rs`
- `src/linux/submit/mod.rs`
- `src/linux/submit/flathub.rs`
- `src/linux/quality.rs`

Do not try to wedge Linux full-app build logic into `src/apple`.

## Build Receipts

Add Linux receipts parallel to Apple receipts.

Receipt fields should include:

- `platform = linux`
- `distribution = development|flatpak|flathub`
- `app_id`
- `version`
- `build`
- `artifact_path`
- `repo_path`
- `flatpak_manifest_path`
- `runtime_family`
- `runtime_version`
- `architectures`

## Hard Rules

- Orbit is the source of truth, not a hand-authored Flatpak manifest
- Flatpak metadata is generated
- Rust + GTK4 + libadwaita is the only first-class Linux app stack in v1
- portals are preferred over blanket permissions
- Flathub compliance is part of the implementation, not an afterthought
- do not add backward-compatibility branches for non-Flatpak packaging in v1

## What We Explicitly Defer

### Deferred to v2

- Qt6 + Kirigami app stack
- `org.kde.Platform` runtime support
- external/manual build-system ingestion
- AppImage exporter
- `deb` exporter
- `rpm` exporter
- Snap
- Linux UI automation integrated into `tests.ui`

### Deferred indefinitely unless needed

- generic “run any existing Cargo project” mode
- packaging arbitrary non-GTK desktop apps with Orbit-native UX

## Implementation Phases

### Phase 1

- add `linux-app.v1.json`
- add Linux manifest loader/normalizer
- add `orbit init` Linux GTK template

### Phase 2

- generate Cargo workspace
- run/build local app sources
- compile resources

### Phase 3

- generate Flatpak manifest
- build/install/run through `flatpak-builder`
- emit receipts and `.flatpak` bundles

### Phase 4

- generate desktop/metainfo/icon exports
- add lints for metadata and permissions

### Phase 5

- implement `submit --platform linux`
- generate Flathub-ready packaging repos
- run `flatpak-builder-lint`
- GitHub PR automation

### Phase 6

- add Linux UI automation on top of the built product
- wire `tests.ui`

## Sources

- Orbit repo:
  - current app-centric model in [README.md](/Users/ilyai/Developer/personal/orbit2/README.md)
  - current Apple schema in [schemas/apple-app.v1.json](/Users/ilyai/Developer/personal/orbit2/schemas/apple-app.v1.json)
  - current resolved manifest model in [src/apple/manifest/mod.rs](/Users/ilyai/Developer/personal/orbit2/src/apple/manifest/mod.rs)
- Flatpak:
  - [Flatpak documentation](https://docs.flatpak.org/)
  - [Building your first Flatpak](https://docs.flatpak.org/en/latest/first-build.html)
  - [Building](https://docs.flatpak.org/en/latest/building.html)
  - [Available runtimes](https://docs.flatpak.org/en/latest/available-runtimes.html)
  - [Dependencies](https://docs.flatpak.org/en/latest/dependencies.html)
  - [Sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
  - [Qt runtime guide](https://docs.flatpak.org/en/latest/qt.html)
- Flathub:
  - [Why Flathub?](https://docs.flathub.org/docs/for-app-authors/why-flathub)
  - [Requirements](https://docs.flathub.org/docs/for-app-authors/requirements)
  - [MetaInfo guidelines](https://docs.flathub.org/docs/for-app-authors/metainfo-guidelines)
  - [Submission](https://docs.flathub.org/docs/for-app-authors/submission)
  - [Flatpak builder lint](https://docs.flathub.org/docs/for-app-authors/linter)
  - [Verification](https://docs.flathub.org/docs/for-app-authors/verification)
- GNOME:
  - [GNOME libraries overview](https://developer.gnome.org/documentation/introduction/overview/libraries.html)
  - [libadwaita](https://gnome.pages.gitlab.gnome.org/libadwaita/)
  - [Adaptive layouts](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/adaptive-layouts.html)
  - [GNOME HIG](https://developer.gnome.org/hig/index.html)
- KDE, for deferred second backend:
  - [Kirigami getting started](https://develop.kde.org/docs/getting-started/kirigami/)
  - [Flatpak Qt guide](https://docs.flatpak.org/en/latest/qt.html)
