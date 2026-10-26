# Runtime Performance Architecture

This reference captures the architecture goal: deterministic transition core, Holzmann boundary rules, explicit verification requirements, and maximum-performance runtime. It is domain-general. Apply it to rule engines, orchestrators, parsers, compilers, protocol runtimes, policy engines, API gateways, event processors, and any system that compiles human-friendly input into fast executable state.

## Prime Directive

Verification is heavy before acceptance. Runtime is thin after acceptance.

Do not run Lean, Kani, Verus, fuzzing, mutation testing, model checking, schema validation, graph validation, or arbitrary spec reasoning in production hot paths. Use them before acceptance to prove and freeze the artifact. Runtime executes a prevalidated, precompiled, compact representation with minimal dynamic work.

Slogan:

```text
Prove slow. Execute fast.
```

## Core Shape

When accepted human/spec/config data is evaluated repeatedly at runtime, compile human complexity away:

```text
human config / API schema / DSL / policy / graph / spec
  -> parse and validate
  -> compile to dense IR
  -> verify, prove, test, fuzz, model-check
  -> freeze, version, hash
  -> runtime executes compact IR
```

Runtime hot path:

```text
input arrives
  -> decode into typed event/command/request
  -> lookup immutable compiled artifact by numeric ID or hash
  -> apply deterministic bounded transition
  -> emit bounded command/effect list
  -> persist according to durability mode
  -> return
```

The hot path must not repeatedly parse YAML, walk arbitrary JSON values, validate global graph/spec invariants, reason about plugin semantics, run proof tools, scan the whole graph/state space, or search by human names. For one-shot streaming, protocol-bound, or externally keyed workloads, introduce dense IR only when measurement proves it beats the simpler representation.

## Dense Runtime IR

Runtime should operate on numeric IDs, dense arrays, bitsets, bounded buffers, borrowed slices, and immutable compiled artifacts when the same accepted representation is used repeatedly. Dense IR is mandatory for repeated runtime evaluation of human/spec/config data; otherwise it is an optimization that needs benchmark evidence.

Prefer:

```text
u32/u64 newtype IDs
Box<[T]>
Vec<T> with fixed lifecycle
FixedBitSet / roaring bitmaps
ArrayVec / SmallVec
Bytes for shared payloads
Arc<CompiledIr>
precomputed offsets and masks
```

Avoid in hot paths:

```text
String keys
HashMap<String, T>
serde_json::Value
YAML/CUE/OpenAPI objects
dynamic graph structures
Box<dyn Trait> dispatch in inner loops
Arc<Mutex<HashMap<Key, Value>>>
heap allocation per transition
format! and large clone
```

Example ID shape:

```rust
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct OpId(pub u32);

#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct ArtifactId(pub u64);

#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct EventSeq(pub u64);
```

Example compiled representation:

```rust
pub struct RuntimeIr {
    pub ops: Box<[Op]>,
    pub edges: Box<[Edge]>,
    pub edge_offsets: Box<[u32]>,
    pub prerequisites: Box<[PrereqMask]>,
    pub terminal_ops: fixedbitset::FixedBitSet,
}
```

Use `IndexMap`, `HashMap`, `petgraph`, strings, and rich metadata during compilation if they help validation or deterministic mapping. Freeze into arrays before runtime.

## Compile-Time Work, Not Runtime Work

Move expensive semantic analysis to compilation/acceptance:

- Topological sort or ordering checks.
- Cycle detection and bounded-loop analysis.
- Reachability and dead-node detection.
- Terminal/absorbing-state validation.
- Dependency precomputation.
- Fan-in/fan-out bounds.
- Transition target validation.
- Payload/schema validation.
- Permission/capability validation.
- Static command/effect bounds.

Runtime should ask only the local transition question:

```text
Given prevalidated IR, current state, and typed input, what bounded state change and effects follow?
```

## Frontier-Based Updates

Avoid scan-based hot paths.

Bad shape:

```text
for every input:
  scan every node/rule/step
  recompute global readiness/validity
```

Better shape:

```text
for every input:
  update affected entity
  inspect only outgoing/dependent entries
  update precomputed masks/counters
  push newly-ready work into bounded queue
```

The target cost is proportional to changed edges, changed items, or newly-ready work, not total graph/spec size.

## Bounded Collections

Bounded collections support both verification and speed.

Use `ArrayVec<T, N>` when the bound is hard and overflow is an error.

