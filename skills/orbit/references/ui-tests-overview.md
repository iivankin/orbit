# UI Tests Overview

## Manifest Shape

Orbit UI tests are declared under `tests.ui` in `orbit.json`.

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

`tests.ui` currently uses an Orbit-native runner for a Maestro-style YAML
subset.

## Running UI Tests

- iOS simulator:
  `orbit test --ui --platform ios`
- macOS:
  `orbit test --ui --platform macos`
- specific flow:
  `orbit test --ui --platform macos --flow onboarding-provider-setup`

Orbit can also profile test runs:

- `orbit test --ui --platform ios --trace`
- `orbit test --ui --platform macos --trace`

## Debugging Commands

Useful helpers around UI tests:

- `orbit ui doctor --platform macos`
- `orbit ui dump-tree --platform ios`
- `orbit ui describe-point --platform ios --x 140 --y 142`
- `orbit ui focus --platform ios`
- `orbit ui logs --platform ios -- --timeout 1s`

Use those before rewriting selectors blindly.

## Flow Authoring Model

Each flow is a `.yaml` or `.yml` document under the configured `tests.ui`
source roots.

Recommended authoring pattern:

- keep one flow per file
- use `name` for readable reports
- use `orbit test --flow ...` when you need to run one top-level flow without changing the manifest
- use `runFlow` for reuse
- keep setup explicit near the top of the flow
- prefer stable `id` selectors where possible

See:

- [ui-test-yaml.md](ui-test-yaml.md) for syntax
- [ui-test-platforms.md](ui-test-platforms.md) for runtime support

## Outputs And Artifacts

Orbit currently writes useful artifacts during UI runs:

- screenshots from `takeScreenshot`
- screen recordings
- a JSON report
- app logs during `orbit test --ui`

README also states that each top-level flow writes an `.mp4` recording alongside
screenshots and the JSON report.

## Trace Caveats

Traced UI runs are stricter than normal UI runs.

When profiling UI tests:

- only one `launchApp` is supported across the traced suite
- `launchApp` must happen before runtime interaction commands
- `clearState`, `clearKeychain`, and `setPermissions` must stay in the prelaunch region
- app lifecycle and other prelaunch-sensitive commands inside `retry` blocks are rejected
- recursive `runFlow` chains are rejected

If trace mode fails but ordinary UI runs work, check the flow structure before
assuming the app is broken.
