# NASA/JPL Power of Ten PLUS For Rust

This reference maps Gerard J. Holzmann's 2006 Power of Ten rules to Rust, then adds the performance gates this skill requires for high-performance systems work. The original rules were written for safety-critical C so code would be reviewable and statically analyzable. In Rust, the same goal becomes: make failure modes typed, resource use bounded, unsafe code absent by default, and performance claims measured for latency, throughput, allocation, storage placement, and cache behavior.

## Source Baseline

| Source | What This Skill Uses |
|---|---|
| Gerard J. Holzmann, "The Power of 10: Rules for Developing Safety-Critical Code", IEEE Computer, 2006 | Canonical ten safety-critical code rules. |
| NASA/JPL Laboratory for Reliable Software context | Reviewability, analyzability, bounded behavior, and zero-warning static checks. |
| NASA-STD-8739.8B, Software Assurance and Software Safety Standard | Assurance context for systematic software safety and IV&V expectations. |

## Canonical Rule Mapping

| # | Power of Ten Rule | Rust Enforcement |
|---|---|---|
| 1 | Restrict code to simple control flow. No `goto`, `setjmp`, `longjmp`, direct recursion, or indirect recursion. | No recursion in critical paths; no panic-driven control flow; prefer `match`, direct calls, and explicit state machines. Async/stream loops must stay inspectable. |
| 2 | Give all loops a fixed upper bound that static checking can prove. | Strict Power-of-Ten code needs a static upper bound or mathematical termination proof for each loop, retry, traversal, worker drain, and stream poll. Runtime caps, timeouts, and cancellation paths are operational containment, not Rule 2 satisfaction by themselves. |
| 3 | Do not use dynamic memory allocation after initialization. | Safety-critical and mission-critical paths allocate only during initialization. Performance-only hot paths may allocate only with explicit budget, benchmark/profiler evidence, and regression guard. |
| 4 | No function longer than one printed page, usually about 60 lines. | Hard ceiling is one-page reviewability; target <=25 logical lines for hot/safety-critical code unless splitting hides invariants. |
| 5 | Assertion density averages at least two assertions per function; assertions are side-effect free and failures trigger explicit recovery. | Encode production invariants in types first; otherwise use constructors, boundary validation, or side-effect-free checks returning typed errors. `debug_assert!` is supplemental documentation only because release builds can strip it. Production `assert!`, `assert_eq!`, `assert_ne!`, and `unreachable!` are panic paths except in tests, benches, build scripts, or process-start invariant failure with clear diagnostics. |
| 6 | Declare data objects at the smallest possible scope. | Declare locals near use; keep borrows, `mut`, lock guards, unsafe-derived references, and temporary allocations short-lived. |
| 7 | Callers check non-void returns; callees check parameter validity. | Never ignore `Result`, `Option`, join handles, sends, flushes, or cleanup failures. Validate external inputs at boundaries and use `#[must_use]` where missed values are dangerous. |
| 8 | Limit preprocessor use to includes and simple macros; no token pasting, varargs, recursion, or complex conditional compilation. | Rust macros must not hide allocation, panics, unsafe, loops, or target behavior. Procedural/generated code requires review of generated behavior. Keep `cfg` branching small. |
| 9 | Restrict pointer use; no more than one dereference level; no hidden derefs; no function pointers. | Raw-pointer dereference, unsafe traits/impls, and transmute-like code are forbidden by default. Function pointers, `dyn Trait`, vtables, and FFI handles require typed safe wrappers and measured need in hot paths. |
| 10 | Compile from day one with all warnings enabled; pass strong static analysis with zero warnings. | `cargo fmt --check`, clippy with warnings denied, no unwrap/expect/panic/todo/unimplemented, and repo static gates block completion. |

## Rust-Specific Strengthening

The Rust version is stricter than the original C rules where the language gives better tools:

