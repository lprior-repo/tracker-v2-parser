# Latency And Throughput Playbook

Fast Rust is mechanical empathy: make the CPU, cache, allocator, branch predictor, scheduler, and compiler do less work. Do not optimize for clever Rust. Optimize for the measured workload.

## Core Contract

For every performance-sensitive change, state:

- Target: latency, throughput, memory footprint, CPU efficiency, tail stability, or binary size.
- Workload: input sizes, distribution, error rate, hot path, concurrency, and target hardware.
- Baseline: command, number, variance, and profiler evidence.
- Change: the smallest code change that attacks the measured bottleneck.
- Result: new number, percent/absolute delta, and regression threshold.
- Storage decision: why stack, heap, arena, pool, static storage, mmap, or caller-owned buffers are fastest for this workload.

## Optimization Hierarchy

1. Choose the right algorithmic complexity.
2. Minimize memory traffic.
3. Keep data contiguous and compact.
4. Avoid unnecessary allocation, cloning, formatting, and hashing.
5. Make branches predictable and keep hot loops boring.
6. Avoid synchronization and syscalls in hot paths.
7. Let LLVM see simple, inlinable, alias-clear code.
8. Use target-specific builds, PGO, SIMD, or unsafe-waiver mode only after profiling proves the bottleneck.

## Machine Model

Arithmetic is cheap. Waiting is expensive.

| Layer | Guidance |
|---|---|
| Registers | Best place for scalar hot state; keep loops simple enough for register allocation. |
| L1/L2/L3 cache | Favor compact contiguous layouts and predictable linear scans. |
| RAM | Avoid pointer chasing, random access, and bloated structs. |
| Allocator | Avoid per-item or per-request allocation; reuse buffers or use arenas/pools when lifetimes match. |
| Branch predictor | Avoid random branches in tight loops; split hot/cold paths. |
| Scheduler/locks | Avoid shared locks, contended atomics, and excessive task/thread scheduling. |
| Disk/network | Batch I/O, buffer reads/writes, avoid tiny syscalls. |

## Measurement Tools

Use installed tools; never invent output. Never report template command names as executed. Replace them with actual repo benchmark names, binaries, scripts, or test names; otherwise report the missing command as a blocker.

No performance claim is accepted unless a benchmark exists. No optimization change is accepted unless it includes before/after numbers. No hot-path dependency is accepted unless benchmarked against the simpler option.

For assembly/IR inspection, API compatibility, auditable builds, SBOM output, or Crux/SAW/Hax proof obligations, use `mechanical-empathy-toolchain.md` after the first-ring bottleneck is measured.

Recommended benchmark dependencies when the project has no equivalent:

```toml
[dev-dependencies]
criterion = "0.8"
iai-callgrind = "0.16"
```

```bash
cargo bench
# TEMPLATE ONLY - DO NOT REPORT AS RUN
cargo bench --bench actual_bench_name
hyperfine 'actual_repo_command_or_benchmark_binary'
cargo flamegraph --bench actual_bench_name
perf stat -- cargo bench --bench actual_bench_name
perf stat -e cycles,instructions,cache-misses,branches,branch-misses -- actual_repo_command
valgrind --tool=cachegrind actual_repo_command
heaptrack actual_repo_command
DHAT_OUT_FILE=dhat.json cargo test --release actual_test_name
```

For release profiling quality, consider frame pointers and line tables when the repo allows it:

```bash
RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release
```

Useful profiler installs when missing:

```bash
cargo install flamegraph
cargo install samply
cargo install cargo-nextest
```

Use `cargo flamegraph --release`, `samply record ./target/release/<binary>`, DHAT, heaptrack, or bytehound to locate CPU and allocation bottlenecks. For async services, add `tokio-console`/`console-subscriber` in diagnostic builds so task stalls and resource contention are measured instead of guessed.

## Latency Discipline

Latency-sensitive work must report p50/p95/p99 or max latency when applicable.

