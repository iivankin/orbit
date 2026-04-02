# Linux UI Automation Spec

This document is a subsystem spec for Linux desktop UI automation. It is not the primary Linux application-platform spec. The main product/platform spec is [linux-application-spec.md](/Users/ilyai/Developer/personal/orbit2/docs/linux-application-spec.md).

## Scope

This spec is for a Linux equivalent of Orbit's current desktop UI automation backend:

- `orbit test --ui --platform macos`
- `orbit ui doctor|dump-tree|describe-point|focus|logs --platform macos`
- the current macOS YAML command set and artifact/report behavior

This spec is not for a full Linux equivalent of Orbit's Apple build/signing/submission pipeline.

Assumption:

- "this exact thing" means "replicate the current macOS desktop UI automation behavior for Linux desktop apps".

If that assumption is wrong, stop here and re-scope before implementation.

## Current Contract To Preserve

The Linux backend must preserve these user-visible behaviors from the current macOS backend:

- same YAML parser and runner
- same selector semantics: `text`, `id`
- same polling behavior for `tapOn`, `assertVisible`, `assertNotVisible`, `scrollUntilVisible`
- same report format shape
- same failure artifacts: screenshot + accessibility tree JSON
- same restriction as macOS for `launchApp`, `stopApp`, `clearState`: they operate only on Orbit's built target
- same backend-driven artifact extension model via `video_extension()`

The current desktop command surface to mirror is:

- `launchApp`
- `stopApp`
- `clearState`
- `tapOn`
- `hoverOn`
- `rightClickOn`
- `doubleTapOn`
- `longPressOn`
- `swipe`
- `swipeOn`
- `dragAndDrop`
- `scroll`
- `scrollOn`
- `scrollUntilVisible`
- `inputText`
- `pasteText`
- `setClipboard`
- `copyTextFrom`
- `eraseText`
- `pressKey`
- `pressKeyCode`
- `keySequence`
- `assertVisible`
- `assertNotVisible`
- `extendedWaitUntil`
- `waitForAnimationToEnd`
- `takeScreenshot`
- `startRecording`
- `stopRecording`
- `openLink`
- `logs`

The commands below remain out of scope for Linux v1, matching the current macOS backend's unsupported area:

- `clearKeychain`
- `setLocation`
- `setPermissions`
- `travel`
- `addMedia`
- `install-dylib`
- `instruments`
- `update-contacts`
- crash log commands

## Product Decision

Implement Linux UI automation as a first-class desktop backend, not as a helper hidden under `src/apple`.

Required structural change:

- extract the cross-platform UI runner, parser, report types, selector matching, artifact handling, and `UiBackend` trait out of `src/apple/testing/ui.rs`
- keep iOS and macOS backends under `src/apple/testing/ui/`
- add Linux-specific backend code under `src/linux/testing/ui/`

Do not put Linux logic under the Apple namespace. That will become unmaintainable immediately.

## Supported Linux Targets

### Linux v1 support matrix

- Wayland on GNOME
- Wayland on KDE Plasma
- X11 on GNOME, KDE Plasma, Xfce, and other EWMH-capable X11 sessions
- GTK 3 apps
- GTK 4 apps
- Qt 6 apps that expose AT-SPI correctly

### Explicitly unsupported in v1

- wlroots-specific environments as a support target promise
- Hyprland as a support target promise
- Sway as a support target promise
- apps that do not expose AT-SPI
- Flutter apps unless they expose a usable AT-SPI tree
- sandbox-specific behavior for Flatpak/Snap beyond the standard desktop portals

Reason:

- AT-SPI is the stable, standard Linux accessibility layer.
- Wayland input and screen capture must use XDG Desktop Portal plus PipeWire.
- X11 still has a stable synthetic input path via XTEST.
- wlroots-family desktop coverage is still too inconsistent to promise as a v1 support target.

## Backend Names

Use one public Linux backend family name and expose the session type in doctor output.

