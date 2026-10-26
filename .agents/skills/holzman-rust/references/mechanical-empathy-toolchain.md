# Mechanical Empathy Toolchain

This reference is the second-ring evidence lane. Do not start here. First use the latency and throughput playbook: algorithm, memory traffic, data layout, allocation, branch predictability, synchronization/syscalls, and compiler-visible code. Use these tools when a performance, API, release, unsafe, or proof obligation needs evidence beyond normal benchmarks.

## Rule

No tool name is evidence by itself. Evidence means command, target symbol or binary, workload, observed output summary, exit status, and residual risk.

Do not report template commands as executed. Replace `actual_*` placeholders with real crate names, package names, symbols, binaries, baseline revisions, or proof projects. If the repo lacks the tool or target, report a blocker or residual risk.

## Tool Selection Matrix

| Claim or obligation | Tool lane | Acceptable evidence |
|---|---|---|
| Faster hot path | `criterion`, `iai-callgrind`, `hyperfine`, repo load test | Before and after numbers, variance, target hardware, regression threshold. |
| CPU/cache bottleneck | `perf stat`, `cargo flamegraph`, `samply`, `cachegrind` | Cycles, instructions, cache misses, branch misses, flamegraph or sampled hot symbol. |
| Allocation bottleneck | `heaptrack`, DHAT, bytehound, allocator counters | Allocation count/bytes before and after, allocation site, peak memory. |
| Zero-cost abstraction | `cargo asm`, `cargo llvm-ir`, `cargo llvm-lines`, `cargo bloat` | Actual symbol or crate report showing no unwanted dispatch, allocation, or IR bloat. |
| Vectorization or bounds-check removal | `cargo asm`, `cargo llvm-ir`, `perf stat`, scalar oracle benchmark | Target-specific assembly/IR plus scalar fallback correctness and benchmark delta. |
| Public API compatibility | `cargo semver-checks` | Baseline revision, package name, pass/fail output, explicit accepted breaking changes. |
| Release provenance | `cargo auditable`, `cargo cyclonedx`, `cargo deny`, `cargo vet` | Auditable binary or SBOM artifact, dependency policy report, residual supply-chain risk. |
| Unsafe waiver or bit-precise kernel | Kani, fuzz, Crux, SAW, Hax, or dedicated UB tooling as required by obligation | Proof/harness command, assumptions, covered function, unsupported language features. |

## Assembly And IR Evidence

Use assembly/IR only when it answers a concrete question: did LLVM inline this, remove bounds checks, vectorize this loop, erase abstraction overhead, or bloat the binary?

```bash
# TEMPLATE ONLY - DO NOT REPORT AS RUN
cargo asm --lib actual_crate::actual_module::actual_function
cargo llvm-ir --lib actual_crate::actual_module::actual_function
cargo llvm-lines --release
cargo bloat --release --crates
```

Reject assembly evidence that is not tied to the exact hot symbol, target CPU, release profile, and workload used by the benchmark.

## API And Release Evidence

Use this lane when the change alters public crates, release binaries, dependency posture, or production artifacts.

```bash
# TEMPLATE ONLY - DO NOT REPORT AS RUN
cargo semver-checks --baseline-rev origin/main
cargo auditable build --release
cargo cyclonedx --format json --output-file target/cyclonedx.json
cargo deny check
cargo vet
```

`cargo semver-checks` does not replace tests. `cargo auditable` and CycloneDX do not prove correctness. They prove that release artifacts and dependency metadata are inspectable.

## Formal Second Ring

Crux, SAW, and Hax are not universal gates. Use them only when the contract or proof-obligations file asks for bit-precise reasoning, extracted model checking, cryptographic/codec proof, unsafe boundary proof, or refinement beyond Lean/Kani/fuzz.

Expected reporting:

```text
Tool:
Obligation ID:
Target function/module:
Assumptions:
Unsupported Rust features:
Command:
Exit status:
Evidence artifact:
Residual risk:
```

If the tool cannot model the Rust feature used by the target, that is a finding. Do not turn unsupported language coverage into a pass.

## Acceptance Checklist

- The first-ring bottleneck was measured before second-ring tools were used.
- Every command names a real benchmark, binary, package, symbol, or proof project.
- Assembly/IR claims cite the exact symbol and release flags.
- API compatibility claims cite the baseline revision.
- SBOM/provenance claims cite the generated artifact path.
- Formal-tool claims cite the proof obligation and unsupported-feature limits.
- Missing tools are reported as blockers or residual risk, never as success.
