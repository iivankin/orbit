# Windows Desktop UI Backend Spec

## Scope

This spec is for the Windows counterpart of Orbi's current macOS desktop UI automation backend and its fixture coverage.

It is **not** a Windows port of Orbi's Apple build/sign/submit pipeline.

Concretely, the thing being ported is the desktop-testing surface that currently lives in:

- `src/apple/testing/ui.rs`
- `src/apple/testing/ui/backend.rs`
- `src/apple/testing/ui/macos_driver.swift`
- `examples/macos-app`

The target outcome is: Orbi can drive a Windows desktop fixture app with the same YAML runner, the same report/artifact model, and equivalent tested flows.

## Goals

- Preserve the existing Orbi YAML flow format and runner semantics.
- Preserve the existing artifact model:
  - screenshots
  - per-flow video
  - failure screenshot
  - failure hierarchy
  - JSON report
- Preserve the same desktop command subset that macOS currently exposes in README and tests:
  - `launchApp`
  - `stopApp`
  - `clearState`
  - `tapOn`
  - `hoverOn`
  - `rightClickOn`
  - `dragAndDrop`
  - `swipe`
  - `scroll`
  - `scrollUntilVisible`
  - `inputText`
  - `pressKey`
  - `assertVisible`
  - `takeScreenshot`
  - video recording
  - `openLink`
  - `logs`
- Keep unsupported commands explicitly unsupported with stable error messages, the same way the current macOS backend does.
- Add a Windows example app plus UI flows that mirror the current macOS example coverage.

## Non-goals

- No attempt to add Windows into `ApplePlatform`.
- No attempt to make the Apple build pipeline target Windows.
- No simulator/device layer.
- No Windows Store submission/signing work in this spec.
- No backward-compat shim that aliases Apple concepts to Windows concepts implicitly.

## Existing Contract To Preserve

### Shared Runner Contract

The current runner in `src/apple/testing/ui.rs` already provides the behavior we want to keep:

- YAML parser and command model
- selector matching by visible text and accessibility identifier
- polling-based assertions and waits
- nested flows, retry, repeat
- screenshots and per-flow recordings
- JSON report shape

This should become OS-agnostic shared code.

### Current macOS Desktop Coverage

The current macOS backend in `src/apple/testing/ui/backend.rs` supports:

- launch/stop/clear-state for Orbi's built app only
- accessibility tree dump
- point inspection
- focus
- pointer gestures
- keyboard input including modifiers
- screenshot
- window-scoped video recording
- open link
- log streaming through macOS `log stream`

The Windows backend must match this contract as closely as Windows allows.

### Current Fixture Coverage

The current example flows in `examples/macos-app/Tests/UI/` cover:

- smoke text entry and assertion
- right click
- hover
- drag and drop
- scroll until visible
- repeated scroll command
- clear state
- keyboard shortcut

Windows must ship with the same flow coverage.

## Product Decision

### Exact Scope Of The Windows Port

Implement a new backend named:

- `orbi-uia-windows`

This backend is the Windows equivalent of:

- `orbi-ax-macos`

It targets ordinary Windows desktop apps that expose Microsoft UI Automation data.

### Minimum Supported OS

- Backend API floor: Windows 10 version 1903 / build 18362
- Fixture project and CI baseline: Windows 10 version 2004 / build 19041
- Recommended: Windows 11

Reason:

- UI Automation is older, but window-scoped capture via `IGraphicsCaptureItemInterop::CreateForWindow` requires Windows 10 version 1903.
- Per-flow video is a first-class Orbi artifact, so capture is a hard requirement for this backend.
- The WinUI 3 fixture should target `net8.0-windows10.0.19041.0` and be tested on 19041+, which keeps the project/runtime story simpler than trying to optimize around the raw 18362 capture floor.

### Integrity-Level Rule

Orbi and the application under test must run at the same integrity level.

Reason:

- Windows `SendInput` is blocked by UIPI when targeting a higher-integrity process.

If Orbi tries to drive an elevated target from a non-elevated process, the backend must fail with a clear message instead of silently flaking.

## Required Architectural Cutover

This work should not be implemented under `apple::testing::ui`.

That namespace is already the wrong abstraction boundary.

### New Module Layout

Create a shared UI-testing module outside `apple`:

```text
src/testing/ui.rs
src/testing/ui/parser.rs
src/testing/ui/backend.rs
src/testing/ui/runner.rs
src/testing/ui/selector.rs
src/testing/ui/backends/ios_simulator.rs
src/testing/ui/backends/macos.rs
src/testing/ui/backends/windows.rs
```

Then keep thin platform adapters:

```text
src/apple/ui.rs
src/apple/testing.rs
```

The existing macOS and iOS implementations move into the shared backend tree with no behavior change beyond path moves.

### Hard Cutover Rule

Do not add Windows by extending `ApplePlatform`.

Instead:

- keep Apple build/run concepts inside `apple::*`
- make the YAML runner and backend trait generic
- let Apple and Windows backends plug into the same runner

This is the only maintainable direction.

## Windows Backend Architecture

## High-Level Shape

