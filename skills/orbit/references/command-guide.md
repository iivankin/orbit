# Command Guide

## Global Flags

Useful flags across Orbit commands:

- `--manifest <path>` to pick the exact product
- `--platform <platform>` when the manifest is multi-platform
- `--non-interactive` for repeatable agent runs
- `--verbose` for deeper diagnostics

`orbit init` is the main exception: it requires an interactive terminal.

## Formatting And Linting

Use these first after source or manifest edits:

- check formatting: `orbit format`
- write formatting changes: `orbit format --write`
- run lint and semantic checks: `orbit lint`
- lint one platform explicitly: `orbit lint --platform ios`

## Tests

- unit tests: `orbit test`
- iOS UI tests: `orbit test --ui --platform ios`
- macOS UI tests: `orbit test --ui --platform macos`
- profiling: `orbit test --trace` or `orbit test --trace memory`

UI tests use the manifest's `tests.ui` configuration.

## UI Helper Commands

Use these when debugging flows or simulator/app state:

- `orbit ui doctor --platform macos`
- `orbit ui dump-tree --platform ios`
- `orbit ui describe-point --platform ios --x 140 --y 142`
- `orbit ui focus --platform ios`
- `orbit ui logs --platform ios -- --timeout 1s`
- `orbit ui open --platform ios https://example.com`
- `orbit ui add-media --platform ios ./Tests/Fixtures/cat.jpg`
- `orbit ui crash --platform ios list`

`orbit ui` helpers are often the fastest way to debug selector mistakes before
rewriting a YAML flow.

## Run

Use `run` for fast runtime verification.

- iOS simulator: `orbit run --platform ios --simulator`
- iOS device: `orbit run --platform ios --device`
- specific device: `orbit run --platform ios --device --device-id <id>`
- attach debugger: `orbit run --platform ios --device --debug`
- profile a launch: `orbit run --platform ios --simulator --trace`

## Build

Use `build` when artifact shape, signing, distribution, or packaging matters.

- development build:
  `orbit build --platform ios --distribution development`
- App Store release build:
  `orbit build --platform ios --distribution app-store --release`
- macOS Developer ID release build:
  `orbit build --platform macos --distribution developer-id --release`

Rules:

- `--distribution` defaults to development, but agents should usually pass it explicitly.
- `--release` selects Release instead of Debug.
- use `--output` only when the user wants a specific artifact path.

## Submit

- latest matching receipt:
  `orbit submit --platform ios --wait`
- explicit receipt:
  `orbit submit --receipt .orbit/receipts/<receipt>.json --wait`

Only run `submit` with explicit user intent. It performs real remote side
effects.

## Dependencies

- all git-backed dependency intent:
  `orbit deps update`
- single dependency:
  `orbit deps update OrbitGreeting`

Run this when dependency declarations changed and Orbit needs to refresh
resolved revisions.

## IDE And Diagnostics

- `orbit ide install-build-server`
- `orbit ide dump-args --platform ios --file Sources/App/App.swift`
- `orbit bsp`
- `orbit inspect-trace .orbit/artifacts/profiles/run.trace`

Use these for editor integration, compiler-argument inspection, BSP setup, or
trace inspection.

## Apple Utilities

These commands inspect or mutate Apple account/device/signing state:

- `orbit apple device list --refresh`
- `orbit apple device register --current-machine`
- `orbit apple device import --file ./devices.csv`
- `orbit apple signing export --platform ios --output-dir ./signing`
- `orbit apple signing import --platform ios --distribution development --p12 ./signing.p12 --password <password>`

Ask the user before mutating device or signing state.

## Clean

- `orbit clean --all`

Treat this as destructive. It removes local Orbit state and may also remove
Orbit-managed remote Apple resources.

## Suggested Validation Recipes

For ordinary source edits:

1. `orbit format`
2. `orbit lint`
3. `orbit test`

For UI flow edits:

1. `orbit test --ui --platform <platform>`
2. `orbit ui dump-tree --platform <platform>` or `orbit ui describe-point ...`
3. rerun with `--verbose` if behavior is unclear

For packaging or signing edits:

1. `orbit lint`
2. `orbit test`
3. `orbit build --platform <platform> --distribution <kind>`

For dependency edits:

1. `orbit deps update`
2. `orbit lint`
3. `orbit test`
4. `orbit build ...` if packaging changed
