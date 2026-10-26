# Structural Correctness Overlay

This reference extracts the useful Asupersync ideas into runtime-agnostic async
Rust review rules. Do not treat Asupersync as the default dependency. Treat it as
a design benchmark: make correctness structural instead of relying on comments
and discipline.

## The Contract

Async code is structurally correct only when these properties are visible in the
code and test evidence:

1. Every task has an owner.
2. Cancellation is request -> stop intake -> drain -> finalize/rollback -> report.
3. Irreversible effects are two-phase, transactional, idempotent, or rollback-safe.
4. Time, I/O, randomness, persistence, and spawning are explicit capabilities.
5. Terminal outcomes distinguish success, domain error, cancellation, timeout,
   and panic/join failure.
6. Complex races have deterministic replay evidence.
7. Long-running async protocols yield or checkpoint fairly.
8. Shutdown emits a drain progress certificate.
9. Lock/channel/semaphore topologies expose wait-graph diagnostics where cycles
   are possible.
10. Resource obligations are released on success, error, cancellation, and panic.
11. Panics are isolated and classified by supervisor policy.
12. Admission is bounded for queues, pools, retries, fanout, and spawn loops.
13. Custom primitives have executable contracts and conformance tests.
14. Runtime migration claims include parity, performance, interoperability, and
   rollback evidence.

## Tokio Translation Table

| Asupersync idea | Tokio-compatible implementation |
|-----------------|----------------------------------|
| Region-owned tasks | `JoinSet`, `TaskTracker`, supervised actor, or service-owned task registry |
| `Cx` capability context | explicit ports/traits: `Clock`, `Spawner`, `Repo`, `Transport`, seeded RNG |
| cancel-correct protocol | `CancellationToken` tree + closed input + bounded drain + final report |
| two-phase effects | reservation token, DB transaction, outbox, idempotency key, rollback guard |
| deterministic lab runtime | `tokio::time::pause/advance`, `loom`, `shuttle`, `turmoil`, proptest seed logs |
| explicit `Outcome` | enum that separates `Ok`, domain `Err`, `Cancelled`, `Timeout`, `Panicked` |
| adaptive preemption/checkpoints | `yield_now`, work budgets, custom poll budgets, checkpointed protocol contexts |
| drain progress certificates | `DrainReport` with accepted/completed/cancelled/timed-out/aborted/panicked counts |
| spectral wait-graph warnings | lock-order table, wait-for graph logs, loom deadlock models, timeout diagnostics |
| resource cleanup verifier | RAII obligation guards, leak tests, cancellation storm cleanup checks |
| panic isolation | `JoinError::is_panic`, `Outcome::Panicked`, supervisor restart/degrade/escalate policy |
| replacement readiness | parity matrix, benchmark pack, adapter tests, rollback plan |

## Task Region Ownership

Bad: task has no owner and can outlive the request/service that created it.

```rust
tokio::spawn(async move {
    worker(req).await;
});
```

Good: a supervisor owns the task set and drains it on shutdown.

```rust
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

async fn run_service(token: CancellationToken) -> Result<(), Error> {
    let mut tasks = JoinSet::new();

    while let Some(req) = next_request().await? {
        let child_token = token.child_token();
        tasks.spawn(async move { worker(req, child_token).await });
    }

    token.cancel();

    while let Some(result) = tasks.join_next().await {
        classify_join_result(result)?;
    }

    Ok(())
}
```

The exact primitive is less important than the invariant: the owner can stop,
drain, classify, and report every child task.

## Cancel, Drain, Finalize

Reject shutdown paths that only call `cancel()` or `abort()` and return. A valid
shutdown path has these steps:

1. Request cancellation.
2. Stop accepting new work.
3. Drain in-flight work or hit a bounded timeout.
4. Finalize committed resources and rollback/release reserved resources.
5. Report counts for completed, cancelled, timed out, aborted, and panicked work.

`JoinHandle::abort` is an emergency brake, not the normal shutdown path.

## Two-Phase Effects

Bad: state is invalid if the future is dropped at the await.

```rust
async fn transfer_bad(accounts: &Accounts, amount: Money) -> Result<(), Error> {
    accounts.debit(amount)?;
    remote_credit(amount).await?;
    accounts.mark_complete()?;
    Ok(())
}
```

Good: prepare synchronously, await a commit point, release on cancellation.

```rust
async fn transfer(accounts: &Accounts, amount: Money) -> Result<Outcome<Receipt, Error>, Error> {
    let reservation = accounts.reserve_debit(amount)?;

    match remote_credit(reservation.idempotency_key()).await {
        Ok(receipt) => {
            accounts.commit_debit(reservation, &receipt)?;
            Ok(Outcome::Ok(receipt))
        }
        Err(error) if error.is_cancelled() => {
            accounts.release_debit(reservation)?;
            Ok(Outcome::Cancelled)
        }
        Err(error) => {
            accounts.release_debit(reservation)?;
            Ok(Outcome::Err(error))
        }
    }
}
```

## Capability-Gated Effects

Domain/application code should not directly grab clocks, randomness, files,
sockets, or spawners when deterministic tests matter. Pass the capability in.

```rust
trait Clock {
    fn now(&self) -> Instant;
}

trait Spawner {
    fn spawn_owned(&self, name: TaskName, task: OwnedTask) -> TaskId;
}
```