Implement Windows as:

1. A Rust backend in Orbi that implements `UiBackend`.
2. A Windows-native sidecar executable:
   - `orbi-windows-ui-driver.exe`
3. A Windows example fixture app:
   - WinUI 3 on .NET 8 using the Windows App SDK

### Why A Sidecar

Use a sidecar for Windows for the same reason macOS uses one:

- isolates platform-specific UIA/WinRT state
- isolates capture and input failures from the main CLI
- gives us a stable helper CLI surface for debugging
- keeps the main runner orchestration simple

### Why WinUI 3 For The Fixture

Use WinUI 3 for the example app.

Reasons:

- this is the user requirement
- it is Microsoft's current native desktop XAML stack
- WinUI 3 windows are still HWND-based, so the backend window-resolution and capture model stays correct
- first-class UI Automation support is available through `AutomationProperties.AutomationId` and standard control peers
- `KeyboardAccelerator`, `ContextFlyout`, `CanDrag`, `AllowDrop`, and pointer events cover the fixture interactions we need
- it keeps the fixture representative of the modern Windows desktop stack instead of validating only legacy XAML behavior

The backend itself must remain framework-agnostic and work with any Windows desktop app that exposes UI Automation properly.

### Fixture Packaging Model

The fixture app should be a **packaged MSIX** WinUI 3 desktop app.

But unlike the standard Microsoft setup, Orbi must not rely on the Visual Studio project system.

### No Visual Studio Project System

This is a hard requirement.

The Windows implementation must not depend on:

- checked-in `.sln` files
- checked-in `.csproj` or `.vcxproj` files
- `msbuild`
- `dotnet build`
- `devenv`
- Visual Studio-managed compile/link graphs

Orbi must own:

- source discovery
- dependency resolution
- compile command construction
- link command construction
- runtime asset staging
- output bundle layout

Allowed tools:

- low-level compilers and linkers
- package extractors/resolvers
- metadata/resource tools

Disallowed pattern:

- "generate a normal Visual Studio project and let MSBuild handle the real work"

### WinUI 3 Consequence

This requirement materially changes the fixture design.

Microsoft's official WinUI 3 documentation assumes:

- new WinUI 3 apps are packaged by default
- app-authored XAML is compiled as part of the build, and normal XAML compilation steps should remain enabled
- package identity is expressed through an app package manifest and MSIX packaging/deployment flow

Therefore, Orbi must add an explicit **XAML compilation phase** to the Windows build pipeline instead of falling back to code-first-only UI.

The v1 fixture should now be:

- **WinUI 3**
- **MSIX packaged**
- **app-authored XAML**
- **Orbi-compiled**
- **Orbi-packaged**

That means:

- `App.xaml` is allowed
- `MainWindow.xaml` is allowed
- page/control/resource-dictionary XAML files are allowed
- Orbi, not MSBuild, is responsible for compiling that XAML into generated code and runtime assets

This keeps the fixture genuinely WinUI 3 and preserves the normal WinUI authoring model while still honoring the no-project-system requirement.

### Orbi-Owned XAML Compilation

Orbi must treat WinUI XAML compilation as a first-class build stage.

Tooling direction:

- resolve the compiler and companion IO libraries from the pinned WinUI / Windows App SDK tooling payload that Orbi stages itself
- use the standalone `XamlCompiler.exe` entrypoint directly
- do not shell out to `msbuild` targets just to reach the compiler

Known public contract from Microsoft's source:

- `XamlCompiler.exe <input-json> <output-json>`
- the compiler consumes a JSON input model
- the compiler emits a JSON output model listing:
  - generated code files
  - generated XAML files
  - generated XAML page files
  - generated XBF files

Orbi must reproduce the effective WinUI build shape that Microsoft's interop targets use:

1. Discover authored XAML inputs:
   - `App.xaml`
   - page XAML
   - user-control XAML
   - resource dictionaries
2. Resolve compiler references and metadata:
   - app assemblies
   - WinUI assemblies
   - .NET reference assemblies
   - Windows metadata and WinRT interop metadata
3. Run a real **pass 1** equivalent:
   - emit input JSON with `IsPass1=true`
   - include `XamlApplications`, `XamlPages`, `ReferenceAssemblies`, `ReferenceAssemblyPaths`, `TargetPlatformMinVersion`, `SavedStateFile`, and the WinUI feature/control flags Orbi chooses to support
   - invoke `XamlCompiler.exe`
   - parse generated code / generated XAML / generated XBF output lists
4. Compile an **intermediate local assembly** from:
   - user-authored `.cs`
   - pass-1 generated `.g.cs`
5. Run a real **pass 2** equivalent:
   - emit input JSON with `IsPass1=false`
   - pass `LocalAssembly=<intermediate assembly>`
   - preserve and reuse the same `SavedStateFile`
   - include `RootsLog` and any SDK XAML pages if Orbi later adopts them
   - invoke `XamlCompiler.exe`
   - parse the final generated code / generated XAML / generated XBF output lists
6. Compile the final managed assembly from:
   - user-authored `.cs`
   - final generated `.g.cs`
