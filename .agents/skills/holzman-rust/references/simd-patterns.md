# SIMD And Low-Level Rust Patterns

SIMD is allowed only when scalar code is correct, the workload is hot, and measurements show a win. Safe SIMD, auto-vectorization, and crate-provided safe APIs are preferred. Hand-written unsafe SIMD is forbidden by default; if target intrinsics make unsafe unavoidable or speed-first nightly work explicitly asks for `std::arch`, stop and request an explicit user waiver before writing it.

## When SIMD Applies

Use SIMD for repeated operations over homogeneous data:

- Numeric reductions and transforms.
- Image, audio, video, and signal processing.
- Parsing and scanning byte streams.
- Checksums, hashes, compression, and crypto-style kernels.
- Packet processing and columnar data scans.

Do not use SIMD when input is tiny, branch-heavy, allocation-heavy, or dominated by I/O.

## Required Shape

Every SIMD implementation needs:

- Scalar reference implementation.
- Scalar fallback for unsupported targets.
- Target-feature gate or runtime feature detection.
- Alignment and remainder handling.
- Tests comparing scalar and SIMD outputs across edge cases.
- Benchmark showing the SIMD path wins for intended sizes.
- Zero unsafe in generated or modified code unless the user explicitly approved an unsafe waiver before implementation.

## Safe SIMD First Pattern

Prefer simple scalar loops, iterator/zip shapes that help LLVM auto-vectorize, or project-approved safe SIMD crates/APIs. Keep the scalar oracle and fallback even when the optimized path is safe.

```rust
fn sum_f32_scalar(input: &[f32]) -> f32 {
    input.iter().copied().sum()
}

pub fn sum_f32(input: &[f32]) -> f32 {
    // Safe baseline. Replace only with a safe SIMD API after benchmark proof.
    sum_f32_scalar(input)
}
```

## Unsafe Waiver Pattern

Do not write this by default. If FFI or target intrinsics make unsafe unavoidable, stop and obtain explicit user approval. The waiver must name:

- Why safe Rust, auto-vectorization, and safe SIMD APIs are insufficient.
- CPU features and fallback behavior.
- Bounds, alignment, aliasing, lifetime, initialization, and remainder invariants.
- Tests: scalar-vs-optimized equivalence, edge cases, sanitizer or dedicated UB tooling where applicable.
- Benchmarks proving the unsafe path wins on the target workload.

Nightly max-performance waiver mode may evaluate `stdarch_x86_avx512`, `#[target_feature]`, runtime feature dispatch, `core_intrinsics::likely`, `core_intrinsics::unlikely`, `core_intrinsics::assume`, unchecked arithmetic, or `unreachable_unchecked`. Each use needs explicit approval, current nightly API verification, scalar fallback, target hardware, correctness proof, and before/after benchmark evidence.

## Portable SIMD Pattern

Use nightly `std::simd` only when the project pins nightly and allows exactly the needed source feature. Keep the same scalar fallback and benchmark standard.

Required crate policy:

```rust
#![feature(portable_simd)]
#![forbid(unsafe_code)]
```

Required cargo gate:

```bash
cargo +nightly -Zallow-features=portable_simd,try_blocks check --workspace --all-targets --all-features
```

Prefer non-panicking SIMD APIs. Panic-capable APIs such as short-slice loads or short destination copies require an immediate local length proof above the call and tests for empty, one, `LANES - 1`, `LANES`, `LANES + 1`, non-multiple, and large inputs. Prefer `load_or_default`, `load_or`, `load_select`, `gather_or`, and `gather_or_default` where they fit.

```rust
fn sum_chunks(input: &[f32]) -> f32 {
    let chunks = input.chunks_exact(4);
    let remainder = chunks.remainder();

    let chunk_sum = chunks.fold(0.0f32, |acc, chunk| {
        acc + chunk.iter().copied().sum::<f32>()
    });

    chunk_sum + remainder.iter().copied().sum::<f32>()
}
```

Replace the chunk body with project-approved SIMD types only after correctness tests and baseline benchmarks exist.

## Alignment And Remainder Rules

- Never assume alignment unless the type or allocation guarantees it.
- Prefer unaligned loads when the architecture handles them cheaply and benchmark confirms.
- Always process `chunks_exact(N).remainder()`.
- Test empty, short, exactly-one-vector, non-multiple, and large inputs.
- Verify NaN, signed zero, overflow, and rounding behavior for floats when relevant.

## Unsafe Waiver Invariant Template

Only after explicit waiver, every unsafe SIMD block needs a comment that states:

```rust
// SAFETY:
// - CPU feature: <feature> is proven by <cfg/runtime detection>.
// - Bounds: chunks_exact ensures each load has <N> initialized elements.
// - Alignment: load intrinsic accepts unaligned input, or alignment is proven by <source>.
// - Aliasing: input is shared read-only, output is unique.
// - Remainder: scalar path handles trailing elements.
```

## Benchmark Pattern

Use realistic sizes and compare scalar against SIMD.

```rust
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn bench_sum(c: &mut Criterion) {
    let mut group = c.benchmark_group("sum_f32");

    for size in [16usize, 256, 4096, 65_536] {
        let data: Vec<f32> = (0..size).map(|n| n as f32).collect();

        group.bench_with_input(BenchmarkId::new("scalar", size), &data, |b, data| {
            b.iter(|| sum_f32_scalar(black_box(data)));
        });

        group.bench_with_input(BenchmarkId::new("auto", size), &data, |b, data| {
            b.iter(|| sum_f32(black_box(data)));
        });
    }
}

criterion_group!(benches, bench_sum);
criterion_main!(benches);
```

## Rejection Triggers

- SIMD without scalar correctness oracle.
- Target intrinsics without `cfg` or runtime detection.
- `std::arch` intrinsics in first-party code without explicit user approval.
- `std::simd` without pinned nightly and `-Zallow-features=portable_simd,try_blocks` gate.
- Panic-capable SIMD API without immediate length proof and edge-length tests.
- Missing property tests comparing scalar and SIMD paths.
- Unsafe without explicit prior user waiver.
- Unsafe blocks with no invariant comment, tests, or benchmark proof.
- Missing remainder path.
- Benchmarks only on one tiny input size.
- Different numeric semantics from scalar path without documented acceptance.

## Zero-Slippage Nightly Gate

SIMD and low-level changes must pass the same strict workspace gate as the main skill. Missing components are blockers.

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
