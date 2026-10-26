---
name: functional-rust
description: "Strict functional-first Rust generator for AI-assisted Rust: zero-panic purity (Data-Calc-Actions), type-driven design, newtypes, thiserror errors, and Holzmann NASA/JPL reliability. Use whenever writing, fixing, or reviewing Rust code — especially CI repairs, functional architecture, zero-unwrap enforcement, AI code hardening, or bead workflow implementation."
---

```jsonl
{"kind":"meta","skill":"functional-rust","version":"5.0.0","updated":"2026-03","format":"markdown-with-embedded-jsonl","compressed":true}
{"kind":"hierarchy","id":"data_calculations_actions","text":"Organize: Data (inert) → Calculations (pure fn) → Actions (I/O). Refactor: Actions→Calc→Data.","order":["Data","Calculations","Actions"],"strategy":"Push logic RIGHT: Actions→Calc→Data"}

// -----------------------------------------------------------------------------
// CORE DOCTRINE: ABSOLUTE RELIABILITY (HOLZMANN NASA/JPL + AI GUARDRAILS)
// -----------------------------------------------------------------------------
{"kind":"rule","id":"no_unwrap_anywhere","level":"fatal","text":"NEVER use unwrap in any form. ZERO exceptions.","bans":["unwrap","expect","panic!","unwrap_unchecked","unwrap_or","unwrap_or_else","unwrap_or_default"],"preferred":["match","if let","map","and_then","ok_or_else","map_or_else"],"notes":["AI Guardrail: Ban includes Option::unwrap_or*_ and Result::unwrap_or*_. This codebase is mathematically sound."]}
{"kind":"rule","id":"no_swallowed_errors","level":"fatal","text":"Every failure path must be handled, logged, or explicitly returned.","bans":["catch with empty block","ignore Result"],"notes":["AI Guardrail: Silent failures corrupt state silently. Nothing gets swallowed. Ever."]}
{"kind":"rule","id":"linear_control_flow","level":"error","text":"Keep it linear. No deep nesting.","bans":["nesting > 2 levels"],"preferred":["early returns (with errors)","flat logic"],"notes":["AI Guardrail: If logic requires more than 2 levels of nesting, decompose immediately."]}
{"kind":"rule","id":"no_imperative_loops","level":"fatal","text":"Zero imperative loops. Use Iterator/Stream pipelines.","bans":["for","while","loop {}"],"preferred":["map","fold","reduce","itertools","rayon::par_iter"],"notes":["AI Guardrail: Iterators are naturally bounded by data. For truly infinite I/O streams in the shell, you MUST enforce a tokio::time::timeout or .take(N)."]}
{"kind":"rule","id":"one_function_one_job","level":"error","text":"Max ~60 lines per function.","bans":["monolithic functions"],"notes":["AI Guardrail: AI optimizes for task completion. Force decomposition upfront."]}
{"kind":"rule","id":"state_assumptions","level":"error","text":"Add assertions for preconditions and postconditions.","preferred":["debug_assert!","explicit boundary parsing"],"notes":["Make implicit assumptions explicit and checkable at runtime."]}
{"kind":"rule","id":"surface_side_effects","level":"error","text":"I/O and mutations must be explicitly named and isolated to the Actions layer.","bans":["hidden I/O in helpers"],"notes":["A function that looks pure but has side effects is lethal debt."]}

// -----------------------------------------------------------------------------
// FUNCTIONAL ARCHITECTURE
// -----------------------------------------------------------------------------
{"kind":"principle","id":"make_illegal_states_unrepresentable","text":"Use enums for state machines. Each variant has exactly valid fields.","ex_bad":"Order{shipped:bool,addr:Option<Address>}","ex_good":"enum Order{Draft(Draft),Validated(Validated),Shipped(Shipped)}"}
{"kind":"principle","id":"parse_dont_validate","text":"Parse at boundary into trusted types. Once parsed, data is always valid.","ex":"struct Email<'a>(&'a str); impl<'a> Email<'a>{fn parse(s:&'a str)->Result<Self,Err>}"}
{"kind":"rule","id":"no_mut_by_default","level":"error","text":"Zero mut by default. Prefer pure transforms.","bans":["let mut","mut "],"preferred":["fold","scan","map","filter","collect"]}

// -----------------------------------------------------------------------------
// MAXIMUM PERFORMANCE (ZERO-COPY, ZERO-HEAP, PARALLELISM)
// -----------------------------------------------------------------------------
{"kind":"performance","id":"zero_copy_parsing","text":"Parse boundaries MUST use borrowed lifetimes or zero-copy types.","preferred":["&'a str","&'a [u8]","bytes::Bytes"],"bans":["String allocations during parsing"]}
{"kind":"performance","id":"clone_on_write","text":"Use Cow<'a, T> when mutation is only sometimes required.","preferred":["std::borrow::Cow"],"notes":["Only allocates when actual mutation occurs."]}
{"kind":"performance","id":"zero_heap_collections","text":"Return SmallVec for short-lived collections to prevent heap allocation.","preferred":["smallvec::SmallVec"],"bans":["Vec for small, predictable sized returns"]}
{"kind":"performance","id":"late_collection","text":"Avoid intermediate allocations in pipelines.","bans":["collecting into Vec mid-pipeline"],"preferred":["chain iterators until the final consumer"]}
{"kind":"performance","id":"parallel_pipelines","text":"Use Rayon for massive data transformations.","preferred":["rayon::prelude::*","par_iter()","into_par_iter()"],"notes":["Free multithreading for pure functional pipelines."]}
{"kind":"performance","id":"fold_over_push","text":"Accumulate state in CPU registers via fold/reduce.","preferred":["Iterator::fold","Iterator::reduce"],"bans":["pushing to mutable vectors in a loop"]}

// -----------------------------------------------------------------------------
// STACKS & GATES: THE PERFECT 10
// -----------------------------------------------------------------------------
{"kind":"stack","crate":"itertools","use":"ergonomic sync pipelines","when":"core"}
{"kind":"stack","crate":"rayon","use":"CPU-saturating parallel pipelines","when":"core + shell"}
{"kind":"stack","crate":"rpds","use":"immutable, structural-sharing state","when":"core (state snapshots)"}
{"kind":"stack","crate":"bytes","use":"zero-copy network/parsing payloads","when":"parsing boundary"}
{"kind":"stack","crate":"smallvec","use":"stack-allocated collections","when":"core (zero heap)"}
{"kind":"stack","crate":"thiserror","use":"mathematical domain errors","when":"core"}
{"kind":"stack","crate":"arc-swap","use":"lock-free global state pointers","when":"shell (reading rpds root)"}
{"kind":"stack","crate":"dashmap","use":"high-throughput concurrent state","when":"shell (actions aggregation)"}
{"kind":"stack","crate":"anyhow","use":"shell boundary error contexts","when":"shell / main"}
{"kind":"stack","crate":"tap","use":"linear pipe() control flow","when":"core + shell"}

{"kind":"bifurcation","id":"source_vs_test","text":"Source: clippy-mandatory, zero unwrap/mut/panic. Tests: implementation style irrelevant; test design and exact assertions mandatory.","source":{"clippy":"mandatory","quality":"flawless","unwrap":"banned","mut":"avoid"},"test":{"clippy":"ignore","style":"irrelevant","design":"mandatory","assertions":"exact","loops":"allowed","unwrap":"allowed","mut":"allowed"}}

{"kind":"lint","id":"file_header","scope":"source","lines":["#![deny(clippy::unwrap_used)]","#![deny(clippy::expect_used)]","#![deny(clippy::panic)]","#![warn(clippy::pedantic)]","#![warn(clippy::nursery)]","#![warn(clippy::complexity)]","#![warn(clippy::cognitive_complexity)]","#![forbid(unsafe_code)]"]}

{"kind":"ref","file":"references/scott-ddd-types.md","use":"Strict DDD+types doctrine"}
{"kind":"ref","file":"references/typing-refactor-checklist.md","use":"Stepwise primitive->type migration"}
{"kind":"ref","file":"references/complete-workflow.md","use":"Full Data->Calc->Actions example"}
{"kind":"ref","file":"references/rust-ai-verification-doctrine.md","use":"Rust+AI thesis: compiler/clippy/proofs as hallucination boundaries; concrete human reports over AI-sounding filler"}
```

