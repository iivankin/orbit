# Chrome CDP UI Testing Spec

Status: draft  
Owner: Orbit  
Last verified: 2026-04-01

## Verified target

This spec is pinned to Chrome for Testing Dev `148.0.7753.0`.

Verified on 2026-04-01:

- Chrome for Testing `last-known-good-versions-with-downloads.json` reported:
  - Stable: `147.0.7727.24`
  - Beta: `147.0.7727.24`
  - Dev: `148.0.7753.0`
  - Canary: `148.0.7766.0`
- A locally launched Chrome for Testing Dev `148.0.7753.0` returned:
  - `Browser: Chrome/148.0.7753.0`
  - `Protocol-Version: 1.3`
  - live `/json/protocol` payload size: `1615390` bytes
- Runtime sanity checks confirmed:
  - `Accessibility.getFullAXTree` returns accessible nodes with `backendDOMNodeId`
  - `DOM.querySelector` and `DOM.getBoxModel` work against the same page
  - `Page.startScreencast` emits `Page.screencastFrame` events in headless mode

## Goal

Implement a separate `orbit-cdp` CLI that reuses Orbit's YAML flow language and reporting model, but runs against Chrome DevTools Protocol instead of iOS/macOS automation.

The tool is for deterministic local and CI execution against a managed Chrome for Testing 148 binary. It is not a generic browser automation framework.

## Non-goals

- No Playwright, Puppeteer, Selenium, or WebDriver layer
- No support for non-Chrome browsers in v1
- No support for arbitrary Chrome versions in v1
- No integration into the main `orbit` CLI in v1
- No attempt to unify Apple and Chrome backends behind the current Apple-specific `UiBackend` trait
- No HTML5 drag-and-drop or OS file drag-and-drop in v1
- No HAR export, tracing UI, or Lighthouse integration in v1

## Version contract

The runner must reject mismatched browser versions.

Required startup checks:

- Launch or attach to Chrome for Testing `148.0.7753.0`
- Call `Browser.getVersion`
- Require `product == "Chrome/148.0.7753.0"`
- Require major version `148`
- Require `protocolVersion == "1.3"`

If any check fails, abort with a hard error and explain that milestone `148` was not stable on 2026-04-01 and that Orbit is intentionally pinned to the verified Dev build.

## Public surface

### CLI

This tool is a separate binary, not a subcommand.

Binary name:

- `orbit-cdp`

Primary invocation:

```sh
orbit-cdp run ./Tests/UI/login.yaml --output ./artifacts/login
orbit-cdp run ./Tests/UI --output ./artifacts/full-run --base-url http://127.0.0.1:3000
```

Required arguments:

- input path
  - a single `.yml` or `.yaml` flow
  - or a directory that will be scanned recursively for flows
- `--output <dir>`

Optional flags:

- `--base-url <url>`
- `--chrome <path>`
- `--headless`
- `--headed`
- `--viewport <width>x<height>`
- `--mobile`
- `--touch`
- `--locale <locale>`
- `--timezone <timezone>`

Authoring/debug commands:

- `orbit-cdp doctor`
- `orbit-cdp dump-tree --output ./artifacts/tree.json`
- `orbit-cdp describe-point --x 140 --y 142 --output ./artifacts/point.json`

`doctor` checks:

- Chrome executable exists
- Chrome launches with `--remote-debugging-port=0`
- reported version is exactly `148.0.7753.0`
- `ffmpeg` exists on `PATH`

### Flow config

The CLI is intentionally file-oriented. There is no manifest dependency.

Optional top-level config keys for `orbit-cdp` flows:

- `name`
- `baseUrl`
- `startUrl`
- `headless`
- `viewport`
- `mobile`
- `touch`
- `locale`
- `timezone`

CLI flags override YAML config.

Validation rules:

- top-level flow documents must remain Maestro-style YAML
- `baseUrl` is optional, but required when a flow uses relative URLs
- `startUrl` defaults to `/`
- `viewport.width` and `viewport.height` must be positive integers
- `headless` defaults to `true`
- `touch` defaults to `mobile`

### Hard cutover flow semantics

`orbit-cdp` does not reuse Apple `appId` semantics.

For `orbit-cdp`:

- `launchApp` means browser navigation
- `launchApp` accepts either:
  - bare `launchApp`
  - `launchApp: "/relative-path"`
  - `launchApp: "https://absolute.url"`
  - `launchApp: { url: "/login", clearState: true, permissions: ... }`

`appId` is not supported in `orbit-cdp` flows.

## File layout

Do not wire this into `src/cli.rs` or `src/apple/testing/ui/*`.

Separate binary entrypoint:

- `src/bin/orbit-cdp.rs`

Create shared UI flow model modules:

- `src/testing/ui/model.rs`
- `src/testing/ui/parser.rs`
- `src/testing/ui/report.rs`

Create CDP-specific modules:

