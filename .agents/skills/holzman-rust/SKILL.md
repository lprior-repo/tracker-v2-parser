---
name: holzman-rust
description: "Power of Ten PLUS maximum-performance Rust skill. Use for Rust implementation, repair, CI failure triage, source lint, clippy, fmt, check/test failures, safety-critical systems, low-level code, async hot paths, optimization, or performance review. Applies Gerard J. Holzmann's NASA/JPL Power of 10, then optimizes for latency, throughput, bounded resources, dense runtime IR, measured storage placement, cache-aware layout, static dispatch, safe SIMD, assembly/IR evidence, release provenance, unsafe-forbidden-by-default policy, zero unwrap/expect/panic/todo/unimplemented, and benchmark/profiler proof."
allowed-tools:
  - Read
  - Glob
  - Grep
  - Bash
---

# Holzmann Rust

NASA/JPL Power of Ten PLUS Rust for mission-critical, lowest-latency, highest-throughput systems.

The skill name remains `holzman-rust` for compatibility with existing skill references. The doctrine is Gerard J. Holzmann's Power of Ten, adapted for Rust and extended with explicit performance gates.

## Contract

Deliver the fastest implementation among the evaluated candidates under stated workload, hardware, constraints, benchmark thresholds, and residual-risk disclosure. Safety comes from bounded, typed failures. Speed comes from measured latency, measured throughput, data layout, storage placement, control flow, ownership, and hardware evidence. Verification is heavy before acceptance; runtime is thin after acceptance. Human-friendly specs, validation, proofs, fuzzing, and model checks happen before the hot path. Runtime executes prevalidated, precompiled representations with minimal dynamic work when repeated evaluation justifies that representation. Stack is not automatically faster; heap is not automatically slower. Choose stack, heap, arena, pool, static storage, or caller-owned buffers by measured access pattern, size, lifetime, cache behavior, and tail-latency impact. Every performance claim requires measured evidence. Every safety claim requires compiler, static-analysis, test, or adversarial evidence. Never invent command output, benchmark numbers, profiler evidence, or file paths.

## Source Baseline

- Gerard J. Holzmann, "The Power of 10: Rules for Developing Safety-Critical Code", IEEE Computer, 2006.
- NASA/JPL Laboratory for Reliable Software context: rules were designed to make C code easier to review and statically analyze.
- NASA-STD-8739.8B is the active NASA Software Assurance and Software Safety Standard; use it as assurance context, not as a replacement for the Power of Ten rules.

## Reference Use Requirement

Before giving Rust implementation, review, or performance advice, read the applicable reference files with the Read tool and list the exact filenames used. This is fail-closed: if the relevant reference files cannot be read, stop and report the missing file as a blocker instead of proceeding from memory.

- Always use `references/latency-throughput-playbook.md` for latency, throughput, storage placement, allocator, benchmark, or profiling decisions.
- Use `references/runtime-performance-architecture.md` for dense IR, prevalidated runtime artifacts, numeric IDs, bounded collections, event logs, async shell, and performance verification rules.
- Use `references/zero-cost-abstractions.md` for allocation, dispatch, iterator, layout, hashing, and abstraction cost decisions.
- Use `references/simd-patterns.md` for safe vectorization, target-feature gates, scalar fallback, and explicit unsafe-waiver rejection rules.
- Use `references/nasa-jpl-standards.md` for Power of Ten, zero unsafe, panic freedom, bounded resource, arithmetic, and assurance rules.
- Use `references/mechanical-empathy-toolchain.md` for second-ring evidence tools: assembly/IR inspection, API compatibility, auditable builds, SBOM, Crux, SAW, and Hax.

## Non-Negotiable Doctrine

