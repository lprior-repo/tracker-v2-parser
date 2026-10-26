# Spawn Discipline & Runtime Hygiene

## The Cooperative Scheduling Mandate

Tokio's multi-threaded runtime multiplexes tasks onto a small pool of OS threads (typically matching CPU core count). Tasks yield control at `.await` points. The runtime cannot preempt a task — if a task blocks, all other tasks on that thread starve.

**The threshold**: Any synchronous computation taking longer than 10-100 microseconds MUST be offloaded. This is the industry consensus from Tokio's core maintainers.

## Legal Spawn Locations

Spawning is an **Action** in the Data-Calc-Actions hierarchy. It belongs exclusively in:

| Location | Why it's legal |
|----------|---------------|
| HTTP handler entry points | Edge of the system — orchestrates domain + infra |
| gRPC service methods | Same — presentation layer |
| Infrastructure adapter initialization | Setting up background workers |
| `main()` / server setup | Top-level orchestration |
| Job queue dispatchers | Scheduler boundary — that's the point |

**Everywhere else is forbidden.** Domain logic, calculations, and pure functions must never spawn.

## spawn_blocking Guide

For sync I/O and short blocking operations:

```rust
// GOOD: Legacy sync database driver offloaded
let result = tokio::task::spawn_blocking(move || {
    sync_db.query("SELECT * FROM orders")
}).await.map_err(AppError::BlockingTask)?;
```

**Limitations of spawn_blocking**:
- Thread pool defaults to 512 threads — too many for sustained CPU work
- Tasks cannot be cancelled once started (run to completion)
- High context-switch overhead destroys cache locality

## The Rayon Pattern for CPU-Bound Work

For heavy computation, bridge Rayon (sync) with Tokio (async) via channels:

```rust
use tokio::sync::oneshot;

async fn process_data(payload: Bytes) -> Result<Processed, AppError> {
    let (tx, rx) = oneshot::channel();

    // Sync Rayon computation — doesn't block the async runtime
    rayon::spawn(move || {
        let result = payload.chunks(1024)
            .par_iter()  // Rayon: one thread per core
            .map(compute_chunk)
            .collect::<Vec<_>>();
        let _ = tx.send(result);
    });

    // Async side: yield while Rayon works
    let chunks = rx.await.map_err(|_| AppError::ComputationCancelled)?;
    Ok(aggregate(chunks))
}
```

**Why this matters**: Rayon uses exactly N threads (N = CPU cores). Tokio's blocking pool can use 512. Rayon maximizes cache locality and minimizes context switches.

## Workload Routing Table

| Workload Profile | Strategy | Mechanism | Rationale |
|-----------------|----------|-----------|-----------|
| High-volume Network I/O | async / Tokio | Event loop | Yields during waits, scales to millions of connections |
| Legacy Sync I/O | spawn_blocking | Thread pool (512) | Keeps sync code off reactor threads |
| Heavy CPU Computation | Rayon + oneshot | Fork-join (N cores) | Cache-local, true parallelism |
| Infinite Background Loops | std::thread::spawn | Dedicated OS thread | Prevents pool exhaustion |
| In-memory Processing | Sync iterators | Zero overhead | No async cost, full optimizer visibility |

## Anti-Patterns

### Fire-and-forget spawn
```rust
// BAD: No error handling, no bounds, no JoinHandle
tokio::spawn(async move {
    expensive_call(user_id).await;
});
```

### Spawn inside domain logic
```rust
// BAD: Domain crate should have zero async dependencies
pub fn process_order(order: Order) -> Result<(), DomainError> {
    tokio::spawn(async { save(order).await }); // WRONG LAYER
}
```

### Unbounded spawn loop
```rust
// BAD: Creates unlimited tasks with no backpressure
for item in items {
    tokio::spawn(process(item)); // Can create millions of tasks
}

// GOOD: Use for_each_concurrent with a bound
futures::stream::iter(items)
    .for_each_concurrent(32, |item| async move { process(item).await })
    .await;
```

## Structured Concurrency with JoinSet

```rust
use tokio::task::JoinSet;

async fn process_batch(items: Vec<Item>) -> Vec<Result<Output, AppError>> {
    let mut set = JoinSet::new();

    for item in items {
        set.spawn(process(item));
    }

    let mut results = Vec::with_capacity(items.len());
    while let Some(result) = set.join_next().await {
        results.push(result.map_err(AppError::from)?);
    }
    results
}
```

JoinSet automatically aborts remaining tasks when dropped — proper cleanup on cancellation.
