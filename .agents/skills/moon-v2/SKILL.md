---
name: moon-v2
description: "Moon v2 build-system and CI triage expert for Rust. Use for `moon ci`, `.moon/`, failed CI reports, `fmt`, `check`, `test`, `lint-src`, source-length gates, task design, caching, and Holzman Rust zero-warning source gates."
---

```jsonl
{"kind":"meta","skill":"moon-v2","version":"1.2.0","format":"markdown-with-embedded-jsonl","scope":"moon-build-system","core_truths":["ci_gate_is_canonical","source_lint_zero_tolerance","tests_lenient"]}
{"kind":"claude_best_practice","id":"ci_gate_is_canonical","text":"CI GATE IS CANONICAL (MANDATORY): moon ci is the canonical gate for Moon v2. moon run :ci is only valid as a repo-specific alias when explicit root :ci task exists. Run tests in pipeline; don't enforce test implementation style through clippy."}
{"kind":"claude_best_practice","id":"source_lint_zero_tolerance","text":"SOURCE LINT ZERO TOLERANCE (MANDATORY): Source code must have 0 clippy issues (no errors, no warnings, no clippy lints). moon ci fails if ANY clippy issue detected in source. Tests must compile and prove behavior; implementation style is lenient."}
{"kind":"rule","id":"moon_ci_preferred","level":"warn","text":"Prefer moon ci over moon run :ci. moon ci is canonical in v2. Only use moon run :ci when a root :ci task is explicitly defined in repo.","bans":["moon run :ci as primary gate when root :ci task does not exist"],"preferred":["moon ci","moon run :<specific-task>"],"notes":["moon ci respects runInCI automatically. moon run :ci requires explicit :ci task definition to behave as expected."]}
{"kind":"rule","id":"ci_triage_loop","level":"error","text":"Do not rerun full moon ci in a loop. Run one baseline, inspect the CI report, run targeted failing lanes until fixed, then run one final moon ci confirmation.","bans":["repeated full moon ci before targeted lane triage","claiming moon ci status from stale report data"],"preferred":["moon ci baseline","inspect .moon/cache/ciReport.json when present","moon run :<failing-task>","final moon ci"]}
{"kind":"rule","id":"no_lazy_exceptions","level":"error","text":"Do not add source-line, clippy, source-length, or gate exceptions to make CI pass unless the user explicitly requests an exception after seeing the real fix option. Prefer fixing code, splitting files, removing dead code, or repairing tasks."}
{"kind":"rule","id":"lint_src_zero_tolerance","level":"error","text":"ZERO CLIPPY ISSUES IN SOURCE CODE (MANDATORY): moon ci gate must fail if clippy reports ANY issue in source (errors, warnings, or lints). Source lint uses -W clippy::all to catch everything. Tests must compile and prove behavior; implementation style is lenient.","bans":["allowing clippy warnings in source code","allowing clippy lints in source code","allowing clippy errors in source code","running moon ci with any clippy issues in source"],"preferred":["moon run :lint-src (catches all clippy issues)","cargo clippy --lib --bins --examples -- -D warnings -W clippy::all"],"notes":["Source code must be flawless (0 issues). Tests must compile and prove behavior with exact assertions. Fix ALL source clippy issues before CI."]}
{"kind":"rule","id":"lint_src_separate_from_tests","level":"warn","text":"Separate lint-src task from test tasks. Use dedicated lint-src task that runs clippy only on source targets (--lib --bins --examples) and excludes tests. Test implementation style is not gated by clippy.","bans":["adding test clippy style gates to build/test tasks"],"preferred":["moon run :lint-src","cargo clippy --lib --bins --examples -- -D warnings -W clippy::all"],"notes":["Tests are for behavior proof and exact assertions. Don't block test work due to test clippy/style warnings."]}
{"kind":"rule","id":"holzman_rust_posture","level":"warn","text":"Maintain Holzman Rust posture with zero tolerance: deny warnings (-D warnings) + deny all clippy lints (-W clippy::all). Source code must be flawless, measurable, and panic-free.","bans":["allowing unwrap/expect in production code","suppressing clippy warnings or lints","ignoring any clippy issue in source code"],"preferred":["-D warnings -W clippy::all"],"notes":["ALL clippy lints are caught and treated as failures in source code. Tests must compile and prove behavior, but test implementation style is not clippy-gated."]}
{"kind":"rule","id":"test_tasks_in_pipeline","level":"info","text":"RUN TEST TASKS IN PIPELINE: moon ci should include test tasks by default. Tests run with NO strict clippy enforcement. Tests are exempt from source lint zero-tolerance policy.","bans":["excluding tests from pipeline","requiring tests to pass strict lint rules"],"preferred":["moon ci includes :test by default","tests run with cargo test or nextest"],"notes":["Tests verify behavior with exact assertions. Don't block test work due to test lint/style issues."]}
{"kind":"pattern","id":"moon_ci_workflow","text":"MOON CI WORKFLOW (v2 canonical)","example":"moon ci\n# CI runs affected targets respecting runInCI\n# Check CI report action for PR summaries","notes":["moon ci detects affected files automatically. All tasks with runInCI=true run (or runInCI unset). v2: affected calculation excludes graph relations unless --include-relations flag. Run report action adds PR comments with timing and status."]}
{"kind":"pattern","id":"lint_src_workflow","text":"LINT-SRC TASK DESIGN","example":"moon run :lint-src\n# In .moon/tasks/all.yml:\n# lint-src:\n#   command: 'cargo clippy --lib --bins --examples -- -D warnings -W clippy::all'\n#   options:\n#     runInCI: true\n#   inputs:\n#       - '@globs(sources)'\n#   toolchains:\n#     - rust\n# Cargo clippy default source-target selection excludes tests","notes":["Use -W clippy::all to catch ALL clippy issues (zero tolerance). Source code must have 0 issues (errors, warnings, lints). Tests are style-lenient but must compile and prove behavior with exact assertions. Run moon run :lint-src for CI, or cargo clippy --lib --bins --examples -- -D warnings -W clippy::all locally."]}
{"kind":"pattern","id":"holzman_rust_clippy","text":"HOLZMAN RUST CLIPPY (ZERO TOLERANCE)","example":"cargo clippy --lib --bins --examples -- -D warnings -W clippy::all","notes":["Deny ALL clippy lints: -D warnings -W clippy::all (zero tolerance). Every clippy issue (error, warning, lint) causes CI failure. Source code must be flawless (0 issues). Tests must compile and prove behavior, but test implementation style is not clippy-gated."]}
{"kind":"pattern","id":"moon_v2_workspace_config","text":"MOON V2 WORKSPACE CONFIGURATION","example":"# .moon/workspace.yml\nvcs:\n  client: git\n  provider: github\n  defaultBranch: main\n  versionConstraint: '>=1.0.0'\nremote:\n  host: 'grpcs://127.0.0.1:9092'  # Local bazel-remote cache (use 127.0.0.1 not localhost)\n  cache:\n    compression: 'zstd'\n    instanceName: 'your-repo-name'\n    verifyIntegrity: false  # Skip for local trusted cache (faster)\n    localReadOnly: false  # Allow uploads from local dev\npipeline:\n  installDependencies: true\n  syncWorkspace: true\nhasher:\n  optimization: 'accuracy'  # accuracy for CI, performance for dev (optional)\n  walkStrategy: 'vcs'\ntelemetry: false","notes":["v2 uses 'remote' (not 'unstable_remote'). For local bazel-remote, use 127.0.0.1:9092 (gRPC) + HTTP on 9090 for health. 'remote.cache.verifyIntegrity: false' for trusted local cache. 'remote.cache.localReadOnly: false' allows uploads from local dev. sccache runs independently as Layer 1 cache."]}
{"kind":"pattern","id":"moon_v2_toolchains_config","text":"MOON V2 TOOLCHAINS CONFIGURATION","example":"# .moon/toolchains.yml\nrust:\n  version: '1.84.0'  # Pin Rust version\n  bins:\n    - 'cargo-nextest@latest'  # Pre-built via cargo-binstall\n    - 'cargo-audit@latest'\n  syncToolchainConfig: true  # Syncs to rust-toolchain.toml","notes":["v2 uses .moon/toolchains.yml (plural), not .moon/toolchain.yml. 'unstable_' prefix removed from toolchains. 'bins' installs binaries with cargo-binstall for speed. 'syncToolchainConfig' writes rust-toolchain.toml automatically."]}
{"kind":"pattern","id":"moon_v2_tasks_config","text":"MOON V2 TASKS CONFIGURATION (.moon/tasks/all.yml)","example":"# .moon/tasks/all.yml\n# Core lanes inherited by all projects\nfmt:\n  command: 'cargo fmt --all --check'\n  toolchains:\n    - rust\n  options:\n    runInCI: true\n    inputs:\n      - '@globs(sources)'\n\nlint-src:\n  command: 'cargo clippy --lib --bins --examples -- -D warnings'\n  toolchains:\n    - rust\n  options:\n    runInCI: true\n  inputs:\n      - '@globs(sources)'\n\ncheck:\n  command: 'cargo check --workspace'\n  toolchains:\n    - rust\n  options:\n    runInCI: true\n    inputs:\n      - '@globs(sources)'\n\ntest:\n  command: 'cargo nextest run --workspace'\n  toolchains:\n    - rust\n  options:\n    runInCI: true\n    inputs:\n      - '@globs(sources)'\n      - '@globs(tests)'\n\nbuild:\n  command: 'cargo build --workspace --release'\n  toolchains:\n    - rust\n  options:\n    runInCI: true\n    inputs:\n      - '@globs(sources)'\n      - '@globs(tests)'\n    outputs:\n      - 'target/release/<binary-name>'\n\naudit:\n  command: 'cargo audit'\n  toolchains:\n    - rust\n  options:\n    runInCI: true\n    inputs:\n      - 'Cargo.lock'\n      - 'Cargo.toml'\n\n# Composite lanes\nci:\n  command: 'moon ci'  # Canonical CI gate\n  options:\n    runInCI: true\n\nquick:\n  command: 'moon run :fmt :lint-src :check'  # Fast dev loop\n\nlint:\n  command: 'moon run :lint-src'  # Strict source linting\n","notes":["v2 uses .moon/tasks/all.yml for inherited tasks. 'toolchains' setting per-task (not 'platform'). Deep merge for inheritance. 'inputs'/'outputs' use @globs references defined in workspace fileGroups. Composite tasks like :ci and :quick don't define commands—just call other tasks."]}
{"kind":"pattern","id":"moon_v2_ci_workflow","text":"MOON V2 CI IN GITHUB ACTIONS","example":"# .github/workflows/ci.yml\nname: CI\non:\n  push:\n    branches: [main]\n  pull_request:\njobs:\n  ci:\n    name: CI\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n        with:\n          fetch-depth: 0\n      - uses: moonrepo/setup-toolchain@v0\n      - run: moon ci\n      - uses: moonrepo/run-report-action@v1\n        if: success() || failure()\n        with:\n          access-token: ${{ secrets.GITHUB_TOKEN }}","notes":["fetch-depth: 0 is CRITICAL for moon ci's affected file detection. moonrepo/setup-toolchain@v0 installs moon and toolchains. moonrepo/run-report-action@v1 posts PR comment with results. moon ci respects runInCI automatically."]}
{"kind":"pattern","id":"local_bazel_remote","text":"LOCAL BAZEL REMOTE CACHE (Layer 2 of 3-layer caching)","example":"# Start bazel-remote (Layer 2: Moon task outputs AC+CAS cache)\nbazel-remote --dir ~/.cache/bazel-remote --max-size 500 --storage_mode zstd --grpc_address 127.0.0.1:9092 --http_address 127.0.0.1:9090 &\n\n# sccache (Layer 1: compiler artifacts) runs independently via ~/.cargo/config.toml wrapper\n# Moon remote config points to Layer 2: grpcs://127.0.0.1:9092\n# 3-Layer cache: sccache -> bazel-remote -> Cargo incremental target/\n# Layer 1: sccache (in-memory, ~9GB, 12 jobs, fastest rustc artifact hits)\n# Layer 2: bazel-remote (disk-based, AC+CAS, ~500MB, Moon task outputs)\n# Layer 3: Cargo (per-repo, incremental compilation in target/)","notes":["Start bazel-remote with 127.0.0.1 (not localhost). HTTP on 9090 for health, gRPC on 9092 for AC+CAS. sccache configured in ~/.cargo/config.toml rustc-wrapper. Moon caches to bazel-remote (Layer 2), sccache caches rustc artifacts (Layer 1). Use remote.cache.verifyIntegrity: false for trusted local cache (faster). remote.cache.localReadOnly: false allows uploads from local dev."]}
{"kind":"gate","id":"moon_ci_gate","commands":["moon ci"],"notes":"Canonical Moon v2 CI gate. Runs affected targets respecting runInCI. All tasks with runInCI=true run automatically. Use moonrepo/run-report-action@v1 for PR summaries. Pipeline green only when no clippy errors."]}
```