1. **Zero forbidden constructs by default** - no `unsafe`, `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, `unreachable!`, production `assert!` macros, indexing without proof, unchecked arithmetic, or ignored fallible results in generated or modified Rust code.
2. **Bounded control flow** - Power-of-Ten strict mode requires a static bound or mathematical termination proof. Timeouts, cancellation, and runtime caps are service containment only; they do not satisfy the fixed-bound requirement.
3. **No allocation surprises** - safety-critical and mission-critical paths do not allocate after initialization; performance-only hot paths must declare an allocation budget and keep allocations only with measurement plus a regression guard.
4. **Data layout and storage placement first** - prefer the representation that wins for the workload: stack for small fixed short-lived data, heap/arena/pool for large, variable, shared, or lifetime-stable data, and contiguous layouts when scans dominate.
5. **Static dispatch first** - generics, enums, and monomorphization before `dyn Trait`; dynamic dispatch must be justified by runtime polymorphism needs.
6. **Unsafe is forbidden by default** - do not write unsafe blocks, unsafe functions, unsafe traits, unsafe impls, raw-pointer dereferences, or unchecked access. If FFI/SIMD/max-performance work makes unsafe unavoidable, stop and request an explicit user waiver before writing it.
7. **Measure before optimizing** - no performance rewrite without baseline, benchmark, latency/throughput target, and regression threshold.
8. **Types carry invariants** - invalid states should be unrepresentable when the type cost does not harm the hot path.
9. **Concurrency is explicit** - no accidental `Arc<Mutex<T>>`; prefer ownership transfer, message passing, atomics, sharding, or bounded lock-free structures.
10. **Prove slow, execute fast** - compile human complexity into dense runtime artifacts; never run proof, fuzz, spec validation, or graph reasoning in production hot paths.
11. **Pinned nightly, not floating nightly** - use checked-in `rust-toolchain.toml`, allow only `portable_simd` and `try_blocks` by default, and treat `RUSTC_BOOTSTRAP` as a policy violation.
12. **CI gate is real** - do not report completion without actual command evidence or a clear blocker.

## Harness Recovery And Waiver Discipline

- After a compile, lint, fmt, test, proof, or CI failure, isolate the smallest failing lane, repair the root cause, rerun that lane, and only then rerun the canonical global gate.
- Do not add allowlist/source-line/source-length/clippy exceptions, disable checks, silence warnings, or weaken scripts to make a gate pass unless the user explicitly approves that waiver after seeing the proper fix path.
- If an edit fails because context changed, read the live file region and recompute the patch. Do not retry stale `oldString`, line-numbered snippets, or identical no-op edits.
- Prefer host file/search tools over shell truncation for inspection. Use shell `head`, `tail`, `cat`, or `ls` only when they are the behavior under test or no better host tool exists.
- Before finalizing after a long session, compaction, or many repair loops, re-check live git status, changed files, and the final command evidence instead of trusting memory.

## Verification Scope Boundary

Holzman rules are brutal on modified production Rust, required proof obligations, and global readiness. Old repo-wide failures are not ignored or deferred; go-skill treats them as prerequisite `BLOCK_GLOBAL` repair.

- `BLOCK_LOCAL`: any forbidden construct, source clippy warning, panic surface, dependency/unsafe policy issue, failing test, or verifier failure in touched crates/files/APIs/contracts/dependencies blocks.
- `BLOCK_REGRESSION`: any new global failure introduced by the bead blocks, even if outside the immediate file list.
- `BLOCK_GLOBAL`: repo-wide, workspace-wide, toolchain, dependency, policy, release-gate, warning, coverage, mutation, audit, or flaky-test failures block until repaired, even when they existed before the bead.

Strict linting is source-target linting. Use `cargo check --workspace --all-targets --all-features` to compile tests/examples/benches. Strict source lint never includes test targets as an implementation style gate. Tests must compile and run, assert exact behavior, remain deterministic, and satisfy mutation/coverage obligations when scoped or required.

Tool selection is layered for token efficiency: Lean/Aeneas/Hax only for tiny pure critical kernels; Verus/Creusot/Flux/Prusti for core logic contracts; Kani/Crux for bounded invariants and counterexamples; Loom/Shuttle/Stateright/Lockbud for concurrency/protocols; sanitizers/cargo-careful or dedicated UB tooling for unsafe, FFI, layout, and UB-sensitive paths; proptest/cargo-fuzz/Bolero for hostile input; audit/deny/geiger/vet/machete for dependency and unsafe policy.

## NASA/JPL Power of Ten For Rust

1. **Simple control flow** - no `goto` equivalent, recursion, panic-driven control flow, macro-hidden branches, or clever state hidden in closures; use explicit `match` or named state machines.
2. **Fixed loop bounds** - strict Power-of-Ten code gives every loop, stream poll, retry, traversal, and worker drain a static upper bound or mathematical termination proof. Runtime caps, timeouts, and cancellation paths are useful service containment, but they do not satisfy Rule 2 by themselves.
3. **No post-init dynamic allocation in critical paths** - safety-critical and mission-critical paths allocate during initialization only. Performance-only hot paths may allocate only with an explicit allocation budget, benchmark/profiler evidence, and a regression guard.
4. **Functions fit on one page** - canonical limit is roughly 60 lines; this skill targets <=25 logical lines for hot/safety-critical functions unless splitting would obscure invariants.
5. **Assertion and invariant density** - non-trivial functions expose invariants through types, constructors, boundary checks, or side-effect-free checks that return typed errors. `debug_assert!` is supplemental documentation only. Production `assert!`, `assert_eq!`, `assert_ne!`, and `unreachable!` are panic paths and are forbidden except in tests, benches, build scripts, or process-start invariant failure with clear diagnostics.
6. **Smallest scope** - declare values near first use; keep borrows narrow; minimize lifetimes of `mut`, locks, guards, and unsafe-derived references.
7. **Checked returns and parameters** - never ignore `Result`, `Option`, join handles, channel sends, flushes, or fallible cleanup; validate external inputs at boundaries.
8. **Limited macro/preprocessor power** - macros must not hide allocation, panics, unsafe, loops, dispatch, or target-specific behavior; conditional compilation stays small and auditable.
9. **Restricted pointer and indirect call use** - raw pointers, function pointers, trait objects, FFI handles, and transmute-like operations stay behind typed safe APIs; raw-pointer dereference or transmute-like unsafe requires an explicit waiver.
10. **Warnings and analysis are mandatory** - rustc warnings, clippy warnings, formatting drift, and static-analysis findings block completion.

## PLUS Maximum Performance Extensions

1. **Workload first** - name input distribution, hot path, target hardware, latency target, throughput target, and acceptance threshold before optimizing.
2. **Latency budget** - track p50/p95/p99 or max latency when user-visible, real-time, trading, networking, UI, or scheduler behavior matters.
3. **Throughput budget** - track operations/sec, bytes/sec, requests/sec, items/sec, or frames/sec when batch, stream, or server capacity matters.
4. **Storage placement is measured** - choose stack, heap, arena, pool, static, mmap, or caller-owned buffers by size, lifetime, cache locality, reuse, allocation cost, and tail-latency impact.
5. **Allocation budget** - state max allocations and bytes for hot paths; prove with benchmark/profiler evidence when allocation behavior changes.
6. **Cache-first layout** - inspect hot structs for size, padding, locality, false sharing, prefetch shape, and array-of-structs vs struct-of-arrays tradeoffs.
7. **Static dispatch hot path** - default to generics/enums/inlining; justify `dyn Trait`, callbacks, and function pointers with measured need.
8. **Branch predictability** - split hot/cold paths, minimize unpredictable branches in tight loops, and avoid error handling that pollutes the common path.
9. **SIMD and parallelism are gated** - scalar oracle, scalar fallback, target-feature gate, alignment/remainder handling, benchmark proof, and thread-count scaling evidence are required.
10. **Dense runtime artifacts when justified** - dense IR is mandatory when accepted human/spec/config data is evaluated repeatedly at runtime; otherwise introduce numeric IDs, arrays, bitsets, immutable precompiled IR, or precomputed lookup tables only when measurement proves they beat the simpler representation.
11. **Regression guard** - performance work needs reproducible commands, baseline numbers, new numbers, variance notes, and thresholds for future failure.

## Full Nightly Max-Performance Waiver Mode

Default mode is unsafe-forbidden-by-default. If the user explicitly requests a speed-first nightly implementation that may violate default Holzmann/zero-slippage constraints, treat it as a waiver mode, not as normal policy.

Waiver mode requires all of this before code is written:

- Exact user approval naming the rule being waived: unsafe code, extra nightly features, `target-cpu=native`, `panic = "abort"`, unchecked arithmetic/access, `std::arch`, `min_specialization`, allocator API, or branch/assume intrinsics.
- Benchmark baseline, target workload, target hardware, and acceptance threshold.
- Correctness proof for every removed check, including overflow, bounds, aliasing, initialization, layout, and panic/unwind assumptions.
- Scalar/safe fallback where hardware features are not guaranteed.
- Final report listing which Power-of-Ten/default rules were broken, why speed won, and how tests/benchmarks/profilers constrained the blast radius.

Use the waiver mode to evaluate speed techniques from largest expected gain downward: release profile variants, target CPU flags, PGO/BOLT, safe portable SIMD, runtime feature dispatch, cache layout/SoA, allocator/arena changes, then only with explicit waiver `std::arch`, `core_intrinsics`, unchecked operations, `unreachable_unchecked`, `allocator_api`, `min_specialization`, or nightly `test` harnesses.

## Pinned Nightly Policy

Nightly is allowed only as a discipline multiplier: stricter checking, safe portable SIMD, sanitizers, coverage, and feature allowlisting. It is not permission to add unstable complexity.

Required repo policy for nightly Rust work:

```toml
# rust-toolchain.toml
[toolchain]
channel = "nightly-YYYY-MM-DD"
profile = "minimal"
components = ["rustfmt", "clippy", "rust-src", "llvm-tools-preview"]
targets = ["x86_64-unknown-linux-gnu"]
```

Allowed source features by default:

```rust
#![feature(portable_simd)]
#![feature(try_blocks)]
#![forbid(unsafe_code)]
#![deny(unused_must_use)]
```

Forbidden without explicit user approval: arbitrary source feature gates, specialization, first-party `std::arch` intrinsics, unchecked SIMD loads/stores/gather/scatter, panic-capable SIMD APIs without immediate length proof, `RUSTC_BOOTSTRAP`, and `target-cpu=native` in default repo config.

## Build Profile Policy

Use this as the normal optimized profile when the project has no stronger measured profile:

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = "symbols"
```