- Optimize p99 differently than average latency; allocator spikes, locks, page faults, and queueing dominate tails.
- Avoid per-request allocation and hidden `Arc`/`Mutex`/`HashMap` churn.
- Keep logging, formatting, parsing, retries, and cold errors outside the hot path.
- Prefer bounded queues and explicit backpressure to unbounded buffering.
- Use `#[cold]` for rare error helpers when it improves code layout.

## Throughput Discipline

Throughput-sensitive work must report operations/sec, bytes/sec, requests/sec, items/sec, or frames/sec under realistic batching and concurrency.

- Batch I/O and CPU work when it improves amortization.
- Prefer contiguous scans over pointer-heavy traversal.
- Use per-thread local buffers and merge later instead of shared hot locks.
- Parallelize only when work per item is large enough to overcome scheduling overhead.
- Measure scaling by thread count before claiming parallel speedup.

## Storage Placement Decision

Stack is not automatically fastest. Heap is not automatically slowest. Choose by measurement and shape.

| Placement | Prefer When | Reject When |
|---|---|---|
| Stack value or array | Small, fixed-size, short-lived, copied rarely, no dynamic growth. | Large values, recursive/deep frames, variable size, expensive moves, or stack pressure. |
| `Vec` / `String` heap buffer | Variable size, contiguous scan, caller needs ownership, buffer can be reused. | Per-iteration allocation, unknown capacity, or ownership not needed. |
| Caller-owned output buffer | Repeated operations, encoding/parsing/serialization, request loops. | API simplicity matters more and path is cold. |
| Arena | Many objects share one lifetime: parsers, ASTs, graphs, request-scoped objects. | Objects have independent lifetimes or arena retention hurts memory. |
| Pool | Repeated same-sized buffers/objects and allocator churn is measured. | Pool contention, complexity, or stale memory dominates. |
| `SmallVec` / `ArrayVec` | Tiny collections dominate and benchmark proves stack inline storage wins. | `N` or element size bloats the parent object, copies get slower, or overflow is common. |
| `Box` rare large variant | Keeps common enum/result size small and rare branch cold. | Boxing puts common hot data behind a pointer. |
| Static/lazy data | Immutable tables, lookup constants, shared read-only config. | Initialization cost, synchronization, or cache footprint hurts startup/hot path. |
| mmap/file-backed | Huge data, random access, OS paging is beneficial. | Tail latency cannot tolerate page faults. |

## Data Layout Rules

Store together what is accessed together.

- Use `Vec<T>` by default for hot homogeneous data.
- Avoid linked lists and pointer-heavy object graphs in hot paths.
- Prefer indexes into `Vec<Node>` over `Rc<RefCell<Node>>` graphs when possible.
- Use array-of-structs when loops touch most fields together.
- Use struct-of-arrays when loops scan one or a few fields and SIMD/cache locality matter.
- Check `std::mem::size_of::<T>()` for hot types.
- Reorder fields to reduce padding when ABI does not matter.
- Box rare large enum variants to keep common values compact.
- Use compact numeric types when range proof exists and memory bandwidth matters.
- Avoid `repr(packed)` unless unaligned access is handled and measured.

Example layout decision:

```rust
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

struct Particles {
    x: Vec<f32>,
    y: Vec<f32>,
    vx: Vec<f32>,
    vy: Vec<f32>,
}
```

If every loop updates all fields together, `Vec<Particle>` can be right. If hot loops scan only `x` or `vx`, `Particles` can reduce cache misses and improve SIMD opportunities.

## Allocation And Reuse Rules

- Prefer `&str`, `&[T]`, and `&mut [T]` parameters when ownership is not required.
- Prefer `fn parse_into(input: &[u8], scratch: &mut Scratch, out: &mut Output)` shapes for hot APIs.
- Use `Vec::with_capacity` or `String::with_capacity` when size is known or bounded.
- Reuse buffers with `clear()` when capacity should be retained.
- Avoid `format!`, `to_string`, `to_owned`, and `collect::<Vec<_>>()` in hot loops unless measured.
- Treat `clone()` as a cost. `Arc::clone` avoids allocation but still has atomic traffic.
- Prefer `Cow` only when the borrowed/owned split actually avoids hot allocations.
- Use allocator changes only after reducing allocation rate; changing allocator is not the first move.