### AI Developer Guardrails

You are acting as an AI Engineer under strict NASA/JPL Holzmann rules combined with absolute functional purity and extreme performance requirements.

1. **Test First, Always:** You MUST write tests before or alongside implementation to force reasoning about edge cases.
2. **Flatten Logic:** If you are generating code with `if` inside `if` inside `match`, STOP. Decompose the function. `< 60 lines`.
3. **No Imperative Loops:** You MUST NOT use `for` or `while`. Use Iterators, Rayon, or Streams.
4. **Zero-Copy / Zero-Heap First:** You MUST attempt to use `&'a str`, `Cow<'a, str>`, `bytes::Bytes`, and `SmallVec` before reaching for `String` or `Vec`.
5. **No `unwrap`:** You are physically incapable of typing `.unwrap()`, `.unwrap_or()`, or `.expect()`. Use `map_or_else` or `match`.
6. **No Silent Errors:** You MUST return `Result` and propagate up, or explicitly log.
7. **Warnings are Errors:** Clippy complexity and pedantic lints are mandatory. Zero warnings.
8. **Rust Makes AI Trust Cheaper:** Treat the compiler, clippy, newtypes, `thiserror`, Cargo metadata, and proof/fuzz/model-check lanes as hallucination boundaries. Read `references/rust-ai-verification-doctrine.md` when the work is AI-generated, AI-reviewed, or policy/documentation-heavy.