- `src/cdp/cli.rs`
- `src/cdp/testing/ui.rs`
- `src/cdp/testing/ui/runner.rs`
- `src/cdp/testing/ui/cdp_client.rs`
- `src/cdp/testing/ui/browser.rs`
- `src/cdp/testing/ui/session.rs`
- `src/cdp/testing/ui/selector.rs`
- `src/cdp/testing/ui/video.rs`
- `src/cdp/testing/ui/helper.js`
- `src/cdp/ui.rs`

Apple runner code may later import the shared model/parser/report modules, but `orbit-cdp` must ship independently from the main `orbit` command surface.

## Browser lifecycle

### Discovery

Chrome executable resolution order:

1. `--chrome`
2. `ORBIT_CDP_CHROME_148_PATH`
3. known local Chrome for Testing cache path
4. fail

### Launch

Launch Chrome with:

- `--remote-debugging-port=0`
- `--user-data-dir=<output dir>/chrome-profile`
- `--no-first-run`
- `--no-default-browser-check`
- `about:blank`
- `--headless=new` when configured

Read the WebSocket browser endpoint from `DevToolsActivePort` or stderr.

### Session model

Per test run:

- One Chrome process
- One disposable `BrowserContext`
- One primary page target
- One root page session
- Auto-attached child sessions for OOPIFs and workers

Required CDP setup:

- browser target:
  - `Browser.getVersion`
  - `Target.createBrowserContext`
  - `Target.createTarget`
- root page session:
  - `Target.setAutoAttach(autoAttach=true, waitForDebuggerOnStart=false, flatten=true)`
  - `Page.enable`
  - `Runtime.enable`
  - `DOM.enable`
  - `Network.enable`
  - `Log.enable`
  - `Page.addScriptToEvaluateOnNewDocument`
- child page/OOPIF sessions:
  - `Page.enable`
  - `Runtime.enable`
  - `DOM.enable`
  - `Network.enable`
  - `Log.enable`
- worker sessions:
  - `Runtime.enable`
  - `Log.enable`

Cleanup:

- `Target.closeTarget` for the page
- `Target.disposeBrowserContext`
- kill Chrome process

## Selector model

The Chrome backend must not rely on repeated full-tree AX plus per-node `DOM.getBoxModel` calls. That approach is too expensive for Orbit's polling model.

Instead, Orbit uses an injected helper script in an isolated world and resolves selectors per poll.

### Helper responsibilities

The helper returns a flat list of candidates for the current frame/session. Each candidate includes:

- DOM order index
- tag name
- role
- `text_candidates`
- `id_candidates`
- `copied_text`
- `visible`
- `scrollable`
- `disabled`
- `checked`
- `rect`

`text_candidates` must be built from:

- `aria-label`
- associated `<label>` text
- `innerText`
- input `value`
- `placeholder`
- `alt`
- `title`
- `name`

`id_candidates` must be built from:

- `id`
- `data-testid`
- `data-test-id`
- `data-qa`
- `data-cy`
- `name`

Visibility must require:

- non-zero bounding box
- `display != none`
- `visibility != hidden`
- `opacity != 0`
- viewport intersection

### Match scoring

Use the same score semantics as the existing Apple matcher:

- exact match: `3`
- case-insensitive exact match: `2`
- case-insensitive contains: `1`

For Chrome:

- total score is `text_score + id_score`
- prefer visible candidates
- then higher score
- then candidates with a rect
- then earlier DOM order

### Frame search

Search order:

1. root page session
2. child page sessions in frame-tree order

The selector engine returns a resolved element with:

- session id
- frame id
- label
- copied text
- rect
- center point
- scrollable ancestor center if known

## Command support

### Supported in v1

- `launchApp`
  - web meaning: ensure page exists and navigate to `launchApp.url` or flow `startUrl`
  - `clearState` is honored
  - `permissions` is honored
- `stopApp`
  - close current page target
- `killApp`
  - alias of `stopApp`
- `clearState`
  - dispose current browser context and create a fresh one
- `tapOn`
- `hoverOn`
- `rightClickOn`
- `tapOnPoint`
- `doubleTapOn`
- `longPressOn`
- `swipe`
- `swipeOn`
- `scroll`
- `scrollOn`
- `scrollUntilVisible`
- `inputText`
- `pasteText`
- `setClipboard`
- `copyTextFrom`
- `eraseText`
- `pressKey`
- `hideKeyboard`
  - implemented as blur/no-op, not OS keyboard control
- `assertVisible`
- `assertNotVisible`
- `extendedWaitUntil`
- `waitForAnimationToEnd`
- `takeScreenshot`
- `startRecording`
- `stopRecording`
- `openLink`
- `setLocation`
- `setPermissions`
- `runFlow`
- `repeat`
- `retry`

### Explicitly unsupported in v1

- `dragAndDrop`
- `clearKeychain`
- `pressKeyCode`
- `keySequence`
- `pressButton`
- `travel`
- `addMedia`

Unsupported commands must fail during flow preflight before execution starts.

## Command to CDP mapping

### Navigation and page lifecycle

- `launchApp` and `openLink`
  - `Page.navigate`
  - `Page.reload` when relaunching same URL with `clearState == false`
- `stopApp`
  - `Target.closeTarget`