## Hot Loop Rules

Hot loops should be boring.

- Move parsing, allocation, logging, dynamic dispatch, capacity growth, and error formatting out of the loop.
- Prefer iteration over slices to indexing when it lets LLVM remove bounds checks naturally.
- Keep loop-carried state small and register-friendly.
- Split hot and cold paths; use `#[cold]` for rare error construction when justified.
- Compare branchy and branchless forms only with benchmarks; branchless code can do extra work.
- Manual loops are acceptable when they prove clearer bounds, fewer branches, fewer allocations, or better vectorization.
- Iterator chains are acceptable when they stay lazy and compile well.

## Data Structure Selection

- Use direct indexing into `Vec`/arrays for small integer IDs.
- Use sorted `Vec` plus binary search for small or mostly-read maps when it beats hashing.
- Use `HashMap` only when hashing is the right tradeoff; choose hasher by threat model and benchmark.
- Avoid linked lists for hot data.
- Use enum indexing, arrays, perfect hashes, or dense vectors when the key set is bounded.

## Dispatch And Code Size

- Static dispatch helps hot loops by enabling inlining and specialization.
- `dyn Trait`, callbacks, and function pointers can be fine outside hot loops.
- Too much monomorphization can increase binary size and hurt instruction cache.
- Move non-generic work out of generic functions when `cargo llvm-lines` shows IR bloat.
- Check `cargo bloat` and `cargo llvm-lines` when changing generics, inlining, or features.

## Serialization, Parsing, And Strings

- Use `&[u8]` for byte protocols and ASCII-like parsing; do not pay UTF-8 abstraction cost unless needed.
- Use `&str` for text reads and `String` only when ownership/mutation is required.
- Write into existing `String`/`Vec<u8>` buffers via `write!`, `writeln!`, or direct byte writes.
- Avoid `BufRead::lines()` in hot file readers because it allocates a `String` per line; reuse a `String` with `read_line`.
- Batch writes with `BufWriter` or `write_all` on chunks, not per byte.

## Concurrency Rules

- Do not parallelize blindly; scheduling overhead can dominate small work.
- Prefer Rayon for large, independent CPU-bound data transforms.
- Prefer async for I/O concurrency, not CPU-heavy computation.
- Never hold async locks across `.await` unless deliberately proven safe and cheap.
- Avoid shared `Mutex` in hot paths; prefer sharding, local accumulation, atomics, message passing, or per-thread buffers.
- Avoid contended atomics; local counts plus final merge often wins.
- Avoid false sharing with per-thread buffers or cache-line padding for hot counters.

## Build And Compiler Settings