- backend name for reports: `orbit-a11y-linux`
- doctor output session subtype:
  - `session: wayland`
  - `session: x11`

Do not create two unrelated public backend names unless behavior diverges in a user-visible way.

## External Primitives

### Accessibility tree and semantic actions

Use AT-SPI for:

- app discovery
- accessibility tree dump
- describe-point
- names, roles, identifiers, values, frames
- focus
- semantic default action on actionable elements
- editable text when available
- semantic scroll-to when available

Validated basis:

- `org.a11y.atspi.Accessible` provides `Name`, `Description`, `AccessibleId`, `GetChildren`, and `GetRole`
- `org.a11y.atspi.Component` provides `GetAccessibleAtPoint`, `GetExtents`, `GrabFocus`, `ScrollTo`, and `ScrollToPoint`
- `org.a11y.atspi.Action` provides invokable actions for actionable controls
- `org.a11y.atspi.Text` provides readable text content

### Wayland input and capture

Use XDG Desktop Portal RemoteDesktop + ScreenCast + PipeWire.

Use the portal `Notify*` methods in v1:

- `NotifyPointerMotionAbsolute`
- `NotifyPointerButton`
- `NotifyPointerAxisDiscrete`
- `NotifyPointerAxis`
- `NotifyKeyboardKeycode`
- `NotifyKeyboardKeysym`

Do not use `ConnectToEIS` in v1.

Reason:

- the `Notify*` path is standardized and already exposed by `ashpd`
- `ConnectToEIS` adds extra complexity
- once EIS is connected, input must go through that path exclusively
- Orbit does not need that complexity for v1 parity

### X11 input

Use XTEST via `x11rb`.

Reason:

- XTEST is explicitly designed for synthetic input for testing with no user intervention
- it is stable and standard on X11

### URI opening

Use the OpenURI portal on Linux instead of shelling out to `xdg-open`.

Reason:

- it is standardized
- it works consistently on Wayland and X11
- it matches the desktop-security direction Orbit should align with

## Required Rust Dependencies

Pin exact versions in `Cargo.toml`.

Recommended additions:

- `ashpd = { version = "=0.13.9", default-features = false, features = ["tokio", "remote_desktop", "screencast", "open_uri", "pipewire"] }`
- `tokio = { version = "=1.50.0", features = ["rt", "time", "sync"] }`
- `atspi = { version = "=0.29.0", default-features = true }`
- `zbus = { version = "=5.14.0", default-features = false, features = ["tokio"] }`
- `pipewire = "=0.9.2"`
- `x11rb = "=0.13.2"`
- `xkbcommon = "=0.9.0"`
- `image = "=0.25.10"`

Notes:

- `atspi` is the right crate, not `test-by-a11y`. `test-by-a11y` is useful as proof that Linux accessibility-driven testing is viable, but it does not solve Orbit's portal, capture, input, artifact, or CLI integration needs.
- `zbus` is added explicitly because Orbit will need direct bus calls in addition to crate wrappers.

## CLI And Type Changes

### New UI platform enum

Do not reuse the Apple-only `TargetPlatform` enum for Linux UI support.

Introduce:

```rust
pub enum UiPlatform {
    Ios,
    Macos,
    Linux,
}
```

Use `UiPlatform` for:

- `TestArgs.platform` when `--ui` is set
- all `orbit ui ... --platform ...` argument structs

Keep the existing `TargetPlatform` enum Apple-only for build, run, submit, signing, and IDE paths.

Reason:

- adding `linux` to `TargetPlatform` would leak Linux into Apple-only build flows and force fake support branches everywhere
- the current coupling is already too tight for a clean Linux addition

### Session prep abstraction

Replace the Apple-specific `prepare_ui_session()` contract with a platform-neutral abstraction.

Introduce:

- `UiLaunchArtifact`
- `UiPreparedSession`
- `UiPlatformBackendFactory`

`UiLaunchArtifact` must contain only what the runner actually needs:

- `platform`
- `target_name`
- `app_id`
- `bundle_or_app_root`
- `executable_path`
- `receipt_path`

Do not make the shared runner depend on `crate::apple::build::pipeline::BuildOutcome`.

## File Layout

Recommended layout:

- `src/testing/mod.rs`
- `src/testing/ui/mod.rs`
- `src/testing/ui/parser.rs`
- `src/testing/ui/runner.rs`
- `src/testing/ui/report.rs`
- `src/testing/ui/backend.rs`
- `src/apple/testing/ui/ios_simulator.rs`
- `src/apple/testing/ui/macos.rs`
- `src/linux/mod.rs`
- `src/linux/testing/mod.rs`
- `src/linux/testing/ui/mod.rs`
- `src/linux/testing/ui/backend.rs`
- `src/linux/testing/ui/atspi.rs`
- `src/linux/testing/ui/portal.rs`
- `src/linux/testing/ui/x11.rs`
- `src/linux/testing/ui/capture.rs`
- `src/linux/ui.rs`

Do not keep the current giant mixed file shape once Linux is added.

## Linux App Identity

Use the reverse-DNS application id as the Linux equivalent of `bundle_id`.

Linux app id rules:

- same identifier string Orbit already uses for the desktop app identity
- also use it as the default desktop-file id and D-Bus application id later when Orbit grows Linux app packaging

Use this field internally as `app_id`, but keep it serialized to the existing shared report field name `bundle_id` only if changing report schema is not desirable yet.

## Linux Backend Architecture

### Top-level object

Add `LinuxBackend` implementing `UiBackend`.

State:

- `app_id: String`
- `app_root: PathBuf`
- `executable_path: PathBuf`
- `launched_process: Mutex<Option<Child>>`
- `last_tap_point: Mutex<Option<(f64, f64)>>`
- `session_kind: LinuxSessionKind`
- `atspi: LinuxAtspiClient`
- `capture: LinuxCaptureManager`
- `input: LinuxInputManager`
- `runtime: tokio::runtime::Runtime`

`LinuxSessionKind`:

- `Wayland`
- `X11`

Determine from:

- `$XDG_SESSION_TYPE`
- fallback detection if unset

### AT-SPI client

`LinuxAtspiClient` responsibilities:

- connect to the AT-SPI bus
- discover the target app by PID and app id
- dump the target app subtree into Orbit's normalized JSON shape
- hit-test a point
- resolve top-level window extents
- resolve focused window
- read text from text/value interfaces
- set text through `EditableText` when possible
- invoke semantic actions through `Action`
- request semantic focus through `Component.GrabFocus`

App discovery algorithm:

1. spawn the target executable
2. poll the desktop root's children
3. resolve each AT-SPI application's D-Bus unique name to a Unix PID via the session D-Bus daemon
4. pick the app whose PID matches the launched child
5. fallback to app-id/name matching only for already-running external inspection commands

Do not identify apps only by visible window title. That will break immediately.

### Tree normalization contract

Emit the same logical shape the shared runner already understands.

Each element dictionary should use:

- `AXRole`
- `AXSubrole` when available
- `AXLabel`
- `AXIdentifier`
- `AXValue`
- `frame`

Mapping rules:

- `AXRole`: AT-SPI role name
- `AXLabel`: `Accessible.Name`, then `Description`, then readable text/value
- `AXIdentifier`: `AccessibleId` when non-empty
- `AXValue`: text/value content when available
- `frame`: `Component.GetExtents(screen)`

Output format:

- flat JSON array of dictionaries, like the macOS helper

Reason:

- the existing runner already recursively scans arrays and maps
- preserving the AX-style keys avoids touching the selector logic

### Focus

`focus()` implementation:

1. resolve the best top-level target window in the app subtree
2. call `Component.GrabFocus` on that window
3. if needed, call `GrabFocus` on the app's currently focused or first focusable child
4. poll briefly until the window is focused or actionable

Do not shell out to `wmctrl` as the primary mechanism.