7. Stage generated XAML/XBF payloads into the final app package staging directory using the same relative-path logic Orbi assigned to the authored XAML inputs.
8. Preserve compiler logs, JSON manifests, generated file lists, and saved state in intermediates for debuggability.

V1 scope note:

- Orbi only needs the real build pipeline, not Visual Studio design-time IntelliSense compilation
- Orbi does not need to implement the full MSBuild target surface; it needs the equivalent runtime-build behavior of `MarkupCompilePass1 -> XamlPreCompile -> MarkupCompilePass2 -> generated XAML/XBF staging`

Intermediates should live under an Orbi-owned path such as:

```text
.orbi/intermediates/windows/<target>/<configuration>/
```

Minimum persisted artifacts:

- `xaml/pass1/input.json`
- `xaml/pass1/output.json`
- `xaml/pass2/input.json`
- `xaml/pass2/output.json`
- `xaml/XamlSaveStateFile.xml`
- optional roots log such as `xaml/<assembly>.xr.xml`
- generated `.g.cs`
- generated `.xbf`
- copied/generated XAML payloads

### Managed Compile And Packaging Stages

After XAML compilation, Orbi's Windows pipeline must:

1. Compile the intermediate assembly required by pass 2.
2. Compile the final app assembly from authored plus generated sources.
3. Stage an MSIX package layout that contains:
   - the final executable and managed assemblies
   - generated XAML/XBF payloads
   - package assets
   - the package manifest
   - any PRI/resource outputs Orbi decides to generate
4. Produce a signed `.msix` from that staging layout.
5. Stage any companion install/test artifacts Orbi wants for CI, such as certificate material or install scripts.

Orbi must build the compile graph itself.

It may use low-level language compilers, but it must not delegate the graph to `dotnet build` or `msbuild`.

### Orbi-Owned MSIX Packaging

Orbi must treat MSIX packaging as another first-class build stage.

Required responsibilities:

1. Generate or materialize `AppxManifest.xml` from Orbi-controlled metadata.
2. Stage package visual assets and other manifest-referenced files.
3. Generate `resources.pri` with `MakePri.exe` or equivalent MRT tooling when the package layout requires PRI-backed resources.
4. Pack the staged layout into `.msix` using `MakeAppx.exe` or equivalent AppX packaging APIs.
5. Sign the package with `SignTool.exe` or equivalent signing APIs.
6. Record package identity outputs needed by the runner:
   - package full name
   - package family name
   - application id
   - AUMID (`<packageFamilyName>!<applicationId>`)

V1 scope:

- single-application package only
- single-architecture CI target only
- sideload/testing certificate flow is acceptable
- no Visual Studio-generated `Add-AppDevPackage.ps1`

### MSIX Manifest Requirements In V1

The packaged WinUI 3 fixture must have an Orbi-owned manifest with at least:

- `<Identity>` with Orbi-controlled name, publisher, version, and architecture
- `TargetDeviceFamily` for desktop Windows
- one desktop `Application` entry for the fixture executable
- visual elements and package assets required for installation and launch
- Windows App SDK framework dependency declarations required for packaged deployment
- `runFullTrust` restricted capability for the packaged desktop app

The package should remain a full-trust packaged desktop app in v1, not an AppContainer conversion experiment.

### Supported XAML Features In V1

The Orbi-owned XAML pipeline must support the authored features needed by the fixture:

- standard page/window XAML
- `x:Name`
- code-behind partial classes
- resource dictionaries
- styles
- templates used by built-in controls
- `KeyboardAccelerator`
- `MenuFlyout` / `ContextFlyout`
- compiled bindings via `{x:Bind}`

Reason:

- Microsoft documents that `{x:Bind}` is converted into generated code at XAML compile time and produces generated `.g.cs` partial-class code in the build output
- without real XAML compilation, the fixture would lose an important part of normal WinUI authoring semantics

### XAML Compilation Risk

This is a materially larger build-systems task than the earlier code-first-only approach.

Reason:

- Microsoft's standard guidance is still project-centric
- XAML compilation, generated code, and runtime asset staging are normally hidden behind the project system

This is acceptable, but it should be planned as dedicated build-system work, not treated as a minor fixture tweak.

### Packaged Runtime Model

Because the fixture is MSIX-packaged, it must run with package identity and must **not** use the Windows App SDK Bootstrapper API as its startup mechanism.

Required behavior:

- declare the package manifest entries required for packaged WinUI 3 desktop startup
- keep the app framework-dependent in v1
- fail install or launch with a clear error if required framework dependencies are missing on the machine

Deployment notes:

- use the Windows App SDK Stable channel only
- re-check the latest stable patch before implementation and pin it exactly in Orbi's dependency metadata
- stage or install the required Windows App SDK framework dependencies as part of Orbi's MSIX test/install flow
- do not rely on project-file auto-generated packaging helpers

V1 rule:

- packaged MSIX is the only supported deployment model
- do not support unpackaged or packaged-with-external-location variants in this backend

### Package Install, Reset, And Launch Model

Orbi must own the full install and launch lifecycle for the fixture package.