- Zero forbidden constructs are mandatory in generated or modified Rust code: no `unsafe`, `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, `dbg!`, unchecked indexing, unchecked arithmetic, lossy `as` conversions, or ignored fallible results.
- Panic freedom is mandatory for production-reachable code: no `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, `unreachable!`, production `assert!` macros, unchecked indexing, or unchecked arithmetic without local proof.
- Invalid states should be unrepresentable with newtypes, enums, typestates, and validated constructors when that does not damage the hot path.
- `unsafe` is forbidden by default. If FFI or target intrinsics make it unavoidable, stop and request an explicit user waiver before writing it; the waiver must define owner approval, invariants, tests, and why safe Rust cannot meet the requirement.
- Async code must preserve boundedness: spawned tasks, queues, channels, retries, streams, and cancellation paths need explicit resource limits.
- Dependencies and feature flags are part of the code shape: avoid importing allocation, dynamic dispatch, proc-macro, or runtime costs without need.

## PLUS Performance Extensions

Safety-critical reviewability is necessary but not sufficient. Performance-sensitive Rust also needs these gates:

| Extension | Requirement |
|---|---|
| Workload definition | State target hardware, input distribution, hot path, and threshold before optimizing. |
| Latency budget | Track p50/p95/p99 or max latency when user-visible, real-time, networking, UI, scheduler, or tail-stability behavior matters. |
| Throughput budget | Track operations/sec, bytes/sec, requests/sec, items/sec, or frames/sec under realistic batching and concurrency. |
| Storage placement | Choose stack, heap, arena, pool, static, mmap, or caller-owned buffers by measured size, lifetime, locality, reuse, allocation cost, and tail-latency behavior. |
| Allocation budget | Mission/safety-critical paths have no post-init allocation. Performance-only hot paths count or bound allocations and bytes; preallocate or borrow by default. |
| Cache layout | Review `size_of`, field order, contiguity, false sharing, and AoS vs SoA. |
| Static dispatch | Prefer generics/enums/inlining in hot paths; justify `dyn Trait` and callbacks with measurement. |
| Branch behavior | Split hot/cold paths and reduce unpredictable branches in tight loops. |
| Numeric semantics | Use checked/saturating/wrapping arithmetic deliberately; document float edge cases. |
| SIMD discipline | Keep scalar oracle/fallback, target gate, alignment/remainder handling, and benchmark proof. |
| Concurrency budget | Cap queues, tasks, retries, locks, and contention; document cancellation and lock ordering. |
| Code size | Check monomorphization, inlining, dependency, and feature-flag bloat when changed. |
| Regression guard | Record baseline, optimized result, command, workload, and pass/fail threshold. |

## Mechanical Empathy Standard

Fast Rust should make the machine do less work:

- Fewer bytes moved.
- Fewer cache misses.
- Fewer heap allocations and allocator lock interactions.
- Fewer unpredictable branches.
- Fewer locks, atomics, syscalls, and scheduler handoffs in hot paths.
- Fewer virtual calls and less pointer chasing.
- Simpler loops that LLVM can vectorize or optimize.

Do not accept performance claims based on style. Accept measured bottleneck removal.

## Panic-Free Standard

Production reachable code fails review when it contains:

- `unsafe` blocks, unsafe functions, unsafe traits, unsafe impls, raw-pointer dereferences, `transmute`, or unchecked access without an explicit prior user waiver.
- `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, or `unreachable!`.
- Indexing like `items[i]` without a preceding bound proof or safe accessor.
- `parse().unwrap()`, `Mutex::lock().unwrap()`, or channel `send().unwrap()`.
- `assert!`, `assert_eq!`, or `assert_ne!` in production-reachable code, except process-start invariant failure with clear diagnostics.
- `dbg!`, ignored `Result`, ignored join handles, or `let _ =` on must-use values.

Allowed narrow panic exceptions require comments and tests:

- Build scripts or tests where panic is the intended failure signal.
- Process-start invariants that cannot recover and produce clear diagnostics.
- Microbenchmarks where setup panics do not ship.

There is no implicit unsafe exception. Unsafe requires explicit user approval before writing code.

## Full Nightly Max-Performance Waiver

Default policy remains Power-of-Ten plus unsafe-forbidden-by-default. If the user explicitly requests speed-first nightly work, the rule waiver must be named before code is written.

Waiver mode may evaluate `lto = "fat"`, `panic = "abort"`, `overflow-checks = false`, `target-cpu=native`, PGO/BOLT, `portable_simd`, runtime feature dispatch, `core_intrinsics`, `std::arch`, `unchecked_*`, `unreachable_unchecked`, `allocator_api`, and `min_specialization`. Each waived item needs benchmark evidence, target hardware, fallback or deployment constraint, and a written proof for removed checks.

Where waiver mode conflicts with a Power-of-Ten/default rule, the report must say which rule was broken and why the measured speed gain justified the exception.

## Zero-Slippage Nightly Gate

Every Rust workspace touched by this skill must use this gate before acceptance. Missing binaries, components, or repo config are blockers, not silent skips.

```bash
cargo +nightly fmt --all -- --check