### Pointer actions

- desktop mode:
  - `Input.dispatchMouseEvent`
- touch mode:
  - `Input.synthesizeTapGesture`
  - `Input.synthesizeScrollGesture`
  - `Emulation.setTouchEmulationEnabled`

Before any pointer action:

- resolve selector
- scroll candidate into view with helper or `DOM.scrollIntoViewIfNeeded`
- recompute rect

### Text input

- focus target with helper or `DOM.focus`
- type with `Input.insertText`
- send backspace and named keys with `Input.dispatchKeyEvent`

### Screenshots and recording

- screenshot: `Page.captureScreenshot`
- recording:
  - `Page.startScreencast`
  - receive `Page.screencastFrame`
  - ack with `Page.screencastFrameAck`
  - finalize with `Page.stopScreencast`

### Emulation

- viewport: `Emulation.setDeviceMetricsOverride`
- geolocation: `Emulation.setGeolocationOverride`
- locale: `Emulation.setLocaleOverride`
- timezone: `Emulation.setTimezoneOverride`

### Permissions

Use `Browser.setPermission` with a curated allowlist only:

- `geolocation`
- `notifications`
- `camera`
- `microphone`
- `clipboard-read`
- `clipboard-write`

State mapping:

- `allow` -> `granted`
- `deny` -> `denied`
- `unset` -> `prompt`

### Logging

Enable and stream:

- `Runtime.consoleAPICalled`
- `Runtime.exceptionThrown`
- `Log.entryAdded`
- `Network.loadingFailed`

## Recording format

Video output remains `.mp4`.

Implementation:

- store screencast frames as JPEG files under a temp directory
- store frame timestamps from `ScreencastFrameMetadata.timestamp`
- build an ffmpeg concat manifest with per-frame durations
- encode to H.264 MP4 with `yuv420p`

If `ffmpeg` is missing:

- `orbit-cdp doctor` fails
- test execution fails before the first flow starts

## Reporting

Use the existing flow and step report structure, but define a Chrome-specific run envelope.

Required run fields:

- `id`
- `runner: "chrome-cdp"`
- `input_path`
- `output_dir`
- `browser_product`
- `browser_protocol_version`
- `browser_context_id`
- `page_target_id`
- `base_url`
- `start_url`
- `report_path`
- `artifacts_dir`
- `status`
- `flows`

Failure artifacts:

- failure screenshot PNG
- failure selector snapshot JSON
- console log tail JSONL

## Dump-tree and describe-point

`dump-tree` for Chrome returns Orbit's helper snapshot, not raw DOM or raw AX.

Each entry includes:

- session id
- frame id
- tag
- role
- text candidates
- id candidates
- copied text
- visible
- rect

`describe-point` uses:

- `DOM.getNodeForLocation`
- helper evaluation for tag, text, ids, role, and rect

## Tests

### Unit

- manifest validation for `runner == chrome-cdp`
- selector scoring
- key mapping
- artifact naming
- unsupported command preflight

### Real browser e2e

Add `tests/e2e_orbit_cdp.rs` with a static fixture server and managed Chrome 148.

Required e2e coverage:

- navigate and assert text
- tap and input text
- hover and right click
- scroll until visible
- swipe in touch mode
- clearState resets cookies and localStorage
- screenshot file written
- auto-recorded MP4 written
- console logs and JS exceptions captured
- permission override for geolocation
- unsupported command fails before run start

## Rollout

### Phase 1

- extract shared UI model/parser/report modules

### Phase 2

- implement Chrome launcher, CDP client, selector engine, and MVP command set

### Phase 3

- add `doctor`, `dump-tree`, and `describe-point`

### Phase 4

- add request interception, downloads, and WebAuthn as separate follow-up work

## Sources

- Chrome for Testing versions JSON:
  - https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json
- Chrome for Testing known-good versions:
  - https://googlechromelabs.github.io/chrome-for-testing/known-good-versions-with-downloads.json
- Chrome DevTools Protocol home:
  - https://chromedevtools.github.io/devtools-protocol/
- Relevant CDP domains:
  - https://chromedevtools.github.io/devtools-protocol/tot/Browser/
  - https://chromedevtools.github.io/devtools-protocol/tot/Target/
  - https://chromedevtools.github.io/devtools-protocol/tot/Page/
  - https://chromedevtools.github.io/devtools-protocol/tot/Runtime/
  - https://chromedevtools.github.io/devtools-protocol/tot/DOM/
  - https://chromedevtools.github.io/devtools-protocol/tot/Accessibility/
  - https://chromedevtools.github.io/devtools-protocol/tot/Input/
  - https://chromedevtools.github.io/devtools-protocol/tot/Network/
  - https://chromedevtools.github.io/devtools-protocol/tot/Fetch/
  - https://chromedevtools.github.io/devtools-protocol/tot/Emulation/
  - https://chromedevtools.github.io/devtools-protocol/tot/Log/
  - https://chromedevtools.github.io/devtools-protocol/tot/Tracing/
  - https://chromedevtools.github.io/devtools-protocol/tot/WebAuthn/