Install/update flow:

1. Build the signed `.msix`.
2. Install or update it with `Add-AppxPackage` or equivalent deployment APIs.
3. Query the installed package metadata with `Get-AppxPackage` / `Get-AppxPackageManifest` or equivalent deployment APIs.
4. Resolve the app's:
   - package full name
   - package family name
   - application id
   - AUMID

Launch flow:

1. Activate the installed app by AUMID.
2. Use `IApplicationActivationManager::ActivateApplication` or equivalent shell activation API.
3. Capture the returned PID.
4. Resolve the top-level window from that PID.

Reset flow for `clearState`:

1. Stop the running app if needed.
2. Reset the installed package to original settings with `Reset-AppxPackage` or equivalent package-management APIs.
3. Relaunch through the normal AUMID activation path when the test flow continues.

## Backend Identity

`WindowsDesktopBackend` must report:

- `backend_name() -> "orbi-uia-windows"`
- `target_name() ->` the resolved top-level window title if available, otherwise `"Windows"`
- `target_id() ->` PID string
- `video_extension() -> "mp4"`
- `requires_running_target_for_recording() -> true`

## Driver CLI Contract

The Windows helper must mirror the macOS helper command shape wherever possible.

### Commands

- `doctor`
- `window-info --pid <pid>`
- `describe-all --hwnd <hwnd>`
- `describe-point --x <x> --y <y>`
- `focus --hwnd <hwnd>`
- `tap --x <x> --y <y> [--duration-ms <ms>]`
- `move --x <x> --y <y>`
- `right-click --x <x> --y <y>`
- `swipe --start-x <x> --start-y <y> --end-x <x> --end-y <y> [--duration-ms <ms>] [--delta <px>]`
- `drag --start-x <x> --start-y <y> --end-x <x> --end-y <y> [--duration-ms <ms>] [--delta <px>]`
- `scroll --direction up|down|left|right`
- `scroll-at-point --x <x> --y <y> --direction up|down|left|right`
- `text --text <text>`
- `set-value-at-point --x <x> --y <y> --text <text>`
- `key --keycode <vk> [--duration-ms <ms>] [--modifiers control,shift,alt,win]`
- `screenshot --hwnd <hwnd> --output <png>`
- `record-start --hwnd <hwnd> --output <mp4>`
- `record-stop`

`record-start` / `record-stop` are explicit instead of inheriting long-running state into the backend process. The helper owns the recording session state.

### JSON Output Contract

`describe-all` and `describe-point` must emit JSON objects/arrays that the existing selector logic can consume with only minimal shared-runner changes.

Each serialized element must include:

- `name`: `CurrentName`
- `identifier`: `CurrentAutomationId`
- `value`: current value text when present
- `controlType`: localized or canonical UIA control type name
- `offscreen`: `CurrentIsOffscreen`
- `enabled`: `CurrentIsEnabled`
- `scrollable`: boolean if `ScrollPattern` is supported
- `frame`:
  - `x`
  - `y`
  - `width`
  - `height`

Optional fields:

- `nativeWindowHandle`
- `frameworkId`
- `className`
- `clickablePoint`

Example:

```json
[
  {
    "name": "Apply",
    "identifier": "apply-button",
    "controlType": "Button",
    "offscreen": false,
    "enabled": true,
    "scrollable": false,
    "frame": {
      "x": 640,
      "y": 320,
      "width": 96,
      "height": 32
    }
  }
]
```

## Exact API Choices

### Accessibility Tree And Point Inspection

Use Microsoft UI Automation COM APIs.

Primary APIs:

- `IUIAutomation::ElementFromHandle`
- `IUIAutomation::ElementFromPoint`
- `IUIAutomationElement::FindAllBuildCache`
- `IUIAutomationElement` current/cached properties

Rules:

- Resolve the app root from the selected main window `HWND`.
- Use `FindAllBuildCache(TreeScope_Subtree, TrueCondition)` from the root to materialize the tree efficiently.
- Do not walk the entire tree with `TreeWalker` for normal dump operations.

Reason:

- Microsoft explicitly notes that `TreeWalker` cross-process navigation is less efficient than `FindAll` / `FindFirst`.

### Window Resolution

When Orbi launches the AUT:

1. Activate the installed package by AUMID and capture the returned PID.
2. Call `WaitForInputIdle(process, timeout)` when the activated process supports it.
3. Enumerate top-level windows with `EnumWindows`.
4. Filter by PID using `GetWindowThreadProcessId`.
5. Keep only visible candidate top-level windows.
6. Prefer the largest visible top-level window.
7. Use `DwmGetWindowAttribute(..., DWMWA_EXTENDED_FRAME_BOUNDS)` for capture bounds.

If no visible top-level window appears within 3 seconds, fail the same way the macOS backend does for window readiness.

### Focus

To focus:

1. If minimized, call `ShowWindow(hwnd, SW_RESTORE)`.
2. Call `SetForegroundWindow(hwnd)`.
3. Retry for up to 3 seconds.

If foreground activation still fails, return a backend error that mentions Windows foreground restrictions.

