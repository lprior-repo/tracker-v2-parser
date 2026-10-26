# Working instructions

## Scope and skills

Part 1 is a minimal Rust CLI with SQLx + SQLite, not a scraper port. It opens or
creates `DATABASE_URL` (default `sqlite://track-scrapper.sqlite3`), checks database
readiness, and exits. The parent directory must exist. `.env.example` documents
shell exports; the app does not automatically load `.env`. No domain tables or
migrations yet; introduce the first migration with the first real model.

Read local skills matching the task before editing, plus their applicable linked
references:

- Rust: [.agents/skills/functional-rust/SKILL.md](.agents/skills/functional-rust/SKILL.md)
  and [.agents/skills/holzman-rust/SKILL.md](.agents/skills/holzman-rust/SKILL.md).
- Async Rust: [.agents/skills/async-rust-reviewer/SKILL.md](.agents/skills/async-rust-reviewer/SKILL.md).
- Moon/CI: [.agents/skills/moon-v2/SKILL.md](.agents/skills/moon-v2/SKILL.md).
- Behavior coverage: [.agents/skills/bdd-enforcer/SKILL.md](.agents/skills/bdd-enforcer/SKILL.md).

These are vendored user skills, with supporting resources and original content
preserved. This file and checked-in task configuration define this project's
scope and runnable commands. Generic skill examples do not require beads, old
repo wrappers, extra tool stacks, remote caches, telemetry services, or a new
architecture. Do not set up machinery merely because an example mentions it.

## Commands and evidence

Use Moon from the repository root with the checked-in dated Rust toolchain:

```sh
moon run pipeline:fmt
moon run pipeline:check
moon run pipeline:lint-src
moon run pipeline:test
moon run pipeline:build
moon run pipeline:doc
env -u CI moon run pipeline:fmt-write
env -u CI moon run pipeline:run
moon ci --force
```

`moon ci --force` is the canonical CI gate; `pipeline:run` is a local CLI action,
not a CI lane. Diagnose the failing lane before rerunning the full gate. Do not
weaken checks, suppress source warnings, or change assertions to hide a bug.

Manual tasks use `env -u CI` because Moon skips tasks marked `runInCI: false`
when the calling environment already has `CI` set.

Require actual CLI smoke evidence, not just compilation: run with an isolated
SQLite file and verify creation, preservation on a second run, and nonzero
failure with useful diagnostics for a corrupt file and missing parent directory.
Tests use real SQLite and deterministic, isolated Given/When/Then scenarios.
Record commands, observed output, exit status, and any untested behavior honestly.
Keep README instructions aligned with the exercised CLI behavior.

## Implementation rules

- Separate **Data** (inert types), **Calculations** (pure transformations), and
  **Actions** (configuration, filesystem, database, and other I/O). Keep this
  separation simple; it does not require multiple crates or speculative traits.
- No first-party `unsafe`; no production `unwrap`/`expect`/`panic` paths or ignored
  errors. Propagate or explicitly handle failures with useful context. Prefer
  typed invariants and bounded operations over unchecked indexing/arithmetic.
- Strict linting applies to production source targets, not test implementation
  style. Tests must compile, run, and assert exact observable behavior; assertions
  and ordinary test setup are not production panic violations.
- Keep async work in Actions, bound resources and concurrency, own any spawned
  tasks, and preserve cancellation/cleanup correctness. Do not add background
  work or performance abstractions without a real need.
- Preserve user edits and data. Do not wholesale import the reference checkout's
  Restate/Fjall/runtime wrappers, architecture, or tracking machinery.

## Later scraper migration

Migrate one observable behavior at a time using real, representative fixtures and
parity assertions. Retain fixture/data provenance (source URL, capture time, and
relevant attribution/permission), without committing secrets or sensitive data.
Keep SQLx/SQLite as the storage boundary unless the user changes the contract.
Future scraping must respect site policies, rate limits, bounded concurrency,
request timeouts, response-size limits, and finite retry budgets. Stop for human
handling of CAPTCHA or other access challenges; do not bypass them.