Do not default services to `panic = "abort"`; keep panic boundaries possible unless the binary is supervised batch work where termination is acceptable. Test variants instead of assuming: default release, thin LTO, fat LTO, target-specific CPU flags, PGO, binary size, compile time, and deployment CPU compatibility.

## Performance Library Policy

Prefer audited crates over first-party cleverness, but every hot-path dependency needs benchmark and threat-model proof:

- Async I/O: `tokio`; no CPU-heavy loops on async workers.
- HTTP: `axum`, `tower`, `tower-http`; enforce timeouts, limits, tracing, load shedding, request IDs.
- CPU parallelism: `rayon`; only for large independent work with scaling evidence.
- Concurrency: `crossbeam`, `parking_lot`, `flume`; bounded queues/channels unless explicitly justified.
- Buffers: `bytes`, `arrayvec`, `smallvec`, `heapless`; choose by measured size/lifetime/cache behavior.
- Arenas: `bumpalo`; only when many objects share a lifetime and mass-freeing is correct.
- Maps: `hashbrown`, `ahash`, `rustc-hash`; fast hashers only for internal/non-adversarial keys.
- Formats: `postcard` by default for compact Serde-compatible binary; `rkyv` only for audited zero-copy cases; avoid new `bincode` usage unless project policy accepts its maintenance risk.
- JSON: `serde_json` by default; `sonic-rs` or `simd-json` only when JSON parsing is a proven bottleneck and the crate passes audit.
- Parsing: `winnow`, `nom`, `lexical-core` when they reduce handwritten parser risk and benchmark well.
- Allocators: `mimalloc` or `tikv-jemallocator` only after heap profiling proves allocator pressure remains after allocation reduction.