# Moon v2 Build System Expert

You are an expert in **Moon v2** for Rust repositories. Moon is a build system that orchestrates tasks, manages dependencies, and provides CI/CD primitives — not a replacement for Cargo. Your role is to help users configure Moon workspaces, design tasks, set up CI pipelines, and integrate Moon with Rust workflows using Holzman Rust patterns.

## Core Mental Model

- **Moon orchestrates Cargo** — Moon runs `cargo` commands but does not replace Cargo workflows
- **Rust workspace = single Moon project** — For Cargo workspaces, configure a single Moon project covering all crates (avoids duplicate `cargo --workspace` runs)
- **Canonical CI gate** — `moon ci` is the recommended primary CI gate in Moon v2
- **Affected-based execution** — `moon ci` only runs tasks affected by touched files (plus graph relations with `--include-relations`)
- **Toolchains** — v2 uses `.moon/toolchains.yml` (plural), not `.moon/toolchain.yml`
- **Remote cache** — Optional AC+CAS via Bazel Remote API (supports `grpc://` and `http://`)
- **Deep merge inheritance** — v2 merges global task configs sequentially (not shallow), respecting merge options

## CI Triage Loop

Use this loop for every `moon ci` repair, regardless of model or host:

1. Run one baseline `moon ci` unless a fresh same-session CI report already exists.
2. Inspect `.moon/cache/ciReport.json` when present and list failing task labels, statuses, and errors.
3. For each failing lane, run the smallest targeted command, for example `moon run :fmt`, `moon run :check`, `moon run :test`, `moon run :verify-loom`, or the repo-specific script named by the task.
4. Fix the root cause. Do not add allowlist, source-length, lint, or ignored-result exceptions unless the user explicitly approves that policy tradeoff after seeing the proper fix.
5. Rerun the targeted lane that failed. Repeat only for that lane until it passes or a real blocker is proven.
6. After all targeted lanes pass, run one final `moon ci` and report exact evidence.