Use `SmallVec<[T; N]>` when the common case is small and fallback allocation is acceptable and measured.

Use them for:

- Commands/effects emitted by one transition.
- Ready queues.
- Validation errors.
- Small dependency lists.
- Small retry histories.
- Plugin/activity call batches.
- Per-request scratch data.

Do not use them blindly. Large inline capacity can bloat parent structs and hurt cache locality.

## Zero-Allocation Hot Path

Performance-critical transition functions should aim for zero heap allocation:

```rust
pub fn apply_input<const MAX_OUT: usize>(
    ir: &RuntimeIr,
    state: &mut RuntimeState,
    input: &InputEvent,
    out: &mut arrayvec::ArrayVec<Effect, MAX_OUT>,
) -> Result<(), RuntimeError> {
    out.clear();
    transition(ir, state, input, out)
}
```

Prefer caller-owned buffers over returning `Vec` when the function is hot:

```text
fn encode_into(input, out: &mut Vec<u8>)
fn parse_into(input, scratch: &mut Scratch, out: &mut Output)
fn apply_input(ir, state, input, out: &mut ArrayVec<Effect, N>)
```

Reject hot-path APIs that force allocation, ownership, or formatting without measured need.

## Hashing And Lookup Policy

Hash maps are for compilation, metadata, admin paths, or cold lookups. They are not the default hot-path representation.

| Situation | Preferred Shape |
|---|---|
| Public/user-controlled keys | `std::collections::HashMap` or keyed hasher. |
| Internal numeric IDs | Dense arrays or `Vec`. |
| Small read-mostly maps | Sorted `Vec` plus binary search, benchmarked. |
| Internal non-adversarial hashing | `hashbrown`, `ahash`, or `rustc-hash`, benchmarked and threat-modeled. |
| Deterministic compile-time mapping | `IndexMap`. |
| Concurrent lookup/update | Sharded design first; `DashMap` only when measured. |

If keys are bounded integers, direct indexing usually beats hashing.

## Serialization And Payloads

Use different formats by layer:

| Layer | Shape |
|---|---|
| Human config/spec | YAML, CUE, OpenAPI, TOML, JSON Schema. Compile away before runtime. |
| External REST API | JSON via `serde_json` until profiling says otherwise. |
| Internal event log | Compact binary, versioned, checksummed. |
| IPC/local frames | Binary or `Bytes`-backed frames. |
| Proof/report artifacts | Canonical JSON/Markdown/tool-native artifacts. |

Rules:

- No YAML parsing in hot paths.
- No `serde_json::Value` in proof-critical runtime paths.
- Use `bytes::Bytes` for shared network/payload buffers when clone-by-reference is desired.
- Use `postcard` for compact Serde-friendly binary when appropriate.
- Use `rkyv` only when zero-copy deserialization is worth its complexity and verified.
- Use `simd-json` only when JSON parsing is measured as a bottleneck.

## Durable Event Log And Storage

Separate pure transition latency from durable append latency. A transition can be nanoseconds or microseconds. Fully durable sync-to-disk acknowledgement is dominated by storage behavior.

Expose explicit durability modes:

```toml
[durability]
mode = "strict"        # strict | batch | async
fsync = "every_event"  # every_event | every_n_events | every_ms | manual
batch_max_events = 64
batch_max_delay_us = 500
```

Recommended v0 event log shape:

```text
append-only segment files
monotonic EventSeq
record header
payload length
CRC/checksum
payload bytes
commit marker or length-prefix discipline
periodic index
bounded replay
background compaction when needed
```

Storage choices:

| Need | Candidate |
|---|---|
| Custom fastest event append | Segmented WAL. |
| Embedded ACID key-value | `redb`. |
| Write-heavy LSM-style key-value | `fjall`. |
| Mature native KV with operational cost | RocksDB. |
| Replay/index read path | `memmap2`, only when page-fault tail risk is acceptable. |
| Fast checksums | `crc32fast`. |

## Thin Async Shell

The async shell performs I/O, scheduling, timeout, durability, and adapter work. The semantic decision stays in the deterministic bounded transition core.

Rules:

- Do not block inside async tasks.
- Do not do CPU-heavy verification inside request handlers.
- Do not hold locks across `.await`.
- Do not parse or validate full specs on request paths.
- Do not spawn unbounded tasks.
- Do not use unbounded queues.
- Use `spawn_blocking` or dedicated bounded pools for blocking work.
- Use Rayon or dedicated CPU pools for CPU-bound parallel work.
- Keep ports typed, timeout-bound, idempotent, and payload-size-bound.

