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

## Product Sources Of Truth

- Manifest field docs live in the `orbit.json` schema. Prefer editor/schema help over handwritten markdown.
- Workflow and command docs live in `orbit --help` and `orbit <command> --help`.
- UI flow grammar and backend support live in `orbit ui schema [--platform ...]`.
- Use `examples/` for canonical manifest shapes and example UI flows.

## Inside The Orbit CLI Repository

If you are working in the Orbit source repository itself:

- Treat `README.md`, CLI `--help`, `examples/`, and `tests/` as the behavior contract.
- Keep docs and skills aligned with shipped behavior, not aspirational design.
- Use example apps and example UI flows as canonical downstream fixtures.