If a command times out, produces truncated output, or the report is missing, inspect the task-specific log or rerun the focused lane. Do not keep launching full CI as a diagnostic substitute.

## Tool Discipline

- Prefer structured reads and searches over shell truncation. Use `Read`/`Grep` where available; use shell `head`, `tail`, `cat`, or `ls` only when the host lacks a better tool or the command is itself the subject of the task.
- When an edit or patch fails because context changed, read the current file region and recompute the edit. Do not retry stale line-numbered snippets.

## Workspace Configuration (.moon/workspace.yml)

Moon v2 workspace configuration in Rust repositories:

```yaml
# .moon/workspace.yml
vcs:
  client: git
  provider: github
  defaultBranch: main

versionConstraint: '>=1.0.0'  # Enforce Moon binary version

remote:  # Optional: bazel-remote AC+CAS cache
  host: 'grpcs://localhost:9092'  # or 'grpcs://team-cache:9092'
  cache:
    compression: 'zstd'  # or 'none' for I/O-bound setups
    instanceName: 'your-repo-name'  # Distinguishes repos
    verifyIntegrity: true  # Validate digests on download

pipeline:
  installDependencies: true
  syncWorkspace: true

hasher:
  optimization: 'accuracy'  # Use 'performance' for dev speed (tradeoff: hash precision)

telemetry: false
```

### Key Settings Explained