### Click / Hover / Right Click / Swipe / Drag / Scroll

Use `SendInput`.

Rules:

- Pointer coordinates are in screen space.
- For hover-only, move cursor without button state.
- For click/drag/swipe, emit full button down/move/up sequences.
- For scroll:
  - vertical uses `MOUSEEVENTF_WHEEL`
  - horizontal uses `MOUSEEVENTF_HWHEEL`

Default constants:

- swipe duration: `500ms`
- drag duration: `650ms`
- path delta: `5px`
- right-click pause between down/up: `40ms`
- pre/post drag hold: `80ms`

These should match the current macOS backend defaults as closely as possible.

### Text Input

Use a two-stage strategy:

1. `set-value-at-point`
   - resolve element with `ElementFromPoint`
   - walk parent chain if needed
   - if `ValuePattern` is supported and not read-only, append text using `SetValue`
2. fallback `text`
   - focus target window/element
   - inject Unicode keystrokes with `KEYBDINPUT` + `KEYEVENTF_UNICODE`

This is intentionally different from macOS clipboard fallback. Windows has a first-class Unicode keyboard injection path, so use it instead of mutating the clipboard.

### Keyboard Input

Use `SendInput` keyboard events with virtual-key codes.

Modifier mapping:

- `CONTROL` -> `VK_CONTROL`
- `SHIFT` -> `VK_SHIFT`
- `OPTION` -> `VK_MENU`
- `COMMAND` -> `VK_LWIN`
- `FUNCTION` -> unsupported in v1

Important rule:

- Do **not** silently remap `COMMAND` to `CONTROL`.

That would make flows look cross-platform while actually encoding hidden Windows-specific behavior.

Windows fixture flows should use `CONTROL` explicitly.

### Tappable Element Invocation Heuristic

For `tapOn` and similar commands, the shared runner should continue to resolve an element to a point.

To improve reliability on Windows, extend shared matching so a serialized element can optionally expose a `clickablePoint`.

Resolution order:

1. `clickablePoint` if present
2. center of `frame`

The helper should populate `clickablePoint` from `IUIAutomationElement::GetClickablePoint` when available.

### Screenshot

Use `Windows.Graphics.Capture` with `IGraphicsCaptureItemInterop::CreateForWindow(hwnd)` and save a single PNG frame.

This is the required implementation, not an optional optimization.

Do not use `PrintWindow` or GDI capture as primary behavior.

Reason:

- GDI/`PrintWindow` is not reliable for modern GPU-backed windows.
- Orbi's desktop recording/screenshot artifacts should behave consistently with the visible rendered window.

### Video Recording

Use `Windows.Graphics.Capture` plus H.264 MP4 encoding.

Required behavior:

- record only the AUT window
- one recording per top-level flow unless the flow uses manual recording commands
- stop cleanly at end of flow
- if recording fails before file creation, mark the flow failed

Implementation requirements:

- capture source: `CreateForWindow(hwnd)`
- format: H.264 in `.mp4`
- nominal frame rate: 30 FPS
- even output dimensions
- hardware acceleration enabled where the encoder allows it

The capture border should remain enabled in v1.

Reason:

- disabling the border requires additional consent/capability handling that is not needed for functional parity
- border handling does not affect Orbi correctness

### Open Link

Use `ShellExecuteW` or equivalent shell open behavior.

### Logs

Windows does not have a generic equivalent to macOS `log stream --process <name>`.

Therefore the Windows `logs` contract is:

- supported only for the Orbi-launched process
- stream inherited stdout/stderr from the launched child when available
- if the process was not launched by Orbi, or if the app is a GUI app with no console stream, return a clear unsupported message

Explicitly do **not** build v1 around:

- debugger attachment
- `OutputDebugString`
- ETW session plumbing

Reason:

- `OutputDebugString` is debugger-oriented and does nothing without a debugger/system debugger path
- ETW is useful, but not the closest equivalent to the current macOS backend contract

## Shared Runner Changes Required

The existing shared selector code must be extended for Windows data.

### Selector Matching

Extend `match_element_object` and supporting helpers to read:

- text candidates:
  - `name`
  - `value`
  - `title`
  - `label`
  - existing Apple keys
- id candidates:
  - `identifier`
  - `automationId`
  - existing Apple keys

Keep the same score ordering:

- exact
- case-insensitive exact
- case-insensitive contains

### Visibility Filtering

Current visibility filtering uses frame intersection only.

For Windows, add:

- if `offscreen == true`, treat the element as not visible even if the frame intersects the inferred screen

### Scroll Container Discovery

Current scroll discovery is Apple-role-specific.

Add support for Windows by accepting:

- `scrollable == true`

This is better than trying to maintain a brittle list of Windows control-type strings.

## Windows Backend Support Matrix

### Supported In V1

- `launchApp`
- `stopApp`
- `clearState`
- `tapOn`
- `hoverOn`
- `rightClickOn`
- `tapOnPoint`
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
- `logs` with the restricted contract above
- `dump-tree`
- `describe-point`
- `doctor`

### Explicitly Unsupported In V1

