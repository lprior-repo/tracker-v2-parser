# Zero-Cost Performance Rules

Zero-cost means the abstraction compiles away or its cost is deliberately paid and measured. It never means "the code looks idiomatic, therefore it is fast." Mechanical empathy wins: fewer bytes moved, fewer cache misses, fewer allocations, fewer unpredictable branches, fewer locks, fewer virtual calls, fewer syscalls.

## Cost Ledger

For every hot path, identify these costs before editing:

| Cost | What To Inspect | Preferred Shape |
|---|---|---|
| Allocation | `String`, `Vec`, `Box`, `Arc`, `HashMap`, `format!`, `collect` | Borrow, preallocate, stack buffer, arena, caller-owned output |
| Dispatch | `dyn Trait`, virtual calls, trait objects in loops | Generics, enums, inlined concrete types |
| Storage placement | stack vs heap vs arena vs pool vs caller-owned buffer | measured choice by size, lifetime, locality, reuse |
| Layout | large structs, pointer chasing, poor field order | contiguous arrays, smaller fields, SoA for scans |
| Branching | unpredictable branches in tight loops | precompute, split paths, table lookup, state machine |
| Synchronization | `Arc<Mutex<T>>`, global locks, shared counters | ownership transfer, sharding, atomics, message passing |
| Error handling | common path carrying rare failures | validate once at boundary, compact error representation |

## Allocation Discipline

Reject performance work that does not know whether it allocates.

- Prefer `&str`, `&[T]`, and `&mut [T]` for parameters.
- Return owned values only when the caller needs ownership.
- Prefer caller-provided output buffers for repeated work.
- Use `Vec::with_capacity` only when the capacity estimate is justified.
- Use `SmallVec` or `ArrayVec` only when benchmarks prove small sizes dominate.
- Avoid `format!` in hot loops; use `write!` into a reusable buffer.
- Treat `Clone` as a cost, not a convenience.
- Reuse buffers with `clear()` when capacity should be retained.
- Prefer `parse_into`, `encode_into`, or caller-owned `Scratch` APIs for repeated hot operations.
- Choose stack, heap, arena, pool, or caller-owned buffers with measurement; stack is not automatically faster and heap is not automatically slower.

For hot paths or untrusted input, growth must be fallible and bounded:

- State the maximum input size, output size, and allocation count.
- Use checked arithmetic for capacity and byte-size calculations.
- Call `try_reserve` before growth when allocation failure must be graceful.
- Return typed resource errors instead of panicking or aborting unexpectedly.
- Avoid `Vec::new` followed by unbounded push loops on untrusted data.

## Storage Placement Discipline

Pick storage by workload, not folklore:

- Stack arrays are best for small, fixed-size, short-lived values that do not inflate frames or copy costs.
- `Vec`/`String` are best for variable-size contiguous buffers when capacity can be reused or preallocated.
- Caller-owned buffers are best for repeated encode/decode/parse/transform loops.
- Arenas are best when many objects share one lifetime and locality improves.
- Pools help only when allocator churn is measured and pool contention does not replace allocator contention.
- Boxing rare large enum variants can shrink the common path and improve cache behavior.
- Large stack values, deep recursion, and per-request heap churn fail review unless measured safe and faster.

## Dispatch Discipline

Static dispatch is the default in hot paths:

```rust
fn process<P: Processor>(processor: &P, input: &[u8]) -> Result<usize, Error> {
    processor.process(input)
}
```

Use `dyn Trait` only when runtime polymorphism is required:

```rust
fn process_plugin(processor: &dyn Processor, input: &[u8]) -> Result<usize, Error> {
    processor.process(input)
}
```

Review questions:

- Is the call inside a tight loop?
- Does monomorphization bloat matter more than vtable cost?
- Is runtime plugin behavior actually needed?
- Did a benchmark compare both shapes under real workload?

## Iterator And Loop Discipline

Iterator chains are acceptable when they stay lazy and compile well. Manual loops are acceptable when they prove clearer bounds, fewer branches, fewer allocations, or better vectorization.