| Setting | Purpose | Rust Context |
|----------|---------|---------------|
| `vcs.defaultBranch` | Branch for `moon ci` base comparison | Must match your repo's default |
| `versionConstraint` | Lock Moon binary version across machines | Prevents drift in CI |
| `remote.cache.compression` | Compress cached artifacts | `zstd` reduces bandwidth, `none` faster for local NVMe SSD |
| `remote.cache.verifyIntegrity` | Validate blob digests on download | Prevents corruption, slight perf cost |
| `hasher.optimization` | Hash strategy tradeoff | `accuracy` = use lockfile versions (slower), `performance` = use manifest (faster) |
| `remote.cache.localReadOnly` | Dev mode optimization | Local dev only downloads cache, never uploads | CI uploads as usual |
| `pipeline.installDependencies` | Auto-run `InstallWorkspaceDeps`/`InstallProjectDeps` | Useful for Cargo, but optional |
| `pipeline.syncWorkspace` | Auto-run `SyncWorkspace` action | Syncs hooks, codeowners, VCS state |

## Toolchains Configuration (.moon/toolchains.yml)

Moon v2 uses `.moon/toolchains.yml` (plural) instead of v1's `.moon/toolchain.yml`:

```yaml
# .moon/toolchains.yml
rust:
  version: '1.84.0'  # Pin Rust version (matches rust-toolchain.toml)
  bins:                     # Binaries installed via cargo-binstall (faster than cargo install)
    - 'cargo-nextest@latest'
    - 'cargo-audit@latest'
  syncToolchainConfig: true  # Sync version to rust-toolchain.toml automatically
```

### Toolchain Settings Explained

| Setting | Purpose | Notes |
|----------|---------|-------|
| `rust.version` | Pin Rust across all machines | Must align with `rust-toolchain.toml` |
| `rust.bins` | Pre-install global binaries | Uses `cargo-binstall` for speed (pre-built binaries) |
| `rust.syncToolchainConfig` | Auto-sync Rust version | Writes `rust-toolchain.toml` automatically |
| `unstable_` prefix | Removed in v2 | Toolchains are stable; no `unstable_rust` needed |

**Important:**
- Moon v2 toolchains use simple keys (e.g., `rust`, `javascript`), not nested under `node`/`unstable_`
- All toolchains are stable now except Python (still uses `unstable_python`)

## Task Configuration (.moon/tasks/all.yml)

Moon v2 uses `.moon/tasks/all.yml` for tasks inherited by all projects. Deep merge inheritance ensures all configs merge in sequence:

```yaml
# .moon/tasks/all.yml

# Core lanes (inherited by all projects)
fmt:
  command: 'cargo fmt --all --check'
  toolchains:
    - rust
  options:
    runInCI: true
  inputs:
    - '@globs(sources)'

lint-src:
  command: 'cargo clippy --lib --bins --examples -- -D warnings -W clippy::all'
  toolchains:
    - rust
  options:
    runInCI: true
  inputs:
    - '@globs(sources)'

check:
  command: 'cargo check --workspace'
  toolchains:
    - rust
  options:
    runInCI: true
  inputs:
    - '@globs(sources)'

test:
  command: 'cargo nextest run --workspace'
  toolchains:
    - rust
  options:
    runInCI: true
  inputs:
    - '@globs(sources)'
    - '@globs(tests)'

build:
  command: 'cargo build --workspace --release'
  toolchains:
    - rust
  options:
    runInCI: true
  inputs:
    - '@globs(sources)'
    - '@globs(tests)'
  outputs:
    - 'target/release/<binary-name>'

audit:
  command: 'cargo audit'
  toolchains:
    - rust
  options:
    runInCI: true
  inputs:
    - 'Cargo.lock'
    - 'Cargo.toml'

# Composite lanes (call other tasks)
ci:
  command: 'moon ci'  # Canonical CI gate
  options:
    runInCI: true

quick:
  command: 'moon run :fmt :lint-src :check'  # Fast dev loop

lint:
  command: 'moon run :lint-src'  # Strict source linting
```

### v2 Changes from v1

| Area | v1 | v2 | Impact |
|------|-----|-----|--------|
| Config file | `.moon/toolchain.yml` | `.moon/toolchains.yml` | File renamed (plural) |
| Remote setting | `unstable_remote` | `remote` | Simplified top-level key |
| Workspace tasks | `.moon/tasks.yml` | `.moon/tasks/all.yml` | File renamed for inherited tasks |
| Inheritance | Shallow merge | Deep merge | Better composition, predictable merges |
| Task config | `options.platform` | `options.toolchains` | Renamed for clarity |
| Affected detection | Always includes relations | Changed files only (unless `--include-relations`) | More predictable CI |
| `runInCI` | Only respected in `moon ci` | Respected across `moon ci`, `moon run`, `moon check` | More consistent behavior |
| `local` task setting | `local: true` | Use `preset: 'server'` | Clarifies persistent dev servers |
| Command syntax | Complex shell features in `command` | Use `script` for pipes/redirects | Simpler commands only |

### Task Design Principles

1. **Use `toolchains` per task** — Specifies which toolchain(s) are required (e.g., `toolchains: [rust]`)
2. **Use `runInCI` correctly** — Set to `true` for CI tasks, `false` for long-running dev servers (`preset: 'server'`)
3. **Define `inputs` using `@globs()`** — References file groups defined in `.moon/workspace.yml`
4. **Use `outputs` sparingly** — Cache specific artifacts (like release binaries), not entire `target/` directory
5. **Composite tasks don't define commands** — Tasks like `:ci` and `:quick` should only list commands or call other tasks

### Source vs Test Linting

The `lint-src` task is designed for **source code quality** and excludes tests:

```bash
# lint-src: ZERO TOLERANCE for ALL clippy issues (source only)
cargo clippy --lib --bins --examples -- -D warnings -W clippy::all

# Test code: lenient, compile-only (no strict clippy enforcement)
cargo nextest run --workspace  # No clippy flags, just runs tests
```

This aligns with the Holzman Rust posture:
- **Source linting**: Strict (deny warnings, deny critical categories)
- **Test code**: No strict quality gates (tests verify behavior)

## Holzman Rust Patterns

### Clippy Configuration for Zero-Tolerance Source Linting

Source code must have **0 clippy issues** (errors, warnings, lints — ALL treated as failures):

```bash
# Zero-tolerance clippy flags for source code
cargo clippy --lib --bins --examples \
  -D warnings \                    # Deny all warnings as errors
  -W clippy::all \                 # Deny ALL clippy lints (zero tolerance)
```

**Explanation of flags:**

| Flag | Purpose | Effect |
|------|---------|---------|
| `-D warnings` | Deny all compiler warnings | Treat warnings as errors (zero tolerance) |
| `-W clippy::all` | Deny ALL clippy lints | Every clippy lint is treated as failure in source |

**Zero-Tolerance Policy:**

- **Source code:** 0 issues (no errors, no warnings, no clippy lints)
  - Every clippy lint, warning, or error causes CI failure
  - Fix ALL issues before proceeding (no suppression, no workarounds)
- **Test code:** Compile-only requirement (no strict clippy enforcement)
  - Tests must compile and prove behavior with exact assertions
  - No clippy flags applied
  - Test implementation style is not gated; test design and exact assertions remain required

### Fast Development Loops

For rapid iteration during development (local checks before committing):

```bash
# Ultra-fast: skip clippy, skip build
moon run :fmt :check

# Fast: format + source lint (zero-tolerance), skip build
moon run :fmt :lint-src

# Normal: full validation
moon run :fmt :lint-src :check
```

## Moon CI/CD Workflow

### Primary CI Command: `moon ci`

**Why `moon ci` is canonical in v2:**

- **Affected-based execution** — Only runs tasks affected by touched files (plus relations with `--include-relations`)
- **Respects `runInCI`** — Automatically honors task-level CI configuration
- **Graph relations excluded by default** — More predictable CI (changed files only)
- **Consistent behavior** — Same semantic across `moon ci`, `moon run`, `moon check`

### When to Use `moon run :ci`

The `moon run :ci` form should **only** be used when:

1. A root-level `:ci` task exists in `.moon/tasks/all.yml`
2. You need explicit control over which tasks run (e.g., to run a subset in CI)
3. The task name conflicts with another tool's primary gate command

**Never use `moon run :ci` as the primary gate** when no explicit `:ci` task exists. It adds unnecessary complexity.

### GitHub Actions Integration

Standard Moon v2 CI workflow for Rust repositories:

```yaml
# .github/workflows/ci.yml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  ci:
    name: CI
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0  # CRITICAL for affected file detection

      - uses: moonrepo/setup-toolchain@v0

      - run: moon ci

      - uses: moonrepo/run-report-action@v1
        if: success() || failure()
        with:
          access-token: ${{ secrets.GITHUB_TOKEN }}
```

**Key configuration points:**

| Setting | Why Required | Moon v2 Behavior |
|----------|----------------|-------------------|
| `fetch-depth: 0` | `moon ci` needs full Git history | Detects changed files correctly |
| `setup-toolchain@v0` | Installs moon and toolchains | Uses `.moon/toolchains.yml` config |
| `moon ci` | Runs affected targets | Respects `runInCI` automatically |
| `run-report-action@v1` | Posts PR comments | Adds workflow summary, timing data |

### Affected File Detection

`moon ci` determines affected files by comparing HEAD against base branch (defaulting to `vcs.defaultBranch`).

**v2 change:** Graph relations (dependencies and dependents) are **not included** in affected calculation by default. To include them, use `--include-relations` flag.

```bash
# Default: affected by changed files only
moon ci

# Include graph relations
moon ci --include-relations
```

### 3-Layer Caching Strategy (sccache + bazel-remote + Cargo)

For maximum build speed on this machine:

```
Layer 1: sccache (in-memory compiler artifacts)
    ↓
Layer 2: bazel-remote (disk-based AC+CAS cache for Moon)
    ↓
Layer 3: Cargo incremental (per-repo target/ compilation)
```

**Layer 1: sccache (Fastest)**
- Wraps rustc via `~/.cargo/config.toml`
- In-memory cache: ~9GB, 12 parallel jobs
- Caches rustc artifacts across all Rust projects globally
- First cache hit: microsecond response time

**Layer 2: bazel-remote (Moon task outputs)**
- AC+CAS protocol for Moon tasks
- Disk-based: `~/.cache/bazel-remote`, ~500MB
- Shared across all Moon-enabled repos
- HTTP health endpoint: `127.0.0.1:9090`
- gRPC AC+CAS endpoint: `127.0.0.1:9092`
- Compressed with zstd

**Layer 3: Cargo incremental (Per-repo)**
- `target/` directory in each repository
- Cargo's built-in incremental compilation
- Local per-repo compilation state

**Cache Flow:**
```
rustc -> sccache (check)
         ↓ miss
       Cargo -> build -> target/
         ↓
      Moon -> bazel-remote (check/upload task outputs)
         ↓ miss
       Build task -> upload to bazel-remote
```

**Configuration:**

```toml
# ~/.cargo/config.toml (Layer 1)
[build]
rustc-wrapper = "/home/lewis/.cargo/bin/sccache"
jobs = 12
```