cargo +nightly -Zallow-features=portable_simd,try_blocks check \
  --workspace \
  --all-targets \
  --all-features

cargo +nightly -Zallow-features=portable_simd,try_blocks clippy \
  --workspace \
  --lib \
  --bins \
  --examples \
  --all-features \
  -- \
  -D warnings \
  -D unsafe_code \
  -D clippy::unwrap_used \
  -D clippy::expect_used \
  -D clippy::panic \
  -D clippy::panic_in_result_fn \
  -D clippy::todo \
  -D clippy::unimplemented \
  -D clippy::dbg_macro \
  -D clippy::indexing_slicing \
  -D clippy::string_slice \
  -D clippy::get_unwrap \
  -D clippy::arithmetic_side_effects \
  -D clippy::as_conversions \
  -D clippy::let_underscore_must_use \
  -D clippy::await_holding_lock

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

`cargo check --workspace --all-targets --all-features` compiles tests/examples/benches and is allowed. Strict source lint never includes test targets as an implementation style gate. In bead workflows, classify failures against `delivery-scope.jsonl`, `baseline-report.md`, and `global-readiness-report.md`: local, regression, required-obligation, and global-readiness failures block until repaired.

## Bounded Resource Standard

Every hot or mission-critical path needs a resource story:

- Static maximum loop iterations or mathematical termination proof.
- Maximum retry count for Rule 2 compliance.
- Timeout and cancellation token for service containment only; never as the sole proof of a fixed bound.
- Heap allocation count or statement that allocation is outside the path.
- Maximum queue depth, buffer size, or backpressure behavior.
- Lock ordering and contention expectation for shared state.

## Arithmetic Standard

Integer operations must name overflow behavior:

- `checked_*` for invalid external input or invariants that can fail.
- `saturating_*` for counters, metrics, and bounded accumulators.
- `wrapping_*` for hashes, checksums, crypto-style arithmetic, and ring buffers.
- Plain `+`, `-`, `*` only when type/range proof is local and obvious.

## Unsafe Standard

Unsafe code must state:

- Preconditions callers must uphold.
- Postconditions the block guarantees.
- Aliasing and lifetime assumptions.
- Alignment and initialization assumptions.
- Panic behavior and drop safety.
- Test evidence: unit, property, fuzz, sanitizer, or dedicated UB tooling where applicable.

## Verification Commands

Use the repository's canonical gate first. If no gate exists, start with:

```bash
cargo fmt --check
cargo clippy --all-features -- -D warnings -D unsafe_code -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic -D clippy::panic_in_result_fn -D clippy::todo -D clippy::unimplemented -D clippy::dbg_macro -D clippy::indexing_slicing -D clippy::string_slice -D clippy::get_unwrap -D clippy::arithmetic_side_effects -D clippy::as_conversions -D clippy::let_underscore_must_use -D clippy::await_holding_lock
cargo test --all-features
if rg -n '(^|[^A-Za-z0-9_])(assert!|assert_eq!|assert_ne!|unreachable!)' --glob '*.rs' --glob '!**/tests/**' --glob '!**/benches/**' --glob '!**/examples/**' --glob '!build.rs'; then exit 1; else true; fi
cargo audit
cargo deny check
cargo vet
cargo geiger
cargo machete
cargo hack check --workspace --feature-powerset
cargo mutants
```

Fallback preserves the same lint/tool intent as the nightly gate. If a lint, tool, or component is unavailable, report the exact blocker or residual risk; do not silently drop it.

Add these when the touched code justifies them and tools are available. These are templates only; do not report them as run until concrete repo target names are substituted:

```bash
cargo fuzz run actual_fuzz_target
cargo bench
cargo bloat --release --crates
cargo llvm-lines --release
perf stat -- cargo bench --bench actual_bench_name
```
