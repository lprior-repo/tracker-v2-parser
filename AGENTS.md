# Working instructions

## Scope and skills

This is a single-crate Rust scraper and XLSX exporter using SQLx and PostgreSQL.
The CLI provides `scrape` and `export`. Configuration selects states, graduation
years, sources and bounded limits. The application loads `.env` when present.
The application schema has six tables; SQLx migration metadata is separate.
Browser transport uses Chromiumoxide; access challenges require human handling.

Read local skills matching the task before editing, plus their applicable linked
references:

- Rust: [.agents/skills/functional-rust/SKILL.md](.agents/skills/functional-rust/SKILL.md)
  and [.agents/skills/holzman-rust/SKILL.md](.agents/skills/holzman-rust/SKILL.md).
- Async Rust: [.agents/skills/async-rust-reviewer/SKILL.md](.agents/skills/async-rust-reviewer/SKILL.md).
- Moon/CI: [.agents/skills/moon-v2/SKILL.md](.agents/skills/moon-v2/SKILL.md).
- Behavior coverage: [.agents/skills/bdd-enforcer/SKILL.md](.agents/skills/bdd-enforcer/SKILL.md).

These are vendored user skills, with supporting resources and original content
preserved. This file and checked-in task configuration define this project's
scope and runnable commands. Generic skill examples do not require old repo
wrappers, remote caches, telemetry services, or a new architecture. Beads uses
its own Dolt database for issue tracking, separate from PostgreSQL application data.

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
env -u CI moon run pipeline:run -- scrape --config config.toml
env -u CI moon run pipeline:run -- export --config config.toml --output athletes.xlsx
moon ci --force
```

`moon ci --force` is the canonical CI gate; `pipeline:run` is a local CLI action,
not a CI lane. Diagnose the failing lane before rerunning the full gate. Do not
weaken checks, suppress source warnings, or change assertions to hide a bug. The
user explicitly approved disabling only `clippy::missing_errors_doc` to honor the
no-code-comments requirement.

Manual tasks use `env -u CI` because Moon skips tasks marked `runInCI: false`
when the calling environment already has `CI` set.

Require actual CLI smoke evidence, not just compilation: use an isolated
PostgreSQL schema to replay a source fixture twice, verify persistence without
duplicates, and export and reopen an XLSX workbook. Tests use real PostgreSQL and
deterministic, isolated scenarios. Record commands, observed output, exit status,
and any untested behavior honestly.
Keep README instructions aligned with the exercised CLI behavior.

## Implementation rules

- Do not add code comments, including Rust documentation comments.

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
Keep SQLx/PostgreSQL as the storage boundary unless the user changes the contract.
Future scraping must respect site policies, rate limits, bounded concurrency,
request timeouts, response-size limits, and finite retry budgets. Stop for human
handling of CAPTCHA or other access challenges; do not bypass them.