```bash
# Start bazel-remote (Layer 2)
bazel-remote --dir ~/.cache/bazel-remote --max-size 500 \
  --storage_mode zstd \
  --grpc_address 127.0.0.1:9092 \
  --http_address 127.0.0.1:9090 &
```

```yaml
# .moon/workspace.yml (Moon -> Layer 2)
remote:
  host: 'grpcs://127.0.0.1:9092'
  cache:
    compression: 'zstd'
    verifyIntegrity: false  # Trusted local cache
    localReadOnly: false  # Allow uploads from local dev
```

**Optimization Notes:**
- sccache runs globally (shared across ALL Rust projects)
- bazel-remote runs per-machine (shared across Moon projects)
- Each layer serves different purposes:
  - sccache: compiler artifacts (fastest)
  - bazel-remote: Moon task outputs (portable AC+CAS)
  - Cargo: per-repo incremental (local state)

### Remote Caching with Bazel Remote

**Local Development Cache (Recommended First):**

Start local bazel-remote for development speed:

```bash
# Start bazel-remote in background (persists to ~/.moon/cache/bazel)
bazel-remote --dir ~/.moon/cache/bazel --max-size 50g --host 0.0.0.0 --port 9092 &
BAZEL_PID=$!

# Moon workspace.yml points to same cache
# remote:
#   host: 'grpcs://localhost:9092'
```

**Benefits:**
- Ultra-fast local cache hits (shared across terminal sessions)
- Same AC+CAS protocol as team cache (scales easily)
- No network dependency during development

**Team CI Cache (Optional):**

Update `.moon/workspace.yml` to point to shared cache:

```yaml
remote:
  host: 'grpcs://team-cache.company.com:9092'
  auth:
    token: 'MOON_REMOTE_TOKEN'  # Environment variable with auth token
```

**Compression tradeoff:**

| Setting | Use Case | Performance Impact |
|----------|-----------|---------------------|
| `compression: 'none'` | Local NVMe SSD development | Fastest I/O, no CPU overhead |
| `compression: 'zstd'` | Network-bound CI, shared cache | ~2-3x smaller blobs, slower CPU |
| `verifyIntegrity: true` | Security-critical repos | Validates digests, slight perf cost | Recommended |

### Cache Output Strategy

**DO NOT cache entire `target/` directory:**

Moon uses tarball-based caching. Caching `target/` is not viable:

- 50GB+ cache artifacts (very slow uploads/downloads)
- Tarball overhead for massive directories
- Cargo incremental compilation already provides `target/` caching

**What to cache instead:**

```yaml
# Cache release binaries only
build:
  outputs:
    - 'target/release/my-binary'

# Cache test reports or specific artifacts
test:
  outputs:
    - 'target/nextest/reports/**'
```

Let Cargo handle `target/` with its own incremental compilation. Moon caches the final deliverables.

## Common Workflows

### Initial Setup

```bash
# 1. Install moon (via proto or cargo)
curl -fsSL https://moonrepo.dev/install.sh | bash

# 2. Initialize moon workspace in Rust repo
cd /path/to/rust-repo
moon init

# 3. Configure workspace (edit generated .moon/workspace.yml)
# - Set vcs.provider
# - Add remote cache (optional)
# - Set versionConstraint

# 4. Create .moon/tasks/all.yml with Rust task definitions
# See "Task Configuration (.moon/tasks/all.yml)" section above

# 5. Start local bazel-remote (optional but recommended)
bazel-remote --dir ~/.moon/cache/bazel --max-size 50g --host 0.0.0.0 --port 9092 &
```

### Local Development Workflow

```bash
# 1. Check and format code
moon run :fmt

# 2. Run source lint (strict, no tests)
moon run :lint-src

# 3. Type check
moon run :check

# 4. Run tests (normal clippy)
moon run :test

# 5. If all pass, build release
moon run :build
```

### CI Workflow (Manual Push)

```bash
# 1. Ensure repo is up-to-date
git pull origin main

# 2. Run Moon CI gate (canonical)
moon ci

# 3. Check exit code (moon ci fails on clippy errors)
if [ $? -eq 0 ]; then
  echo "CI passed. Ready to push."
else
  echo "CI failed. Fix issues before push."
fi
```

### CI Workflow with `moon run :ci` (Custom Targets)

Only use when you need to run specific tasks in CI (e.g., skip build):

```bash
# Run only fmt and lint in CI (no tests, no build)
moon run :ci :fmt :lint-src
```

## Migration from Moon v1 (Specific to Your Setup)

Your current setup uses Moon v1 with `unstable_remote`. To migrate to v2:

| v1 Setting | v2 Setting | Migration Action |
|-------------|-----------|----------------|
| `.moon/toolchain.yml` | `.moon/toolchains.yml` | Rename file |
| `unstable_remote.host: 'grpc://localhost:9092'` | `remote.host: 'grpcs://127.0.0.1:9092'` | Update host (127.0.0.1 vs localhost, add protocol prefix) |
| `.moon/tasks.yml` | `.moon/tasks/all.yml` | Rename file |
| No mutex config | `options: { mutex: 'rust_heavy' }` | Add mutex to heavy tasks (prevents concurrent sccache + bazel-remote overload) |

**Your specific migration steps:**

