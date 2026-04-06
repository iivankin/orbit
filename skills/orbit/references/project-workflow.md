# Project Workflow

## First Pass

When entering an Orbit project:

1. Find `orbit.json`.
2. Read the manifest before touching code.
3. Determine the affected platform and target shape.
4. Check whether the manifest includes `tests`, `hooks`, `dependencies`,
   `extensions`, `watch`, or `app_clip`.
5. If the repo contains multiple manifests, decide which product you are editing
   and pass `--manifest` on every Orbit command.

Orbit resolves manifests in this order:

1. `--manifest`
2. `./orbit.json`
3. recursive search under the working directory

In non-interactive mode, multiple matching manifests are an error. Agents
should not rely on interactive manifest selection.

## Orbit Mental Model

- One `orbit.json` describes one product.
- The root object is the app, not an Xcode target graph.
- Embedded pieces live under the app:
  - `extensions`
  - `watch`
  - `app_clip`
- If code is not truly shared across platforms, prefer separate manifests.
- If two apps are really different products, they should not share a manifest.

## Working Rules

- Prefer small, manifest-aware changes instead of broad refactors.
- Read the manifest and the touched sources together. Orbit behavior is often a
  product of both.
- Treat `.orbit/` as generated state:
  - `.orbit/build` for build output
  - `.orbit/artifacts` for diagnostics and profiling artifacts
  - `.orbit/receipts` for packaged build receipts
  - `.orbit/orbit.lock` for resolved dependency state
- Do not hand-edit `.orbit` contents unless the user explicitly asks for it.
- If behavior is unclear, rerun the relevant command with `--verbose`.

## Useful Environment Knobs

- `ORBIT_SCHEMA_DIR`
- `ORBIT_DATA_DIR`
- `ORBIT_CACHE_DIR`
- `ORBIT_XCODE_SEARCH_ROOTS`
- `ORBIT_APPLE_TEAM_ID`

`ORBIT_APPLE_TEAM_ID` matters in CI and other non-interactive signing or submit
flows.

## Validation Ladder

Run the lowest-cost command that answers the question, then move upward:

1. `orbit format`
2. `orbit lint`
3. `orbit test`
4. `orbit test --ui --platform <platform>` when UI flows are relevant
5. `orbit run --platform <platform> ...` for runtime verification
6. `orbit build --platform <platform> --distribution <kind>` when packaging,
   signing, or artifact shape is affected

Only reach for `orbit submit` when the user explicitly wants a real submission.

## Ask Before

Ask the user before:

- cleaning with `orbit clean --all`
- importing or exporting signing material
- mutating Apple device state
- mutating remote signing or submission state
- picking one manifest or platform when multiple plausible choices exist

## Orbit Repo Mode

If the current repository is the Orbit CLI source itself:

- use `README.md` as the public contract
- use CLI `--help` as the shipped surface
- use `examples/` as sample downstream projects
- use `tests/` to confirm current behavior before updating docs or skills