Use `wmctrl` only as an optional fallback if a later bug proves AT-SPI focus is insufficient on a specific X11 desktop.

### Input manager

#### Wayland path

Create a persistent portal session:

1. `RemoteDesktop.CreateSession`
2. `ScreenCast.SelectSources` on the same session with:
   - `types = MONITOR`
   - `multiple = true`
   - `cursor_mode = hidden`
3. `RemoteDesktop.SelectDevices` with:
   - `types = KEYBOARD | POINTER`
   - `persist_mode = 2`
   - `restore_token` if one was previously stored
4. `RemoteDesktop.Start`
5. store the new `restore_token`
6. store returned stream metadata

Persist the token in Orbit home, not in the project:

- `~/.orbit/ui/linux-portal-state.json`

Token record key:

- session type
- current desktop
- app id

Rationale:

- portal permission is user/machine/session scoped, not repo scoped

Coordinate mapping:

- keep all AT-SPI geometry in screen coordinates
- choose the screencast stream whose `position`/`size` contains the target window center
- convert screen coordinates to that stream's logical coordinate space before calling `NotifyPointerMotionAbsolute`

Pointer/button mapping:

- left click: Linux evdev button code `BTN_LEFT`
- right click: `BTN_RIGHT`
- double click: two left-click sequences with the existing Orbit delay semantics
- long press: left press, sleep, left release
- hover: absolute motion only

Keyboard mapping:

- use `NotifyKeyboardKeysym` for `pressKey`
- use `NotifyKeyboardKeycode` for `pressKeyCode`
- Linux `pressKeyCode` semantics must be documented as Linux evdev keycodes

Modifier handling:

- press modifiers down first
- press target key
- release target key
- release modifiers in reverse order

#### X11 path

Use `x11rb` with XTEST:

- pointer motion: fake motion events
- button events: fake button press/release
- keyboard events: fake key press/release

Keycode rules:

- Linux `pressKeyCode` still uses Linux evdev keycodes as the public Orbit contract
- convert evdev keycodes to XKB/X11 keycodes by applying the standard XKB offset when needed
- use `xkbcommon` to derive keysyms and modifier requirements for character-based `pressKey`

Reason:

- Orbit must not hardcode a US-layout-only Linux key map
- macOS-style manual key tables are not acceptable on Linux

### Capture manager

#### Screenshots

Wayland:

- read the latest PipeWire frame from the active monitor stream
- crop to the current top-level app window extents from AT-SPI
- encode PNG with the `image` crate

X11:

- capture the root window image
- crop to the target top-level app window extents
- encode PNG with the `image` crate

Do not rely on compositor-specific screenshot CLIs.

#### Video recording

Window-scoped recording must mirror current macOS behavior:

- lock the capture rect when recording starts
- record only that rect
- stop gracefully and wait for the file to finalize

Wayland:

- keep consuming PipeWire frames
- crop to the locked rect
- feed raw frames to `ffmpeg` over stdin

X11:

- sample frames from the X11 root window
- crop to the locked rect
- feed raw frames to `ffmpeg` over stdin

Recording container:

- use `.mkv`

Reason:

- `.mkv` with an always-available ffmpeg encoder path is more reliable than assuming H.264 availability
- the runner already supports backend-specific video extensions

`doctor` must require:

- `ffmpeg` on `PATH`

### Logging

Linux does not have a true equivalent of macOS unified logging for arbitrary desktop apps.

Implement logging in two layers:

- always tee Orbit-launched app `stdout` and `stderr` to terminal and to a per-run log file
- implement `orbit ui logs --platform linux` as best-effort `journalctl --user --follow _EXE=<path>` when `journalctl` is available

If `journalctl` is not available, return a clear unsupported error.

Do not block Linux UI support on a perfect standalone log streaming story.

### Clear state

Mirror the macOS backend's safety rule: only delete app-specific storage roots.

Delete these exact roots when present:

- `${XDG_CONFIG_HOME:-$HOME/.config}/<app_id>`
- `${XDG_DATA_HOME:-$HOME/.local/share}/<app_id>`
- `${XDG_STATE_HOME:-$HOME/.local/state}/<app_id>`
- `${XDG_CACHE_HOME:-$HOME/.cache}/<app_id>`

Do not delete shared toolkit state.

Do not recurse through arbitrary glob patterns.

## Command Mapping

### Semantic-first commands

Prefer AT-SPI semantic behavior before raw input for:

- `tapOn` when the element exposes `Action`
- `inputText` when the focused or last-tapped element exposes `EditableText`
- `scrollUntilVisible` when the target or container exposes `ScrollTo` or `ScrollToPoint`
- `focus` through `GrabFocus`

Fallback to raw input when the semantic path is missing or fails.

This matches the current macOS strategy of preferring accessibility actions for zero-duration taps.

### Raw-input commands

Always use raw input for:

- `hoverOn`
- `rightClickOn`
- `doubleTapOn`
- `longPressOn`
- `swipe`
- `swipeOn`
- `dragAndDrop`
- `scroll`
- `scrollOn`
- `pressKey`
- `pressKeyCode`
- `keySequence`
- `eraseText`

## `doctor` Contract

`orbit ui doctor --platform linux` must print:

- `ui backend: orbit-a11y-linux`
- `session: wayland|x11`
- `atspi: ok|missing`
- `input: ok|missing`
- `screen capture: ok|missing`
- `ffmpeg: ok|missing`
- `portal permissions: ok|missing` on Wayland

Wayland checks:

- AT-SPI bus reachable
- `org.freedesktop.portal.Desktop` reachable
- `RemoteDesktop` version >= 2
- `ScreenCast` reachable
- PipeWire reachable
- `ffmpeg` present
- stored restore token valid or interactive bootstrap succeeds

X11 checks:

- AT-SPI bus reachable
- X11 connection reachable
- XTEST extension present
- `ffmpeg` present

Wayland first-run behavior:

- if no valid restore token exists, `doctor` should open the permission flow and store the new token
- in non-interactive mode, fail with a clear message telling the user to run `orbit ui doctor --platform linux` once interactively

## Error Handling

Use actionable errors, not Linux jargon dumps.

Examples:

- missing AT-SPI:
  - "Linux UI automation requires the AT-SPI accessibility bus. Ensure the desktop accessibility stack is running and the target app exposes AT-SPI."
- missing portal permission:
  - "Wayland UI automation requires a one-time desktop approval for screen capture and input control. Run `orbit ui doctor --platform linux` interactively."
- unsupported target:
  - "The target app does not expose a usable AT-SPI tree, so Orbit cannot automate it on Linux."

## Tests

### Unit tests

Add pure-Rust tests for:

- Linux tree normalization
- selector matching against normalized Linux trees
- coordinate conversion from AT-SPI screen coords to portal stream logical coords
- X11 evdev-to-keycode conversion rules
- portal state persistence and token rotation
- `clearState` path resolution against XDG defaults and env overrides

### Integration tests

Do not start with live desktop CI.

Start with mocked integration tests equivalent to current iOS UI tests:

- mocked AT-SPI tree responses
- mocked portal session responses
- mocked PipeWire frame provider
- mocked `ffmpeg`
- mocked X11 XTEST connection

### Live/manual fixture

Add a Linux desktop fixture app equivalent to `examples/macos-app`.

Required fixture behaviors:

- text field + apply button
- hover target
- right-click target
- drag source/drop target
- keyboard shortcut target
- persisted state target
- scroll container with off-screen footer

Do not claim Linux parity until this fixture passes end-to-end.

## Rollout Plan

### Phase 1

- extract shared UI runner/parser/backend trait out of Apple namespace
- keep iOS and macOS behavior unchanged
- add `UiPlatform`

### Phase 2

- implement Linux AT-SPI tree inspection
- ship `orbit ui doctor --platform linux`
- ship `dump-tree`, `describe-point`, `focus`