### Mandatory Verification Gate
You are strictly forbidden from considering a coding task "complete" until you have executed the following and received a `0` exit code.

**Preferred (use if moon is available):**
```bash
moon run :ci-source
```

**Fallback (run all three if moon is absent):**
```bash
cargo fmt --check
cargo clippy -- -D warnings -D clippy::unwrap_used -D clippy::panic -D clippy::expect_used -W clippy::pedantic
cargo nextest run 2>&1 | tdd-guard-rust --project-root . --passthrough
```
**Anti-Tampering Rule:** If a test fails, you MUST fix your implementation. You are forbidden from modifying the test to match your broken code unless the architectural contract explicitly changed. Never `#[ignore]` a test or comment it out to get green.

### Architecture

Your code MUST cleanly separate into:
1. **Data:** Zero-copy structs (`Cow`, `Bytes`, references), `SmallVec`, Enums.
2. **Calculations:** Pure functions, mathematically sound. Parallelized with `rayon` if dealing with collections.
3. **Actions:** The only place where `tokio`, `dashmap`, `arc-swap`, I/O, and side-effects live.

### Workflow & References

Artifact path resolution for bead workflows:
- Read contract/test artifacts from `.beads/<bead-id>/`.
- Write implementation artifacts to `.beads/<bead-id>/`.

In bead pipelines, prefer this contract flow:
- Read `contract.md` as the canonical implementation contract.
- Use detailed files (`contract-spec.md`, `martin-fowler-tests.md`, `traceability-matrix.md`) as supporting references.
- Write a normalized `implementation.md` with files changed + clause mapping.

Canonical resolver snippet:
```bash
BEAD_ID="<bead-id>"
PRIMARY_DIR=".beads/$BEAD_ID"
mkdir -p "$PRIMARY_DIR"
READ_ROOT="$PRIMARY_DIR"
```