```bash
# 1. Rename toolchain config
mv /home/lewis/src/<project>/.moon/toolchain.yml \
   /home/lewis/src/<project>/.moon/toolchains.yml

# 2. Update workspace.yml remote setting
# Change from:
#   unstable_remote:
#     host: 'grpc://localhost:9092'
# To:
#   remote:
#     host: 'grpcs://127.0.0.1:9092'
#     cache:
#       compression: 'zstd'
#       verifyIntegrity: false  # Trusted local cache
#       localReadOnly: false  # Allow uploads

# 3. Ensure bazel-remote is running
bazel-remote --dir ~/.cache/bazel-remote --max-size 500 \
  --storage_mode zstd \
  --grpc_address 127.0.0.1:9092 \
  --http_address 127.0.0.1:9090 &

# 4. Verify sccache is configured
# Should see: rustc-wrapper = "/home/lewis/.cargo/bin/sccache"
cat ~/.cargo/config.toml | grep rustc-wrapper
```

## Migration from Moon v1 (General)

If migrating from v1 to v2:

| v1 Setting | v2 Setting | Migration Action |
|-------------|-----------|----------------|
| `.moon/toolchain.yml` | `.moon/toolchains.yml` | Rename file |
| `unstable_remote` | `remote` | Update top-level key |
| `.moon/tasks.yml` | `.moon/tasks/all.yml` | Rename file |
| `runner.autoCleanCache` | `pipeline.cacheLifetime` | Simplified config |
| `runner.*` | `pipeline.*` | Renamed `runner` settings to `pipeline` |
| `constraints.enforceProjectTypeRelationships` | `constraints.enforceLayerRelationships` | Renamed |
| `vcs.manager` | `vcs.client` | Renamed |
| `vcs.syncHooks` | `vcs.sync` | Renamed |
| `rust.syncToolchainConfig` | `rust.syncToolchainConfig` (same) | No change |
| `toolchain.*.disabled` | `toolchain: null/false` | Use null/false instead |
| `project.type` | `project.layer` | Renamed setting |
| `task.options.local` | `task.options.preset: 'server'` | Renamed for clarity |
| `runInCI` only in `moon ci` | `runInCI` in all commands | Better consistency |

**Breaking CLI changes:**

| v1 Command | v2 Equivalent | Notes |
|-------------|----------------|-------|
| `moon query hash` | `moon hash` | Command renamed |
| `moon query touched-files` | `moon query changed-files` | Terminology updated |
| `--update-cache, -u` | `--force, -f` | Renamed flag |
| `--profile` | (removed) | Use composite tasks instead |
| `--no-bail` | (removed) | Use `moon exec` directly |
| `--remote` | `--affected-remote` | Renamed flag |
| `moon docker prune --install-toolchain-deps` | `moon docker prune --installToolchainDependencies` | Snake case |
| `moon migrate from-package-json` | (use extension instead) | Removed |

## Anti-Patterns to Avoid

| Anti-Pattern | Problem | Moon v2 Solution |
|--------------|---------|---------------------|
| **Allowing clippy warnings in source** | Not zero-tolerance | Use `-W clippy::all` in `lint-src` task (catches ALL clippy issues) |
| **Allowing clippy lints in source** | Source code not flawless | Fix ALL clippy issues before CI (no suppression, no workarounds) |
| **Using `moon run :ci` as primary gate** | No explicit `:ci` task | Define `:ci` task in `.moon/tasks/all.yml` OR use `moon ci` directly |
| **Caching entire `target/`** | Slow uploads, tarball overhead | Cache specific outputs (release binaries, reports) |
| **Ignoring `runInCI` in local dev** | Inconsistent behavior | `runInCI` respected across all commands in v2 |
| **Hardcoding paths in scripts** | Breaks portability | Use workspace-relative paths, `$MOON_WORKSPACE_ROOT`, or `@globs()` references |
| **Running `cargo --workspace` in multiple projects** | Duplicate work | Use single Moon project for Cargo workspace |
| **Not using affected detection** | Running all tasks unnecessarily | Use `moon ci` for CI, `moon run <specific-task>` for local |
| **Setting `local: true` for dev servers** | Confusing name | Use `preset: 'server'` or explicit `runInCI: false` |
| **Complex shell commands in `task.command`** | Hard to maintain | Use `script` for pipes/redirects, or keep commands simple |
| **Ignoring v2 behavior differences** | Affected calculation, inheritance changes | Read migration guide, adapt workflows |

## Best Practices

### Task Design

- **Single Rust project per Cargo workspace** — Avoids duplicate `cargo --workspace` invocations
- **Separate `lint-src` from test tasks** — Maintain zero-tolerance for source, lenient for tests
- **Use `-W clippy::all` in `lint-src`** — Catches ALL clippy issues in source (zero tolerance)
- **Use `inputs` and `outputs` correctly** — Leverage Moon's hashing for efficient caching
- **Set `runInCI: true` for all CI tasks** — Ensures tasks run in `moon ci` without extra configuration

### CI Configuration

- **Always use `fetch-depth: 0`** — Critical for `moon ci` affected file detection
- **Use `moonrepo/setup-toolchain@v0`** — Installs Moon and configured toolchains automatically
- **Use `moonrepo/run-report-action@v1`** — Adds PR comments with timing and status
- **Prefer `moon ci` over `moon run :ci`** — Canonical gate respects `runInCI` automatically
- **Pin Moon version** — Use `versionConstraint` to prevent drift across machines

### Development Workflow

- **Run `moon run :lint-src` before committing** — Zero-tolerance check for source code (fixes ALL clippy issues)
- **Run `moon run :fmt :lint-src :check` locally** — Fast dev loop before committing
- **Run `moon ci` before pushing** — Validates entire pipeline with affected detection
- **Use local bazel-remote** — Dramatically speeds up iteration with shared cache

### Performance

- **Set `hasher.optimization: 'performance'` for local dev** — Faster hashing (trades some accuracy)
- **Set `remote.cache.compression: 'none'` for local NVMe SSD** — Faster I/O
- **Set `remote.cache.verifyIntegrity: false` for trusted networks** — Skip digest validation for speed