- `clearKeychain`
- `setLocation`
- `setPermissions`
- `travel`
- `addMedia`
- `install-dylib`
- `instruments`
- `update-contacts`
- crash log commands
- `pressButton`

These must fail with backend-specific error messages, mirroring the current macOS behavior.

### `hideKeyboard`

Implement as a no-op success in v1.

Reason:

- desktop Windows apps do not have an Orbi-managed soft keyboard concept analogous to iOS
- this matches the current macOS backend behavior

## `clearState` Contract

Like macOS, `clearState` is supported only for the Orbi-launched app under test.

Because the fixture is MSIX-packaged, persist state with package-scoped application data, not ad-hoc host filesystem roots.

For the Windows fixture app, standardize state under:

- `ApplicationData.Current.LocalFolder\\OrbiFixture`
- `ApplicationData.Current.LocalFolder\\OrbiFixture\\state.json`

Primary backend behavior:

- stop the app if it is running
- reset the installed MSIX package to original settings with `Reset-AppxPackage` or equivalent package-management APIs
- do not manually delete broad package-store roots as the primary implementation

Do not add registry cleanup in v1.

Reason:

- keeps cleanup deterministic
- uses the package manager's reset mechanism instead of brittle filesystem heuristics
- matches the MSIX-only deployment model

## Doctor Command Contract

`orbi ui doctor --platform windows` must print:

- `ui backend: orbi-uia-windows`
- `uiautomation: ok|missing`
- `graphics capture: ok|missing`
- `video encoder: ok|missing`

Checks:

- UI Automation COM activation succeeds
- `GraphicsCaptureSession::IsSupported()` returns true
- recording pipeline prerequisites initialize

No permission preflight analogous to macOS Accessibility is required in v1.

## Windows Example Fixture App

Create:

```text
examples/windows-app/
```

Use:

- WinUI 3
- .NET 8
- Windows App SDK Stable channel
- packaged desktop deployment via signed `.msix`
- `AutomationProperties.AutomationId`
- no Visual Studio project files
- app-authored XAML compiled by Orbi's XAML pipeline

Project shape:

```text
examples/windows-app/orbi.json
examples/windows-app/Sources/App/main.cs
examples/windows-app/Sources/App/App.xaml
examples/windows-app/Sources/App/App.xaml.cs
examples/windows-app/Sources/App/MainWindow.xaml
examples/windows-app/Sources/App/MainWindow.xaml.cs
examples/windows-app/Sources/App/Controls/*.xaml
examples/windows-app/Sources/App/Controls/*.xaml.cs
examples/windows-app/Sources/App/Resources/*.xaml
examples/windows-app/Sources/App/Services/*.cs
examples/windows-app/Package/AppxManifest.xml
examples/windows-app/Package/Assets/*
examples/windows-app/Package/*.xml
examples/windows-app/Tests/UI/*.yaml
```

Implementation constraints:

- keep the default system title bar in v1
- do not use a custom non-client title bar
- use a single top-level `MainWindow`
- keep the main test surface in a single root visual tree
- keep the primary fixture surface in `MainWindow.xaml` plus small supporting resource dictionaries or user controls only when they materially improve clarity
- keep the manifest `Application Id` stable so Orbi can derive a stable AUMID
- do not add explicit Bootstrapper API startup code; packaged startup should come from package identity and manifest dependencies

Reason:

- this keeps the top-level HWND story simple
- it avoids introducing non-client/title-bar automation noise into the fixture
- it makes PID-to-window resolution deterministic
- it keeps the build graph under Orbi's control while preserving normal WinUI authoring semantics
- it aligns the fixture with the MSIX-only product decision

### Required UI Surface

The fixture must mirror the macOS example behavior:

- title label:
  - text: `Orbi Windows fixture`
  - id: `fixture-title`
- name text box:
  - id: `name-field`
- apply button:
  - id: `apply-button`
- greeting label:
  - id: `greeting-label`
- secondary click target:
  - id: `secondary-click-area`
- secondary click result:
  - id: `secondary-click-status`
- drag source:
  - id: `drag-source`
- drop target:
  - id: `drop-target`
- hover target:
  - id: `hover-target`
- hover result:
  - id: `hover-status`
- keyboard shortcut area:
  - id: `shortcut-capture-area`
- shortcut result:
  - id: `shortcut-status`
- persist state button:
  - id: `persist-state-button`
- persisted state label:
  - id: `persisted-state-label`
- scroll container:
  - id: `fixture-scroll`
- scroll footer:
  - id: `scroll-footer`

### WinUI 3 Interaction Implementation Rules

Implement the fixture using WinUI 3 idioms, not Win32 interop shortcuts.

Author the fixture primarily in XAML with normal WinUI code-behind:

- `App.xaml` for application resources
- `MainWindow.xaml` for the main fixture surface
- optional supporting user-control or resource-dictionary XAML when needed
- `.xaml.cs` files for event handlers and fixture state coordination

Do not replace normal WinUI XAML authoring with custom Win32 painting or raw HWND message handling just to avoid the XAML pipeline.

#### Automation IDs