Preferred shell stack:

```text
tokio
axum
tower
hyper
bytes
tracing
metrics
```

## Ports And Effects

Generated/user-defined code must not freely touch I/O. Use typed ports.

Port policy:

- All I/O has timeout.
- All side effects have idempotency keys where retry is possible.
- All queues are bounded.
- All retries are bounded.
- All payloads have max sizes.
- All external work is behind typed adapters.
- The bounded transition core emits bounded effects; the shell executes them.

Effect enum shape:

```rust
pub enum Effect {
    CallActivity {
        op_id: OpId,
        idempotency_key: IdempotencyKey,
        timeout: TimeoutMs,
        payload: bytes::Bytes,
    },
    Persist,
    Complete,
    Fail,
}
```

## Build Profile And PGO

Use release before judging runtime speed:

```bash
cargo build --release
```

Use this as the normal optimized profile when the project has no stronger measured profile:

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = "symbols"

[profile.profiling]
inherits = "release"
debug = true
strip = "none"

[profile.bench]
inherits = "release"
debug = true
```

Do not default services to `panic = "abort"`; keep panic boundaries possible unless the binary is supervised batch work where termination is acceptable. Benchmark profile variants before declaring one fastest: default release, thin LTO, fat LTO, target CPU, PGO, binary size, compile time, and deployment constraints.

For local/native deployments only when portability risk is acceptable:

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

Do not ship `target-cpu=native` binaries to unknown CPUs.

Full nightly max-performance waiver mode may benchmark `lto = "fat"`, `panic = "abort"`, `overflow-checks = false`, `target-cpu=native`, PGO/BOLT, allocator API, specialization, or intrinsics. This is not default policy. It requires explicit user approval, target hardware, before/after benchmarks, correctness proof for removed checks, and a report naming which default rules were waived.

Use PGO only for stable representative workloads:

```text
build instrumented binary
run representative workload
rebuild using profile
benchmark and verify again
```

## Pinned Nightly And Hardening

Pinned nightly gives more checking and performance options; it does not permit unstable sprawl.

```toml
# rust-toolchain.toml
[toolchain]
channel = "nightly-YYYY-MM-DD"
profile = "minimal"
components = ["rustfmt", "clippy", "rust-src", "llvm-tools-preview"]
targets = ["x86_64-unknown-linux-gnu"]
```

Allowed source features by default: `portable_simd` and `try_blocks`. `RUSTC_BOOTSTRAP`, arbitrary feature gates, specialization, and first-party `std::arch` intrinsics fail policy.

Use sanitizers and dedicated UB tooling as extra verification, not as proof by themselves:

```bash
RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -Zbuild-std --target x86_64-unknown-linux-gnu
RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test -Zbuild-std --target x86_64-unknown-linux-gnu
```

## Allocator Policy

Do not start with allocator swaps. First reduce allocation rate and reuse buffers.

If allocation remains a measured bottleneck, benchmark default allocator against alternatives such as `mimalloc` or jemalloc.

Keep an alternative allocator only if it improves the target workload and tail-latency behavior.

Hot paths and untrusted-input paths that grow memory must declare maximum size, use checked arithmetic, call `try_reserve` when graceful allocation failure matters, and return typed allocation/resource errors.

## Observability Without Hot-Path Damage

Use `tracing` and `metrics`, but do not format/log in tight loops.

Prefer counters and histograms:

```text
runtime.inputs.applied
runtime.apply_input.us
runtime.effects.emitted
runtime.event_log.append.us
runtime.replay.events
```

Use detailed tracing on failure, sampling, or cold paths.

## Performance Verification Requirements

Verification requirements must include performance acceptance, not just correctness.

Required gate categories:

```text
correctness passed
performance budget passed
allocation budget passed
regression budget passed
```

Recommended benchmark dependencies when the project has no equivalent:

```toml
[dev-dependencies]
criterion = "0.8"
iai-callgrind = "0.16"
```

Example budgets are illustrative only. They are not universal defaults; replace them with project-configured tools, commands, and thresholds.

```toml
[performance.budgets]
compile_ir_p95_us = 1000
apply_input_p95_ns = 5000
replay_256_events_p95_us = 250
emit_effects_p95_ns = 2000
allocations_apply_input = 0
allocations_transition = 0
```

Suggested tools:

- Criterion for statistical local benchmarks.
- Iai-Callgrind or Cachegrind for deterministic-ish instruction/cache regression checks.
- `cargo flamegraph`, `perf`, or `samply` for hot-path discovery.
- `heaptrack`, DHAT, or allocator instrumentation for allocation budgets.
- `cargo llvm-lines` and `cargo bloat` for monomorphization/code-size tradeoffs.
- `tokio-console` / `console-subscriber` for async task/resource diagnostics in diagnostic builds.

Coverage is optional but useful when project policy requires it:

```bash
RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE="coverage-%p-%m.profraw" cargo +nightly test --workspace --all-features
```

## Zero-Slippage Nightly Gate

Every touched Rust workspace must pass this gate or report the exact missing tool/component as a blocker.

```bash
cargo +nightly fmt --all -- --check
cargo +nightly -Zallow-features=portable_simd,try_blocks check --workspace --all-targets --all-features
cargo +nightly -Zallow-features=portable_simd,try_blocks clippy --workspace --all-features -- -D warnings -D unsafe_code -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic -D clippy::panic_in_result_fn -D clippy::todo -D clippy::unimplemented -D clippy::dbg_macro -D clippy::indexing_slicing -D clippy::string_slice -D clippy::get_unwrap -D clippy::arithmetic_side_effects -D clippy::as_conversions -D clippy::let_underscore_must_use -D clippy::await_holding_lock
cargo +nightly nextest run --workspace --all-features
cargo +nightly doc --workspace --all-features --no-deps
if rg -n '(^|[^A-Za-z0-9_])(assert!|assert_eq!|assert_ne!|unreachable!)' --glob '*.rs' --glob '!**/tests/**' --glob '!**/benches/**' --glob '!**/examples/**' --glob '!build.rs'; then exit 1; else true; fi
cargo audit
cargo deny check
cargo vet
cargo geiger
cargo machete
cargo hack check --workspace --feature-powerset
cargo mutants
```

## Generated Rust Policy

Generated or framework-managed Rust should enforce these hot-path bans:

```text
No YAML parsing in hot path.
No serde_json::Value in hot path.
No HashMap<String, _> in hot path.
No unbounded Vec growth in proof-critical runtime.
No unbounded channels.
No unbounded retries.
No unbounded spawned tasks.
No blocking calls inside async handlers.
No locks held across await.
No clone of large payloads.
No format! in hot path.
No unwrap/expect/panic/unreachable/assert macros in production-reachable code.
No unsafe in generated code unless the user explicitly approved an unsafe waiver before implementation.
```

Positive rules:

```text
Use dense numeric IDs.
Use immutable compiled IR by hash/version.
Use Bytes for payloads.
Use ArrayVec/SmallVec for bounded output buffers.
Use FixedBitSet or Roaring for state sets.
Use precomputed adjacency/offset arrays.
Use explicit timeouts.
Use idempotency keys.
Use bounded channels.
Use benchmarked hot functions.
```

## Recommended Crate Stack

| Subsystem | Candidates |
|---|---|
| HTTP/API shell | `axum`, `tower`, `hyper`, `tokio`, `bytes`, `serde`, `serde_json`. |
| Internal data structures | `arrayvec`, `smallvec`, `fixedbitset`, `roaring`, `indexmap`, `hashbrown`, `ahash`, `rustc-hash`, `petgraph` for compile-time validation. |
| Parsing | `serde`, `nom`, `winnow`, `logos` depending on grammar shape. |
| Binary/event formats | `postcard`, `rkyv`, `bytes`, `crc32fast`, `memmap2`. |
| Storage | segmented WAL, `redb`, `fjall`, RocksDB only with native dependency acceptance. |
| Concurrency | `tokio` for I/O, `rayon` for CPU-bound data parallelism, `crossbeam-channel` for bounded sync channels, `loom`/`shuttle` for concurrency verification. |
| Benchmark/profiling | `criterion`, `iai-callgrind`, `cargo flamegraph`, `samply`, `perf`, `cargo llvm-cov`, `cargo-mutants`. |

## Final Test

Reject designs that are clever but force runtime to redo accepted work. Accept designs that are boring for the machine:

```text
compile human complexity away
use dense representations
avoid allocations
avoid dynamic dispatch
avoid repeated parsing
precompute graph/search facts
bound every queue/retry/loop
separate CPU from I/O
measure everything
```