## Troubleshooting

### "moon ci: no affected targets"

**Cause:** No files changed compared to base branch.

**Solutions:**
1. Verify you're on the correct branch (`git branch`)
2. Check `vcs.defaultBranch` in `.moon/workspace.yml` matches your repo
3. Use `--head`/`--base` flags or `MOON_HEAD`/`MOON_BASE` environment variables

### "moon ci: target already running"

**Cause:** Target process already running (from previous incomplete run).

**Solutions:**
1. Check for background Moon processes: `ps aux | grep moon`
2. Kill stale processes: `pkill -9 moon`
3. Retry `moon ci`

### Clippy Issues in CI (Zero Tolerance)

**Cause:** clippy reported ANY issue (error, warning, or lint) in source code.

**Impact:** `moon ci` fails with exit code 1 (zero-tolerance policy).

**Solutions:**
1. Fix ALL clippy issues locally: `moon run :lint-src` (uses `-W clippy::all`)
2. Use `cargo clippy --lib --bins --examples --message-format=short` for focused error messages
3. Check `.clippy.toml` for custom configurations that may conflict with zero-tolerance
4. Verify `rust.syncToolchainConfig: true` is set (affects `rust-toolchain.toml`)
5. Remember: 0 issues is the goal — fix everything before CI

### "task: command not found"

**Cause:** Binary specified in `command` doesn't exist in PATH.

**Solutions:**
1. Verify binary is installed: `cargo install <binary>`
2. Use full path to binary: `command: '/absolute/path/to/binary'`
3. Check `rust.bins` in `.moon/toolchains.yml` for expected global binaries

### Remote Cache Not Working

**Cause:** `moon ci` not caching to remote service.

**Symptoms:** All tasks show "no cache" in run report.

**Solutions:**
1. Verify `.moon/workspace.yml` `remote.host` is set
2. Check network connectivity to remote host
3. Verify `MOON_REMOTE_TOKEN` environment variable is set (if auth required)
4. Check bazel-remote is running if using local cache
5. Check Moon logs with `--log-level debug`

### Task Not Running in CI

**Cause:** Task skipped despite being affected.

**Solutions:**
1. Check `runInCI: false` is set on task
2. Check task is excluded via `--affected-only` flag in `moon ci`
3. Verify `task.options.envFile` or environment variables are set correctly
4. Check task dependencies are failing (blocks execution)

## Command Reference

### Core Commands

| Command | Purpose | Example |
|---------|---------|---------|
| `moon init` | Initialize workspace | `moon init` |
| `moon check` | Run lint/typecheck tasks | `moon check` |
| `moon ci` | Run CI gate (affected) | `moon ci` |
| `moon run <target>` | Run specific task | `moon run :build` |
| `moon hash` | Compute task hash | `moon hash :build` |
| `moon query changed-files` | List changed files | `moon query changed-files` |
| `moon query targets` | Show task targets | `moon query targets` |

### v2-Specific Flags

| Flag | Purpose | Example |
|------|---------|---------|
| `--include-relations, -g` | Include graph relations in affected | `moon ci --include-relations` |
| `--force, -f` | Force cache refresh | `moon ci --force` |
| `--ci` | Force CI mode (sets `CI=true`) | `moon run :test --ci` |
| `--affected-remote <url>` | Use remote cache for affected queries | `moon ci --affected-remote grpcs://cache:9092` |
| `--toolchain <name>` | Use specific toolchain for command | `moon run :test --toolchain rust` |

### Environment Variables

| Variable | Purpose | When Set |
|-----------|---------|-----------|
| `CI` | Force CI mode detection | CI systems set this; can set manually for testing |
| `MOON_WORKSPACE_ROOT` | Workspace root directory | Read-only; use for paths in scripts |
| `MOON_REMOTE_TOKEN` | Remote cache authentication | Set when `remote.auth.token` is configured |
| `MOON_BASE` | Base branch for comparison | Override `vcs.defaultBranch` |
| `MOON_HEAD` | Head revision for comparison | Override auto-detection |

## Quality Gates Checklist

Before running `moon ci` or pushing to remote, verify:

- [ ] `.moon/workspace.yml` configured with correct `vcs.defaultBranch`
- [ ] `.moon/toolchains.yml` exists with Rust version pinned
- [ ] `.moon/tasks/all.yml` exists with task definitions
- [ ] `lint-src` task uses `-W clippy::all` (zero tolerance)
- [ ] `moon run :lint-src` returns 0 clippy issues (source code flawless)
- [ ] Tests compile successfully with `moon run :test` (no strict clippy required)
- [ ] `moon ci` runs locally without failures (affected targets only)
- [ ] `moonrepo/setup-toolchain@v0` action in CI workflow
- [ ] `moonrepo/run-report-action@v1` in CI workflow
- [ ] `fetch-depth: 0` in checkout action (critical for affected detection)
- [ ] Build completes with `moon run :build`
- [ ] PR run report shows all green (zero clippy issues in source)

---

**Skill Version**: 1.2.0  
**Last Updated**: February 2026  
**Status**: Production-Ready  
**Moon Version**: v2.0.0+  
**Rust Version**: nightly-x86_64-unknown-linux-gnu (via rustup)  
**Core Policy**: Source code = 0 clippy issues (zero tolerance). Tests = compile-only (lenient).  
**Your Setup**: 3-layer cache (sccache + bazel-remote + Cargo). Rust nightly. bazel-remote on 127.0.0.1:9092.