- assign `AutomationProperties.AutomationId` on every test-targeted element
- treat these IDs as stable test surface, not incidental implementation detail

#### Keyboard Shortcut

Implement the shortcut fixture with WinUI 3 keyboard accelerators:

- use `Ctrl+Shift+K`
- define a `KeyboardAccelerator` in authored XAML
- handle the `Invoked` event and set the status label
- set `args.Handled = true`

If accelerator routing needs explicit scoping for focus-heavy layouts, override `OnProcessKeyboardAccelerators` on the page/root and call `TryInvokeKeyboardAccelerator(args)` as documented.

Do **not** implement the shortcut fixture with `RegisterHotKey` or raw window-message interception in v1.

#### Right Click

- attach a `ContextFlyout` to the secondary-click target
- use a `MenuFlyoutItem` action that updates the status label

#### Hover

- use `PointerEntered` on the hover target to update the status label
- a tooltip is optional and not sufficient by itself; the fixture must change visible state so Orbi can assert it

#### Drag And Drop

- set `CanDrag="True"` on the drag source
- set `AllowDrop="True"` on the drop target
- use `DragStarting` to populate text payload
- use `DragOver` to accept the operation
- use `Drop` to read the payload and update the status label

#### Persistence

- store persisted state under `ApplicationData.Current.LocalFolder\\OrbiFixture`
- file I/O inside that package-scoped root is fine, but the root itself should come from `ApplicationData.Current`

### Shortcut Behavior

Use:

- `Ctrl+Shift+K`

Reason:

- this is the natural Windows counterpart to the current macOS shortcut fixture
- Microsoft documents `Control` as a standard keyboard-accelerator modifier for Windows apps
- Windows key chords are poor defaults for application shortcuts

### Persistence Behavior

Persist:

- default value: `Clean slate`
- after button press: `Persisted state restored`

Store the persisted message in the standardized Orbi fixture state root described above using normal file I/O.

## Windows Example UI Flows

Add:

```text
examples/windows-app/Tests/UI/smoke.yaml
examples/windows-app/Tests/UI/right-click.yaml
examples/windows-app/Tests/UI/hover.yaml
examples/windows-app/Tests/UI/drag-drop.yaml
examples/windows-app/Tests/UI/scroll.yaml
examples/windows-app/Tests/UI/scroll-command.yaml
examples/windows-app/Tests/UI/clear-state.yaml
examples/windows-app/Tests/UI/shortcut.yaml
```

These should mirror the macOS flow intent exactly, with the shortcut flow changed to `CONTROL + SHIFT + K`.

## Testing Plan

### Rust Unit Tests

Add unit tests for:

- Windows selector matching via `name`
- Windows selector matching via `identifier`
- `offscreen` visibility filtering
- `scrollable` scroll-container discovery
- Windows artifact extension handling (`mp4`)

### Driver Unit Tests

Add Windows-only tests for:

- top-level window resolution by PID
- JSON serialization shape
- key modifier mapping
- `clearState` package reset invocation

### Fixture Flow Tests

On a Windows CI runner, run:

```sh
orbi test --ui --platform windows
```

Build precondition:

- the installable signed `.msix` must be produced by Orbi's Windows build pipeline, not by a checked-in Visual Studio project
- CI should fail if `.sln`, `.csproj`, or `.vcxproj` files are introduced under `examples/windows-app`

Required passing flows:

- smoke
- right-click
- hover
- drag-drop
- scroll
- scroll-command
- clear-state
- shortcut

### Failure Artifact Test

Add one intentional Windows-only failing flow in test infrastructure, not in the public example app, to assert:

- failure screenshot written
- failure hierarchy written
- report marks failed step and flow

## Implementation Sequence

### Phase 1

- Move shared UI runner/parser/selector code out of `apple::*`
- Keep iOS and macOS behavior unchanged

### Phase 2

- Add Orbi-owned Windows build plumbing for an authored-XAML WinUI 3 fixture
- Add Windows dependency metadata and exact version pinning for Windows App SDK runtime inputs
- Add Orbi-owned XAML compilation support:
  - pass 1
  - intermediate local-assembly compile
  - pass 2
  - generated XAML/XBF staging
- Add Orbi-owned MSIX packaging support:
  - manifest generation
  - PRI generation as needed
  - package packing
  - package signing
  - install/update metadata output

### Phase 3

- Add `WindowsDesktopBackend`
- Add `orbi-windows-ui-driver.exe`
- Implement doctor/tree/point/focus/input/gesture/open-link/screenshot
- Implement packaged install/activate/reset plumbing

### Phase 4

- Implement window-scoped recording
- Implement restricted `logs`
- Add fixture app and flows

### Phase 5

- Add Windows CI job for the example fixture flows

## Acceptance Criteria

This work is done only when all of the following are true:

