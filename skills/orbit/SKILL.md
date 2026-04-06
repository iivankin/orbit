---
name: orbit
description: Work with Orbit CLI app projects and `orbit.json` manifests. Use when a task involves the Orbit Apple app build/sign/submit CLI, `orbit.json`, `.orbit/`, commands such as `orbit init`, `orbit lint`, `orbit format`, `orbit test`, `orbit run`, `orbit build`, `orbit submit`, `orbit ui`, or `orbit apple`, or when an LLM agent needs project-specific guidance for editing and validating Orbit-managed app projects.
---

# Orbit

Use this skill when a repository is driven by `orbit.json` and Orbit CLI.

## Quick Rules

- Find the exact `orbit.json` first.
- If there is more than one manifest, choose intentionally and pass `--manifest`.
- If the manifest declares more than one platform, pass `--platform`.
- Treat `.orbit/` as generated state. Inspect it when useful, but do not edit it by hand.
- Prefer repeatable invocations with `--non-interactive`.
- Retry with `--verbose` before guessing how Orbit behaves.
- Ask before commands with meaningful side effects:
  - `orbit submit`
  - `orbit clean --all`
  - mutating `orbit apple ...` commands
  - signing import/export against real credentials

## Read This First

- General workflow and guardrails: [references/project-workflow.md](references/project-workflow.md)
- Manifest shape and authoring rules: [references/manifest-guide.md](references/manifest-guide.md)
- Command selection and validation flow: [references/command-guide.md](references/command-guide.md)

## UI Tests

Read these when a task touches `tests.ui`, `orbit test --ui`, or `orbit ui`:

- Overview and debugging flow: [references/ui-tests-overview.md](references/ui-tests-overview.md)
- YAML/YML syntax and supported parser forms: [references/ui-test-yaml.md](references/ui-test-yaml.md)
- Backend support and platform caveats: [references/ui-test-platforms.md](references/ui-test-platforms.md)

The parser accepts more commands than every backend supports. Always check the
platform support file before authoring or changing a flow.

## Inside The Orbit CLI Repository

If you are working in the Orbit source repository itself:

- Treat `README.md`, CLI `--help`, `examples/`, and `tests/` as the behavior contract.
- Keep docs and skills aligned with shipped behavior, not aspirational design.
- Use example apps and example UI flows as canonical downstream fixtures.