Use release mode before judging speed:

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
```

For maximum local runtime speed, test variants instead of assuming: default release, thin LTO, fat LTO, `codegen-units = 1`, target CPU settings, PGO, binary size, compile time, and deployment portability.

Do not default services to `panic = "abort"`; keep panic boundaries possible unless the binary is supervised batch work where process termination is acceptable.

Example local override shape:

```toml
[profile.release]
opt-level = 3
# Benchmark default release against thin LTO and fat LTO for this workload.
# Benchmark codegen-units = 1 against the project's default.
# Use panic = "abort" only when unwinding is not part of the product contract.
```

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

Do not ship `target-cpu=native` to unknown CPUs. Use it only for controlled fleets, internal tools, or per-target release artifacts.

For stable production workloads, consider PGO:

1. Build instrumented binary.
2. Run realistic workload.
3. Rebuild using collected profile.
4. Verify benchmark and correctness gates.

## Full Nightly Max-Performance Waiver Mode

Default policy prefers strict zero-slippage. If the user explicitly approves speed-first nightly work, evaluate the largest likely wins first and document every rule waiver.

Candidate profile for local maximum-throughput experiments:

```toml
[profile.release]
opt-level = 3
codegen-units = 1
lto = "fat"
debug = false
debug-assertions = false
overflow-checks = false
panic = "abort"
```

Candidate local CPU build:

```bash
RUSTFLAGS="-C target-cpu=native" cargo +nightly build --release
rustc --print cfg -C target-cpu=native
```

Evaluate, in order: release profile variants, target-specific CPU flags, PGO/BOLT, safe `portable_simd`, runtime feature dispatch, SoA/cache layout, allocator or arena changes, and only then explicit unsafe-waiver intrinsics such as `std::arch`, `core_intrinsics::likely`, `core_intrinsics::unlikely`, `core_intrinsics::assume`, unchecked arithmetic, or `unreachable_unchecked`.

Speed-first exceptions still require before/after benchmarks, profiler evidence, target hardware, fallback/deployment limits, and written proof for overflow, bounds, aliasing, initialization, layout, and unwind assumptions.

## OOM And Growth Discipline

Hot paths and untrusted-input paths that grow memory must not rely on infallible-looking allocation APIs.

- Declare maximum sizes and input bounds.
- Use checked arithmetic for capacity calculations.
- Call `try_reserve` before growth when allocation failure must be graceful.
- Return typed allocation/resource errors.
- Prefer reusable buffers and caller-owned scratch space before allocator swaps.

## Hard Performance Bans

- Do not optimize by vibes.
- Do not add Rayon because "parallel is faster"; require scaling evidence.
- Do not add Tokio for CPU work.
- Do not add `SmallVec` unless inline capacity is justified by distribution and benchmark.
- Do not add a custom allocator unless allocation profiling says allocator pressure remains.
- Do not use fast non-cryptographic hashing for adversarial/user-controlled keys.
- Do not use `async_trait`, `Box<dyn Trait>`, `Arc<Mutex<_>>`, `clone`, string formatting, or heap allocation in hot paths unless benchmarked against simpler options.

## Zero-Slippage Nightly Gate

Every Rust workspace touched by performance work must pass this gate or report the exact missing tool/component as a blocker.

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

## Unsafe Last

Unsafe-waiver mode may help with unchecked indexing, manual SIMD, FFI, custom allocators, specialized layout, or avoiding initialization costs. It does not automatically make code fast and is not allowed without explicit prior user approval.

Before unsafe:

- Benchmark and profile the safe version.
- Prove the bottleneck is bounds checks, initialization, aliasing, or layout.
- Try safe loop shapes that let LLVM remove checks.
- Keep unsafe blocks tiny and behind safe APIs.
- State invariants and test with unit/property/fuzz/sanitizers or dedicated UB tooling where applicable.

`MaybeUninit` is allowed only when initialization cost is measured and the implementation proves every read observes initialized data.

## Review Checklist

Ask these before accepting performance work:

- Is the algorithm right?
- Is the data contiguous?
- Are we allocating, formatting, hashing, or cloning in the hot path?
- Are APIs borrowing slices or forcing ownership?
- Would a reusable scratch/output buffer remove churn?
- Would direct indexing, sorted vec, enum indexing, or arrays beat a hash map?
- Are hot branches predictable?
- Are error/logging/cold paths separated?
- Are locks, atomics, tasks, or syscalls in the hot path?
- Are hot structs compact and padding-aware?
- Is dynamic dispatch in an inner loop?
- Did profiling prove this is the bottleneck?
- Did the benchmark use realistic input size, distribution, concurrency, and hardware?

## Reporting Template

```text
Workload:
Target: latency | throughput | memory | CPU | tail stability
Baseline command:
Baseline result:
Profiler evidence:
Bottleneck:
Reference files used:
Storage placement decision:
Change made:
Correctness gate:
Performance gate:
New result:
Delta:
Regression threshold:
Residual risk:
```
