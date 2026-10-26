# Rust + AI verification doctrine

## Thesis

AI made code cheap. Rust makes it cheaper to trust.

When an agent writes Rust, the goal is not merely to produce plausible source.
The goal is to force generated code through a language and tooling stack that
turns hallucinations into concrete, repairable failures before human review or
production exposure.

Rust is the best default language for AI-assisted serious software because it
combines:

- a compiler that rejects ownership, borrowing, lifetime, trait, and
  exhaustiveness mistakes before runtime;
- explicit `Result`/`Option` error paths that agents can repair mechanically;
- enums, newtypes, typestates, and smart constructors for making illegal states
  unrepresentable;
- clippy, rustfmt, cargo metadata, cargo-deny, nextest, Miri, Loom, fuzzing, and
  property tests as local feedback surfaces;
- Kani, Flux, Verus, and related tools as a path from lint gates to real proof
  obligations;
- enough ecosystem breadth and AI training data to build useful software across
  CLIs, services, systems, embedded, UI, WASM, and infrastructure.

## Agent obligations

When writing or reviewing Rust with AI assistance:

1. Treat the compiler as the first reviewer, not an afterthought.
2. Treat clippy as machine-readable design feedback, not style trivia.
3. Use newtypes for domain IDs, paths, digests, names, quantities, and external
   identifiers.
4. Use `thiserror` or explicit domain error enums in core logic. Do not use
   `Result<T, String>` as an error model.
5. Prefer parse-don't-validate: external data enters through smart constructors,
   parsers, or boundary adapters; core data is already valid.
6. Make workflow states explicit with enums/typestates instead of boolean flags
   and `Option` lifecycle state.
7. Ban happy-path lies: no production `unwrap`, `expect`, `panic`, `todo`,
   `unimplemented`, unchecked indexing, unchecked casts, or ignored fallible
   results.
8. Let trusted tools do work: cargo, rustfmt, clippy, cargo-deny, nextest,
   cargo metadata, proptest, fuzzing, Miri, Loom, Kani, Flux, Verus.
9. Optimize after correctness and evidence. Never invent benchmark, lint, test,
   or proof output.
10. Final claims require actual command evidence or a clear blocker.

## Why this matters for AI

LLMs are good at plausible code. They are weak at knowing whether hidden
assumptions are true. Rust exposes those assumptions:

- an ownership assumption becomes a borrow-checker error;
- a missing state becomes a non-exhaustive match;
- a sloppy domain concept becomes a missing newtype;
- a happy-path shortcut becomes a clippy or policy finding;
- a concurrency assumption becomes a `Send`/`Sync`/Loom concern;
- a dependency issue becomes cargo-deny/cargo-vet evidence.

This is the shift-left thesis: AI supplies speed; Rust supplies friction in the
right places; the agent must use that friction rather than bypass it.

## Writing guidance for Rust AI docs and reports

When explaining Rust+AI work to humans, avoid AI-sounding filler. Use the
Humanizer lesson (https://github.com/blader/humanizer): concrete claims, direct
language, fewer vague superlatives, no fake magic, no chatbot praise, no generic
conclusions.

Good style:

- Name the failure caught.
- Name the command or tool that caught it.
- Name the type, invariant, rule ID, or proof lane involved.
- Explain residual risk honestly.

Bad style:

- "This robust solution ensures quality."
- "Rust unlocks seamless innovation."
- "The future looks bright."
- "Great question!"

Rust AI work earns trust through evidence, not vibes.