Reject both extremes:

- Do not replace clear iterators with index loops without evidence.
- Do not keep allocation-heavy iterator chains because they look idiomatic.
- Do not use `collect::<Vec<_>>()` unless the materialized vector is required.
- Do not use unchecked indexing unless bounds are proven and the gain is measured.

## Data Layout Discipline

For hot structs and collections:

- Check `std::mem::size_of::<T>()` for hot data types.
- Reorder fields to reduce padding when ABI does not matter.
- Prefer arrays/slices over linked structures for scans.
- Consider struct-of-arrays for SIMD and cache-friendly filtering.
- Avoid `repr(packed)` unless unaligned access is explicitly handled.
- Isolate frequently written atomic counters to avoid false sharing.
- Store together what is accessed together; split fields into struct-of-arrays when hot loops scan only one or two fields.
- Prefer indexes into `Vec<Node>` over `Rc<RefCell<Node>>` graphs when ownership and mutation patterns allow it.
- Consider boxing rare large error/enum variants to keep hot `Result` and enum values small.

## Hot Loop Discipline

Hot loops should be boring and hardware-friendly:

- Move parsing, allocation, logging, formatting, dynamic dispatch, capacity growth, and error construction out of tight loops.
- Prefer slice iteration and `zip` shapes that make bounds and aliasing obvious to LLVM.
- Compare branchy and branchless forms with benchmarks; do not assume branchless wins.
- Use `#[cold]` for rare error helpers only when it improves measured layout or branch behavior.
- Keep loop-carried state small enough for registers.

## Hashing Discipline

Hash choice is a threat-model decision:

- Use `std::collections::HashMap` when DoS resistance matters.
- Use faster hashers only when input is trusted or keyed protection exists.
- Use sorted vectors, arrays, perfect hash, or enum indexing when the key set is bounded.
- Benchmark lookup, insertion, memory, and build cost separately.

## Dependency And Abstraction Bans

Do not add machinery because it feels fast:

- No Rayon unless work is CPU-bound, independent, large enough, and scaling is measured.
- No Tokio for CPU-heavy loops.
- No `SmallVec` unless inline capacity is justified by observed distribution and benchmark.
- No custom allocator unless heap profiling proves allocator pressure remains after allocation reduction.
- No fast non-cryptographic hasher for adversarial or user-controlled keys.
- No `async_trait`, `Box<dyn Trait>`, `Arc<Mutex<_>>`, `clone`, `format!`, or heap allocation in hot paths unless benchmarked against the simpler option.

## Measurement Commands

Use whatever exists in the repo. Do not add tools blindly. Never report template command names as executed. Replace them with actual repo commands or report a blocker.

```bash
cargo bench
# TEMPLATE ONLY - DO NOT REPORT AS RUN
cargo bench --bench actual_bench_name
hyperfine 'actual_repo_command_or_benchmark_binary'
cargo bloat --release --crates
cargo llvm-lines --release
perf stat -- cargo bench --bench actual_bench_name
perf stat -e cycles,instructions,cache-misses,branches,branch-misses -- actual_repo_command
```

For allocation evidence, use an available allocator profiler or instrumentation:

```bash
# TEMPLATE ONLY - DO NOT REPORT AS RUN
DHAT_OUT_FILE=dhat.json cargo test --release actual_test_name
```

## Zero-Slippage Nightly Gate

Every Rust workspace touched by abstraction/performance work must pass this gate or report the exact missing tool/component as a blocker.

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

## Reporting Standard

Every optimization report must include:

- Baseline number and optimized number.
- Input sizes and workload description.
- Tool and command used to measure.
- Percent change or absolute latency/throughput change, including p50/p95/p99 when latency matters.
- Stack/heap/arena/pool/caller-buffer decision and why it is fastest for the measured workload.
- Binary-size or compile-time tradeoff when monomorphization was changed.
- Risk statement for target-specific behavior.