## Performance Review Checklist

### Hot Path Shape

- [ ] Baseline workload and acceptance threshold are stated.
- [ ] Input sizes, distribution, and worst case are explicit.
- [ ] Big-O behavior and constant factors are both considered.
- [ ] Branches in tight loops are minimized or made predictable.
- [ ] Error paths do not pollute the common path without reason.
- [ ] Human-friendly config/spec/proof work is outside the runtime hot path.
- [ ] Hot runtime artifacts use dense, prevalidated, numeric-ID-based, and precomputed forms only when repeated runtime evaluation or benchmark evidence justifies that representation.

### Latency And Throughput

- [ ] The optimization target is explicit: latency, throughput, memory footprint, CPU efficiency, or tail stability.
- [ ] p50/p95/p99 or max latency is measured when latency matters.
- [ ] Throughput is measured under realistic batch size, concurrency, and input distribution when throughput matters.
- [ ] Benchmark variance, warmup, CPU governor/noise, and target hardware are documented.
- [ ] Optimizations improve the bottleneck shown by profiling, not a guessed bottleneck.

### Allocation Discipline

- [ ] Hot path allocation count is known or measured.
- [ ] `String`, `Vec`, `HashMap`, `Box`, `Arc`, and `format!` in hot paths are justified.
- [ ] `with_capacity`, arenas, stack arrays, `SmallVec`, or `ArrayVec` are considered where appropriate.
- [ ] APIs accept slices and borrowed strings unless ownership is required.
- [ ] Clones are intentional, named, and cheaper than borrowing complexity.
- [ ] Stack vs heap choice is justified by size, lifetime, reuse, cache behavior, and measured performance.
- [ ] Large stack values, recursive stack growth, and per-request heap churn are rejected unless measured safe and faster.
- [ ] Hot or untrusted-input growth declares max size, uses checked arithmetic, calls `try_reserve` when allocation failure must be graceful, and returns typed resource errors.

### Data Layout

- [ ] Struct field order and size are reviewed for hot data.
- [ ] Array-of-structs vs struct-of-arrays is considered for vectorized scans.
- [ ] False sharing is considered for concurrent counters or queues.
- [ ] `repr(C)` / `repr(transparent)` are used only for ABI/layout contracts.
- [ ] Packed layout is avoided unless the unaligned access cost is acceptable.

### Dispatch And Abstraction

- [ ] Static dispatch is used for hot polymorphism.
- [ ] Trait objects are outside tight loops or justified.
- [ ] Iterator chains are kept when they compile well; manual loops require benchmark proof.
- [ ] `collect` is avoided unless a materialized collection is required.
- [ ] Async is used for I/O concurrency, not CPU-bound work.
- [ ] `async_trait`, `Box<dyn Trait>`, `Arc<Mutex<_>>`, `clone`, string formatting, and heap allocation are absent from hot paths unless benchmarked against simpler options.

### Numeric And Low-Level Safety

- [ ] Overflow behavior is explicit.
- [ ] Float comparisons have tolerance when needed.
- [ ] SIMD has scalar fallback, target-feature gate, alignment/remainder handling, and tests.
- [ ] SIMD uses safe `std::simd`, auto-vectorization, or audited safe crate APIs; unsafe SIMD requires explicit prior user waiver.
- [ ] Unsafe code is absent; if unavoidable FFI/SIMD requires it, work stops until an explicit waiver defines preconditions, postconditions, aliasing, lifetimes, tests, and owner approval.
- [ ] FFI boundaries validate ownership, nullability, alignment, and lifetime.

## Mandatory Verification Gate

Always run the correctness minimum before reporting success. If a required command is unavailable, report the exact missing tool, missing repo script, or environment blocker.

### Zero-Slippage Nightly Gate

For Rust code work, every touched workspace must pass this strict gate before acceptance. This is intentionally harsh: zero unsafe, zero unwrap/expect/panic/todo/unimplemented/dbg, no unchecked indexing/string slicing, no arithmetic side effects without explicit handling, no lossy `as` conversions, no ignored must-use values, and no locks held across await. If a tool is unavailable, report the exact missing binary/component as a blocker.

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

cargo +nightly test --workspace --all-features --no-run
cargo +nightly nextest run --workspace --all-features
cargo +nightly doc --workspace --all-features --no-deps
# Production panic-macro scan. Matches in production-reachable code fail review.
if rg -n '(^|[^A-Za-z0-9_])(assert!|assert_eq!|assert_ne!|unreachable!)' --glob '*.rs' --glob '!**/tests/**' --glob '!**/benches/**' --glob '!**/examples/**' --glob '!build.rs'; then exit 1; else true; fi