### Phase 3

- implement X11 input path
- implement Wayland portal input path
- support `tapOn`, `hoverOn`, `rightClickOn`, `swipe`, `dragAndDrop`, `scroll`

### Phase 4

- implement screenshot capture
- implement MKV recording
- support failure artifacts and manual recording

### Phase 5

- wire Linux into `orbit test --ui`
- add Linux fixture app
- validate parity flow-by-flow

## Non-Goals

- Linux code signing
- Linux store submission
- sandbox escape tricks
- compositor-specific hacks as the primary path
- hardcoded US keyboard-only behavior
- adding Linux under Apple-only abstractions

## Main Risks

### Risk 1: app does not expose AT-SPI correctly

Mitigation:

- constrain the support promise to GTK and Qt apps with usable AT-SPI trees
- fail early in `doctor`

### Risk 2: Wayland first-run permission friction

Mitigation:

- bootstrap once through `doctor`
- persist restore tokens globally

### Risk 3: video recording complexity

Mitigation:

- use one capture pipeline for both screenshots and video
- use `.mkv` and ffmpeg stdin encoding for reliability

### Risk 4: keyboard layout variance

Mitigation:

- use `xkbcommon`
- avoid hardcoded Linux key tables for printable characters

## Final Recommendation

Build this as a real Linux desktop backend around:

- AT-SPI for semantics and tree inspection
- XDG Desktop Portal + PipeWire for Wayland input and capture
- XTEST for X11 input
- `xkbcommon` for key translation
- a shared cross-platform UI runner extracted from the current Apple file

Anything narrower will either fail on modern Wayland desktops or create the wrong abstraction boundaries in the Orbit codebase.

## Sources

- Orbit repo:
  - current desktop UI backend contract in `src/apple/testing/ui.rs`
  - macOS backend implementation in `src/apple/testing/ui/backend.rs`
  - macOS helper in `src/apple/testing/ui/macos_driver.swift`
  - documented backend coverage in `README.md`
  - fixture flows in `examples/macos-app/Tests/UI/`
- AT-SPI:
  - [Ubuntu accessibility stack](https://documentation.ubuntu.com/desktop/en/latest/explanation/accessibility-stack/)
  - [org.a11y.atspi.Accessible](https://documentation.ubuntu.com/desktop/en/latest/reference/accessibility/dbus/org.a11y.atspi.Accessible/)
  - [org.a11y.atspi.Component](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Component.html)
  - [org.a11y.atspi.Action](https://documentation.ubuntu.com/desktop/en/latest/reference/accessibility/dbus/org.a11y.atspi.Action/)
  - [org.a11y.atspi.Text](https://documentation.ubuntu.com/desktop/en/latest/reference/accessibility/dbus/org.a11y.atspi.Text/)
  - [org.a11y.atspi.DeviceEventController](https://documentation.ubuntu.com/desktop/en/latest/reference/accessibility/dbus/org.a11y.atspi.DeviceEventController/)
- XDG Desktop Portal:
  - [RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
  - [ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
  - [OpenURI](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.OpenURI.html)
- X11:
  - [XTEST Extension Protocol](https://x.org/releases/X11R7.7/doc/xextproto/xtest.html)
- Freedesktop specs:
  - [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/latest/)
  - [Desktop Entry Specification](https://specifications.freedesktop.org/desktop-entry-spec/latest-single/)
- Rust crates verified during research:
  - [ashpd 0.13.9](https://docs.rs/ashpd/latest/ashpd/)
  - [atspi 0.29.0](https://docs.rs/atspi/0.29.0/atspi/)
  - [pipewire 0.9.2](https://pipewire.pages.freedesktop.org/pipewire-rs/pipewire/)
  - [x11rb 0.13.2](https://docs.rs/x11rb/0.13.2/x11rb/)
  - [xkbcommon 0.9.0](https://docs.rs/xkbcommon/0.9.0/xkbcommon/xkb/)