This makes replay possible and prevents hidden ambient effects from sneaking into
pure use cases.

## Outcome Lattice

Async supervision needs more than `Result<T, anyhow::Error>`.

```rust
enum Outcome<T, E> {
    Ok(T),
    Err(E),
    Cancelled,
    Timeout,
    Panicked,
}
```

Review every `JoinHandle`, timeout, `select!`, and cancellation path. If the code
cannot tell cancellation from timeout from panic, it cannot implement a correct
supervisor policy.

## Deterministic Replay Evidence

Stress tests are useful but not proof. For schedulers, channels, races, retry
logic, timeout paths, or custom protocols, require at least one deterministic
evidence source:

- `tokio::time::pause()` plus `advance()` for time-driven behavior.
- `loom` or `shuttle` for interleaving exploration.
- `turmoil` for network partitions and latency.
- `proptest` with logged seeds and minimal failing cases.
- Golden trace replay for previously found concurrency bugs.

If a bug can only be reproduced by "run it 1000 times and hope", the test suite
is not an async correctness suite.

## Fairness Checkpoints

Tokio is cooperative. A task that keeps doing work inside one poll starves other
tasks on the same worker. Long loops, custom `Future::poll`, retry processors,
stream fan-in, and scheduler code need a budget.

```rust
for (index, item) in batch.into_iter().enumerate() {
    process_one(item)?;

    if index % 128 == 0 {
        tokio::task::yield_now().await;
    }
}
```

For custom futures, the equivalent is a bounded amount of work per `poll`, then
`cx.waker().wake_by_ref()` and `Poll::Pending`.

## Drain Progress Certificate

Shutdown should produce evidence operators and tests can inspect.

```rust
struct DrainReport {
    accepted: usize,
    completed: usize,
    cancelled: usize,
    timed_out: usize,
    aborted: usize,
    panicked: usize,
    remaining: usize,
}
```

Reject shutdown code that returns `Ok(())` without showing what happened to
in-flight work.

## Wait-Graph Diagnostics

When code mixes locks, semaphores, bounded channels, barriers, and tasks waiting
on tasks, require a deadlock story:

- A documented lock acquisition order.
- A wait-for graph in debug/test builds.
- `loom` or `shuttle` tests for small interleavings.
- Timeout diagnostics that name the held resource and waiting task.

Never hold a mutex guard across external `.await` unless the type owns a written
invariant and tests prove no cycle.

## Resource Obligation Ledger

Every acquired resource creates an obligation. The obligation must be fulfilled
on every terminal path.

| Resource | Obligation |
|----------|------------|
| semaphore permit | release or commit ownership transfer |
| DB transaction | commit or rollback |
| reservation token | commit or release |
| temp file | persist or delete |
| spawned worker | drain, cancel, or abort with report |
| socket/subscription | close, unsubscribe, or hand off owner |

Use RAII guards for mechanical cleanup, but tests still need to assert cleanup
after cancellation and panic paths.

## Panic Isolation

Classify child task panic separately from domain failure.

```rust
match handle.await {
    Ok(Ok(value)) => Outcome::Ok(value),
    Ok(Err(error)) => Outcome::Err(error),
    Err(error) if error.is_cancelled() => Outcome::Cancelled,
    Err(error) if error.is_panic() => Outcome::Panicked,
    Err(error) => Outcome::Err(error.into()),
}
```

The supervisor must decide whether to restart, degrade, quarantine, or crash the
service. Do not flatten panic into generic error text.

## Backpressure And Admission Budgets

Every path that admits work must name its budget and overload behavior:

- Channel capacity.
- Semaphore permits.
- Queue length.
- Retry budget and backoff.
- Spawn concurrency limit.
- Request body and stream item limit.

`mpsc::unbounded_channel` in a service path is a denial-of-service bug unless the
producer is statically bounded and documented.

## Spec, Conformance, And Parity

Custom async primitives need executable contracts before optimization:

- Given/When/Then scenarios for lifecycle behavior.
- Property tests for state-machine invariants.
- Differential tests against Tokio/futures equivalents when they exist.
- Fuzz/model tests for interleavings and malformed input.
- Criterion benchmarks only after correctness contracts exist.

Runtime replacement claims need a stricter pack:

- Tokio-vs-target API parity matrix.
- p50/p99/p999 latency, throughput, memory, startup, and CPU deltas.
- Ecosystem compatibility list for crates that require Tokio.
- Adapter/bridge overhead measurements.
- Failure-mode comparison: cancellation, panic, timeout, backpressure, overload.
- Rollback plan.

## Review Questions

Ask these on every nontrivial async review:

1. Who owns every task?
2. What exact code drains every task?
3. What state is valid if this future is dropped at each `.await`?
4. Which effects are ambient, and can tests replace them?
5. Can cancellation, timeout, panic, and domain error be distinguished?
6. What deterministic replay evidence exists for the hardest race?
7. What benchmark proves the structural correctness machinery did not wreck the hot path?
8. What fairness budget prevents one task from monopolizing a worker?
9. What drain report proves shutdown actually completed?
10. What wait graph or lock-order proof prevents deadlocks?
11. What obligation ledger proves resources are released after cancellation and panic?
12. What parity pack justifies any runtime migration claim?