1. `orbi ui doctor --platform windows` reports the backend ready on a supported Windows machine.
2. `orbi ui dump-tree --platform windows` returns JSON consumable by the shared selector logic.
3. `orbi test --ui --platform windows` passes the full Windows fixture flow suite.
4. Each top-level flow writes screenshots/video/report artifacts using the same report model as current desktop backends.
5. The Windows fixture and backend build without checked-in Visual Studio project files.
6. The Windows fixture uses Orbi-owned compile/link/staging steps rather than delegating to the Visual Studio project system.
7. The Windows fixture's authored `App.xaml`, `MainWindow.xaml`, and supporting XAML compile successfully through Orbi's XAML pipeline, including generated `.g.cs` and staged XAML/XBF outputs.
8. Orbi produces a signed installable `.msix`, installs it, resolves its package identity metadata, and launches the app through packaged activation.
9. Unsupported commands fail explicitly and deterministically.
10. Apple behavior remains unchanged.

## Source Notes

The Windows API choices above are based on official Microsoft documentation:

- WinUI 3 is Microsoft's current desktop XAML framework and is part of the Windows App SDK
- WinUI windowing documentation says the XAML `Window` and `AppWindow` model is based on the Win32 HWND model
- Microsoft now documents that a new WinUI 3 app is packaged by default, and recommends packaged MSIX for new apps and enterprise deployment scenarios
- Microsoft documents that package-identity-gated Windows features are available to packaged apps, and that full MSIX packaging gives full package identity
- the Windows App SDK Bootstrapper API is explicitly for unpackaged or packaged-with-external-location desktop apps, not for normal packaged startup
- Microsoft documents framework-dependent packaged deployment around an MSIX plus package-manifest dependency declarations on the Windows App SDK framework package
- Microsoft recommends using the Deployment API for packaged apps when you need Main/Singleton packages or servicing flow outside the Store; for v1 Orbi should keep the fixture surface inside the packaged framework dependency path unless a feature requires more
- Microsoft documents PowerShell package-management cmdlets for MSIX, including `Add-AppxPackage`, `Remove-AppxPackage`, `Get-AppxPackageManifest`, and `Reset-AppxPackage`
- Microsoft documents that packaged desktop apps needing full trust declare the `runFullTrust` restricted capability
- Microsoft documents manual MSIX creation with `MakeAppx.exe`
- Microsoft documents manual package signing with `SignTool.exe`
- Microsoft documents manual PRI generation with `MakePri.exe` for packaged resources
- WinUI keyboard accelerators are the documented shortcut mechanism, and can be scoped programmatically with `OnProcessKeyboardAccelerators` and `TryInvokeKeyboardAccelerator`
- WinUI drag and drop is documented around `CanDrag`, `AllowDrop`, `DragStarting`, `DragOver`, and `Drop`
- `AutomationProperties.AutomationId` is the documented attached property for stable UI Automation identifiers
- Windows App SDK performance guidance says app-authored XAML is compiled as part of the build and that normal XAML compilation steps should remain enabled
- Microsoft documents app-local data storage via `ApplicationData.Current.LocalFolder` / `LocalCacheFolder`
- Microsoft's WinUI XAML compiler source exposes a standalone `XamlCompiler.exe <input-json> <output-json>` entrypoint plus serializable `CompilerInputs` / `CompilerOutputs` contracts
- Microsoft's WinUI interop targets show the effective runtime-build structure as `MarkupCompilePass1 -> XamlPreCompile -> MarkupCompilePass2`, reusing `SavedStateFile`, feeding `LocalAssembly` into pass 2, and copying generated XAML/XBF payloads into the output layout
- UI Automation is explicitly positioned for automated testing and desktop UI access: `entry-uiauto-win32`
- UI Automation control patterns are the standard way to expose/manipulate behavior such as invoke and scroll: `uiauto-controlpatternsoverview`
- `ElementFromHandle` and `ElementFromPoint` provide root/point resolution
- `FindAllBuildCache` is preferred over tree walking for efficient subtree enumeration
- `GetClickablePoint` is the documented way to obtain a click target for an element
- `SendInput` is the standard synthesized mouse/keyboard API, and it is blocked by UIPI
- `KEYEVENTF_UNICODE` via `KEYBDINPUT` supports Unicode text injection
- `WaitForInputIdle`, `EnumWindows`, `GetWindowThreadProcessId`, `ShowWindow`, and `SetForegroundWindow` cover process/window readiness and focus behavior
- `GetWindowRect` docs explicitly call out `DWMWA_EXTENDED_FRAME_BOUNDS` for visible window bounds
- `IGraphicsCaptureItemInterop::CreateForWindow` is the required window-scoped capture primitive on supported Windows builds
- `GraphicsCaptureSession.IsSupported` gates capture availability
- capture-to-video guidance uses `Windows.Graphics.Capture` as the window/video frame source
- `OutputDebugString` is debugger-oriented and therefore not a good generic log-stream substrate for v1

Inference from those sources:

- because Microsoft's standard WinUI 3 guidance is project-centric and hides both the XAML and packaging pipelines behind project targets, Orbi must explicitly recreate the runtime build graph itself: XAML pass 1, intermediate assembly compile, XAML pass 2, generated XAML/XBF staging, PRI generation when needed, MSIX manifest/resource staging, package packing/signing, package reset, and packaged activation/install flow