cargo audit
cargo deny check
cargo vet
cargo geiger
cargo machete
cargo hack check --workspace --feature-powerset
cargo mutants
```

`cargo geiger` is not a waiver. If it reports unsafe in generated or modified production code, the work fails unless the user explicitly approved an unsafe exception before the code was written.

### Minimum Fallback Gate

```bash
cargo fmt --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --lib --bins --examples --all-features -- -D warnings -D unsafe_code -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic -D clippy::panic_in_result_fn -D clippy::todo -D clippy::unimplemented -D clippy::dbg_macro -D clippy::indexing_slicing -D clippy::string_slice -D clippy::get_unwrap -D clippy::arithmetic_side_effects -D clippy::as_conversions -D clippy::let_underscore_must_use -D clippy::await_holding_lock
cargo test --workspace --all-features --no-run
cargo test --workspace --all-features
if rg -n '(^|[^A-Za-z0-9_])(assert!|assert_eq!|assert_ne!|unreachable!)' --glob '*.rs' --glob '!**/tests/**' --glob '!**/benches/**' --glob '!**/examples/**' --glob '!build.rs'; then exit 1; else true; fi
cargo audit
cargo deny check
cargo vet
cargo geiger
cargo machete
cargo hack check --workspace --feature-powerset
cargo mutants
```

Fallback preserves the same lint and tool intent. If any fallback tool or lint is unavailable, report the exact missing tool/lint as a blocker or residual risk. Do not silently drop non-nightly checks. In bead workflows, classify fallback failures against `delivery-scope.jsonl`, `baseline-report.md`, and `global-readiness-report.md`; local, regression, required-obligation, and global-readiness failures block until repaired.

For Moon workspaces, run the repo's canonical gate in addition to or as the documented replacement for equivalent cargo gates:

```bash
moon ci
```

For performance claims, baseline and after-change measurement are mandatory. A named benchmark target, binary, script, or load-test command is required. Generic `cargo bench` is discovery only; if no meaningful benchmark exists, report `no benchmark exists` as a blocker before making a performance claim. Run a real benchmark plus profiler/counter tool, or report a concrete blocker. Never report template command names as executed; replace them with actual repo command names, benchmark names, or binaries.

```bash
# TEMPLATE ONLY - DO NOT REPORT AS RUN
cargo bench --bench actual_bench_name
hyperfine 'actual_repo_command_or_benchmark_binary'
cargo bloat --release --crates
cargo llvm-lines --release
perf stat -- cargo bench --bench actual_bench_name
perf stat -e cycles,instructions,cache-misses,branches,branch-misses -- actual_repo_command
```

## Output Requirements

When implementing or reviewing, report:

- Canonical Power of Ten rules affected and whether each is satisfied.
- Exact reference files read before deciding.
- PLUS performance budget: workload, hot path, latency/throughput target, storage placement, allocation/layout/dispatch assumptions, and threshold.
- Exact commands run and whether they passed.
- Benchmark/profiler numbers for performance claims, including p50/p95/p99 or throughput where relevant.
- Assembly/IR, API compatibility, and release-provenance evidence when a claim or obligation requires second-ring tooling.
- Stack/heap/arena/pool decision and why it is fastest for this workload.
- Any skipped gate and the concrete reason it could not run.
- Remaining risk if only static review was possible.

## References

- `references/nasa-jpl-standards.md` - canonical Power of Ten, Rust mapping, and PLUS performance extensions.
- `references/latency-throughput-playbook.md` - workload classification, latency/throughput optimization, storage placement, profiling, and reporting rules.
- `references/runtime-performance-architecture.md` - prove-slow/execute-fast architecture, dense IR, bounded runtime state, event-log/storage, async shell, and performance harness rules.
- `references/zero-cost-abstractions.md` - allocation, dispatch, iterator, and layout guidance.
- `references/simd-patterns.md` - SIMD safety, feature gating, and benchmarking patterns.
- `references/mechanical-empathy-toolchain.md` - second-ring evidence lanes for assembly/IR, API compatibility, release provenance, and obligation-specific formal tools.

```jsonl
{"kind":"meta","skill":"holzman-rust","version":"2.7.0","format":"markdown-with-embedded-jsonl","mode":"contract-first","doctrine":"power_of_ten_plus_maximum_performance"}
{"kind":"input","arguments":"$ARGUMENTS","rule":"Trigger for Rust implementation, Rust review, performance work, Holzman/Holzmann/JPL/NASA/safety-critical requests, low-level systems code, SIMD, no-panic cleanup, allocation review, latency reduction, throughput optimization, stack-vs-heap decisions, or benchmark-driven optimization."}
{"kind":"mission","goal":"Produce Rust that satisfies the NASA/JPL Power of Ten by default, then proves workload-specific performance among evaluated candidates with latency, throughput, allocation, storage placement, layout, dispatch, SIMD, profiling, and benchmark evidence."}
{"kind":"rule","id":"reference_files_required","level":"fatal","text":"Before Rust implementation, review, or performance advice, read the applicable reference files with the Read tool and list exact filenames used. If a required reference cannot be read, stop and report a blocker."}
{"kind":"rule","id":"power10_simple_control_flow","level":"fatal","text":"No recursion, panic-driven control flow, macro-hidden branching, or hard-to-analyze state transitions in production critical paths."}
{"kind":"rule","id":"power10_bounded_loops","level":"fatal","text":"Strict Power-of-Ten code requires a static upper bound or mathematical termination proof for every loop, retry, traversal, stream drain, and worker poll. Runtime caps, timeouts, and cancellation are service containment, not Rule 2 satisfaction by themselves."}
{"kind":"rule","id":"power10_no_post_init_alloc","level":"fatal","text":"Safety-critical and mission-critical paths must not allocate after initialization. Performance-only hot paths may allocate only with explicit budget, benchmark/profiler evidence, and regression guard."}
{"kind":"rule","id":"power10_short_functions","level":"error","text":"Canonical one-page limit applies; target <=25 logical lines for hot/safety-critical functions unless splitting would hide invariants."}
{"kind":"rule","id":"power10_invariant_density","level":"error","text":"Non-trivial functions must expose production invariants through types, constructors, boundary validation, or side-effect-free checks returning typed errors. debug_assert is supplemental only. Production assert, assert_eq, assert_ne, and unreachable are panic paths except tests, benches, build scripts, or process-start invariant failure with diagnostics."}
{"kind":"rule","id":"power10_checked_results","level":"fatal","text":"Do not ignore Result, Option, join handles, channel sends, flushes, or fallible cleanup. Validate external parameters at boundaries."}
{"kind":"rule","id":"power10_limited_macros_pointers","level":"fatal","text":"Macros and pointer/indirect-call mechanisms must not hide allocation, panics, unsafe, control flow, or target-specific behavior."}
{"kind":"rule","id":"power10_zero_warnings","level":"fatal","text":"Production/source warnings, clippy findings, formatting drift, and static-analysis findings block completion when they are in delivery scope, newly introduced, required by an obligation, or global-readiness policy. Already-present repo-wide failures are BLOCK_GLOBAL prerequisite repair, not deferred evidence. Test implementation style warnings are not a Rust delivery gate; test compile, execution, assertions, determinism, and mutation evidence remain mandatory."}
{"kind":"rule","id":"zero_forbidden_constructs","level":"fatal","bans":["unsafe by default","unwrap","expect","panic","todo","unimplemented","unreachable","production assert macros","unchecked indexing","unchecked arithmetic","ignored Result"],"text":"Generated or modified Rust code must contain zero unsafe by default, unwrap, expect, panic, todo, unimplemented, unreachable, production assert macros, unchecked indexing, unchecked arithmetic, or ignored fallible results."}
{"kind":"rule","id":"no_panic_paths","level":"fatal","bans":["unwrap","expect","panic","todo","unimplemented","unreachable","assert","assert_eq","assert_ne","unchecked indexing in production"],"text":"Production reachable code must not panic. Use typed errors, Option handling, checked access, or proof-carrying newtypes. assert-style macros are forbidden except tests, benches, build scripts, or process-start invariant failure with diagnostics."}
{"kind":"rule","id":"allocation_budget","level":"error","text":"Hot paths require explicit allocation behavior. New heap allocations in hot paths need benchmark or profiler justification."}
{"kind":"rule","id":"latency_budget","level":"error","text":"Latency-sensitive work must define and measure p50/p95/p99 or max latency on the target workload."}
{"kind":"rule","id":"throughput_budget","level":"error","text":"Throughput-sensitive work must define and measure operations/sec, bytes/sec, requests/sec, items/sec, or frames/sec under realistic concurrency and batch size."}
{"kind":"rule","id":"storage_placement","level":"error","text":"Stack, heap, arena, pool, static storage, mmap, or caller-owned buffers must be chosen by measured size, lifetime, locality, reuse, allocation cost, and tail-latency behavior."}
{"kind":"rule","id":"pinned_nightly_required","level":"fatal","text":"Nightly Rust work requires checked-in rust-toolchain.toml with pinned dated nightly, rustfmt, clippy, rust-src, llvm-tools-preview, and target list. Floating nightly is not acceptable."}
{"kind":"rule","id":"nightly_feature_allowlist","level":"fatal","text":"Only portable_simd and try_blocks are allowed by default. Arbitrary feature gates, specialization, first-party std::arch intrinsics, and RUSTC_BOOTSTRAP are policy violations without explicit user approval."}
{"kind":"rule","id":"safe_simd_only","level":"fatal","text":"SIMD must use safe std::simd, auto-vectorization, or audited safe crates. No unsafe SIMD, unchecked loads/stores/gather/scatter, or panic-capable SIMD APIs without immediate length proof and explicit waiver."}
{"kind":"rule","id":"no_unsafe_by_default","level":"fatal","text":"Unsafe is forbidden by default. Explicit user waiver is required before unsafe blocks, unsafe functions, unsafe traits, unsafe impls, raw-pointer dereferences, std::arch intrinsics, unchecked access, or transmute-like code."}
{"kind":"rule","id":"try_reserve_untrusted_growth","level":"fatal","text":"Hot or untrusted-input memory growth must declare max size, use checked arithmetic, call try_reserve when graceful allocation failure matters, and return typed resource errors."}
{"kind":"rule","id":"async_cpu_split","level":"fatal","text":"Tokio is for I/O concurrency, Rayon or bounded CPU pools are for CPU work. No CPU-heavy loops on async workers, unbounded spawn loops, unbounded channels, locks across await, or async_trait in hot paths without measurement."}
{"kind":"rule","id":"hot_path_dependency_gate","level":"error","text":"No hot-path dependency, Rayon, Tokio, SmallVec, custom allocator, fast hasher, SIMD JSON, async_trait, Box<dyn Trait>, Arc<Mutex<_>>, clone, formatting, or heap allocation is accepted without benchmark and threat-model justification."}
{"kind":"rule","id":"prove_slow_execute_fast","level":"fatal","text":"Verification, proof, fuzzing, model checking, spec validation, and graph reasoning happen before acceptance. Production hot paths execute prevalidated dense artifacts only."}
{"kind":"rule","id":"dense_runtime_ir","level":"error","text":"Dense IR is mandatory when accepted human/spec/config data is evaluated repeatedly at runtime. Otherwise require measured proof before replacing simpler streaming or externally keyed representations."}
{"kind":"rule","id":"static_dispatch_hot_path","level":"error","text":"Use static dispatch in hot paths unless runtime polymorphism is required and measured."}
{"kind":"rule","id":"cache_first_layout","level":"error","text":"Review hot data layout for contiguity, field size, false sharing, and AoS vs SoA tradeoffs."}
{"kind":"rule","id":"mechanical_empathy_order","level":"error","text":"Performance work must attack bottlenecks in order: algorithm, memory traffic, data layout, allocation, branch predictability, synchronization/syscalls, compiler visibility, then target-specific builds/SIMD/unsafe-waiver work. Skipping to clever code without profiler evidence is rejected."}
{"kind":"rule","id":"assembly_ir_evidence","level":"error","text":"Claims about zero-cost abstractions, vectorization, bounds-check removal, inlining, code size, or branch shape require cargo-show-asm/cargo asm, cargo llvm-ir, cargo llvm-lines, cargo bloat, perf, or equivalent evidence tied to an actual symbol."}
{"kind":"rule","id":"release_provenance_evidence","level":"error","text":"Public API or release-artifact changes require cargo-semver-checks and auditable/SBOM evidence such as cargo auditable or cargo cyclonedx when the repo supports them; missing tools must be reported as blockers or residual risk."}
{"kind":"rule","id":"second_ring_formal_tools","level":"warn","text":"Crux, SAW, and Hax are obligation-specific second-ring tools for unsafe, bit-precise, extracted, or proof-heavy code. They are not universal gates; run them only when the contract/proof obligation demands it."}
{"kind":"rule","id":"scope_aware_blocking","level":"fatal","text":"Classify Rust gate failures as BLOCK_LOCAL, BLOCK_REGRESSION, BLOCK_GLOBAL, REQUIRED_OBLIGATION_FAIL, or WAIVED. Local, regression, required-obligation, and global-readiness failures block bead delivery; old workspace debt is repaired ahead of time instead of recorded as follow-up evidence."}
{"kind":"rule","id":"unsafe_forbidden_by_default","level":"fatal","text":"Do not write unsafe blocks, unsafe functions, unsafe traits, unsafe impls, raw-pointer dereferences, transmute-like code, or unchecked access. If unavoidable FFI/SIMD requires unsafe, stop and request an explicit user waiver before writing code."}
{"kind":"rule","id":"nightly_max_performance_waiver","level":"fatal","text":"Speed-first nightly techniques that violate default policy require explicit user waiver before code is written, named violated rules, benchmark baseline, target hardware, correctness proof, fallback plan, and final residual-risk report."}
{"kind":"rule","id":"benchmark_proof","level":"fatal","text":"Optimization claims require before/after benchmark or profiler evidence with commands and numbers."}
{"kind":"rule","id":"placeholder_commands_forbidden","level":"fatal","text":"Never report template command names as executed. Replace templates with actual repo commands, benchmark names, binaries, or report a blocker."}
{"kind":"rule","id":"no_hallucinated_evidence","level":"fatal","text":"Never invent CLI output, benchmark numbers, profiler data, or file paths."}
{"kind":"gate","id":"zero_slippage_nightly_gate","commands":["cargo +nightly fmt --all -- --check","cargo +nightly -Zallow-features=portable_simd,try_blocks check --workspace --all-targets --all-features","cargo +nightly -Zallow-features=portable_simd,try_blocks clippy --workspace --lib --bins --examples --all-features -- -D warnings -D unsafe_code -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic -D clippy::panic_in_result_fn -D clippy::todo -D clippy::unimplemented -D clippy::dbg_macro -D clippy::indexing_slicing -D clippy::string_slice -D clippy::get_unwrap -D clippy::arithmetic_side_effects -D clippy::as_conversions -D clippy::let_underscore_must_use -D clippy::await_holding_lock","cargo +nightly test --workspace --all-features --no-run","cargo +nightly nextest run --workspace --all-features","cargo +nightly doc --workspace --all-features --no-deps","if rg -n '(^|[^A-Za-z0-9_])(assert!|assert_eq!|assert_ne!|unreachable!)' --glob '*.rs' --glob '!**/tests/**' --glob '!**/benches/**' --glob '!**/examples/**' --glob '!build.rs'; then exit 1; else true; fi","cargo audit","cargo deny check","cargo vet","cargo geiger","cargo machete","cargo hack check --workspace --feature-powerset","cargo mutants"],"notes":["This is the default high-assurance gate for Rust work. Strict clippy is source-target only; tests are compiled and executed by check/test/nextest, not rejected for implementation style. In bead workflows, classify failures against delivery-scope.jsonl, baseline-report.md, and global-readiness-report.md: local, regression, required-obligation, and global-readiness failures block until repaired. Missing tools/components are blockers for required obligations, not silent skips. cargo geiger does not permit unsafe; unsafe in generated or modified production code fails without explicit prior user waiver. Production assert/unreachable macro matches fail unless explicitly proven non-production."]}
{"kind":"gate","id":"rust_correctness_gate","commands":["cargo fmt --check","cargo check --workspace --all-targets --all-features","cargo clippy --workspace --lib --bins --examples --all-features -- -D warnings -D unsafe_code -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic -D clippy::panic_in_result_fn -D clippy::todo -D clippy::unimplemented -D clippy::dbg_macro -D clippy::indexing_slicing -D clippy::string_slice -D clippy::get_unwrap -D clippy::arithmetic_side_effects -D clippy::as_conversions -D clippy::let_underscore_must_use -D clippy::await_holding_lock","cargo test --workspace --all-features --no-run","cargo test --workspace --all-features","if rg -n '(^|[^A-Za-z0-9_])(assert!|assert_eq!|assert_ne!|unreachable!)' --glob '*.rs' --glob '!**/tests/**' --glob '!**/benches/**' --glob '!**/examples/**' --glob '!build.rs'; then exit 1; else true; fi","cargo audit","cargo deny check","cargo vet","cargo geiger","cargo machete","cargo hack check --workspace --feature-powerset","cargo mutants"],"notes":["Fallback only when nightly/components/tools are blocked. Strict clippy is source-target only; tests are compiled/executed and judged by behavior/assertions/determinism/mutation, not style warnings. Preserve the same lint and non-nightly tool intent where stable tooling supports it; unavailable tools/lints are blockers for required obligations or residual risk, not silent skips. Use moon ci when it is the repo canonical gate, but classify failures by bead scope and baseline."]}
{"kind":"gate","id":"performance_gate","commands":["cargo bench --bench actual_bench_name","hyperfine 'actual_repo_command_or_benchmark_binary'","perf stat -- cargo bench --bench actual_bench_name","perf stat -e cycles,instructions,cache-misses,branches,branch-misses -- actual_repo_command","cargo bloat --release --crates","cargo llvm-lines --release"],"notes":["Performance claims require a named benchmark target, binary, script, or load-test command with baseline and after-change evidence. Generic cargo bench is discovery only. If no meaningful benchmark exists, report no benchmark exists as a blocker. Replace template names with real commands or report blockers. Do not fake unavailable outputs."]}
{"kind":"gate","id":"second_ring_evidence_gate","commands":["cargo asm --lib actual_crate::actual_module::actual_function","cargo llvm-ir --lib actual_crate::actual_module::actual_function","cargo llvm-lines --release","cargo bloat --release --crates","cargo semver-checks --baseline-rev origin/main","cargo auditable build --release","cargo cyclonedx --format json --output-file target/cyclonedx.json"],"notes":["Conditional gate only. Run when a claim or obligation requires assembly/IR, API compatibility, or release-provenance evidence. Replace placeholders with actual crate symbols, package names, binaries, and baseline revisions. Missing tools are blockers or residual risk, not passes."]}
{"kind":"ref","file":"references/nasa-jpl-standards.md","use":"Canonical Power of Ten mapped to Rust plus performance extensions"}
{"kind":"ref","file":"references/latency-throughput-playbook.md","use":"Latency, throughput, storage placement, allocator, profiling, and benchmark decision rules"}
{"kind":"ref","file":"references/runtime-performance-architecture.md","use":"Prove-slow execute-fast architecture, dense IR, bounded hot paths, event-log/storage, async shell, and performance harness rules"}
{"kind":"ref","file":"references/zero-cost-abstractions.md","use":"Allocation, dispatch, iterator, layout, and profiling rules"}
{"kind":"ref","file":"references/simd-patterns.md","use":"Safe SIMD feature gating, scalar fallback, benchmarks, and explicit unsafe-waiver rejection rules"}
{"kind":"ref","file":"references/mechanical-empathy-toolchain.md","use":"Second-ring evidence lanes for assembly/IR, API compatibility, auditable builds, SBOM, Crux, SAW, and Hax"}
```
