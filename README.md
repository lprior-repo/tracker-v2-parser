# Track scrapper v2

Part 1 of an incremental Rust athletics pipeline rebuild. SQLite through SQLx is
the storage choice for this repository. The executable currently opens or creates
a database, runs SQLite's integrity check, logs readiness and the SQLite version,
and exits. It does not scrape, import athletes, or start a service.

## Prerequisites

- Git, Rustup, and a C compiler/linker (SQLite is bundled; no database server or
  system SQLite installation is required).
- Moon **2.2.4**. With Node.js 22+ and npm: `npm install --global @moonrepo/cli@2.2.4`.
- Rust **nightly-2026-04-27**, pinned in `rust-toolchain.toml` with Clippy and
  rustfmt. Rustup reads that file when Cargo runs. The initial build needs network
  access to download the toolchain and locked dependencies.

## Run

From the repository root:

```sh
env -u CI moon run pipeline:run
```

The default file is `track-scrapper.sqlite3` in the current working directory.
For another location:

```sh
export DATABASE_URL='sqlite://./results.sqlite3'
env -u CI moon run pipeline:run
```

The parent directory must already exist. `.env.example` is a shell configuration
example; `.env` files are **not** loaded automatically. Database files and `.env`
are ignored by Git. Do not point smoke checks at production or historical data.

A successful run logs `SQLite database ready` and exits with status 0. Invalid
configuration, an inaccessible path, or a corrupt database produces an error and
nonzero status; startup does not fall back to a different database. Existing
records are retained on subsequent runs. File-backed databases use WAL, foreign
keys, and a five-second busy timeout with one connection, explicitly closed at
shutdown. There are no domain tables or migrations yet: add the first SQLx
migration with the first real persisted model.

## Development checks

```sh
moon ci --force
```

This runs every CI lane, bypassing affected-file filtering and task caching. A
single Moon project orchestrates Cargo; there is no custom build wrapper or local
cache service requirement.

| Task | Command | Purpose |
|---|---|---|
| Formatting | `moon run pipeline:fmt` | Check Rust formatting |
| Compile | `moon run pipeline:check` | Check all targets and features with the lockfile |
| Source lint | `moon run pipeline:lint-src` | Clippy with warnings denied; excludes test-style linting |
| Behavior tests | `moon run pipeline:test` | Real executable and isolated SQLite files |
| Release build | `moon run pipeline:build` | Build `target/release/track-scrapper-v2` |
| Documentation | `moon run pipeline:doc` | Build Rust docs with warnings denied |

To format files, run `env -u CI moon run pipeline:fmt-write`. Manual tasks unset
`CI` because Moon otherwise skips tasks configured with `runInCI: false`.

The four integration scenarios cover creation of a usable database, persistence
across restarts at a configured path containing spaces, a nonexistent parent
without fallback, and a corrupt file without destructive replacement.

## CI and delivery

[GitHub Actions](.github/workflows/ci.yml) runs the same full Moon gate for pull
requests, pushes to `main`, manual dispatches, and `v*` tags. Successful runs upload
a Linux x86-64 release archive and `SHA256SUMS` as workflow artifacts. A `v*` tag
also creates a GitHub release containing those gated files after checksum
verification. No crates.io publishing or service deployment is configured.

The workflow uses Ubuntu 24.04, pinned action revisions, the checked-in Rust
nightly, and the exact Moon version. Publishing uses the built-in `GITHUB_TOKEN`
with write access only in the tag-release job. A GitHub remote and a push are
required to exercise the hosted workflow; local checks do not publish anything.

## Agent workflow and later ports

Read [AGENTS.md](AGENTS.md). Portable copies of the user's Rust, async Rust, Moon,
and BDD skills live in `.agents/skills/`, including their supporting references.
No home-directory symlinks or external skill installation is needed to read them.

Setup conventions were selected from the `athletic-rust-pipeline-integrate`
checkout: the Moon pin, dated Rust toolchain, strict source linting, and separate
behavior tests. No scraper, Fjall storage, Restate orchestration, old gate
wrappers, or historical data was copied. Keep pure data/calculations separate
from I/O without inventing a multi-crate architecture before it is needed.

Next: choose one scraper behavior from the old pipeline, capture a representative
fixture with provenance, get it working here end-to-end, then introduce only the
SQLite schema required for that behavior.
