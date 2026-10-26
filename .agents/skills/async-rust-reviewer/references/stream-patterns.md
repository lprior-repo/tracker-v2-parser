# Stream Patterns & Concurrency Decision Matrix

## Streams: The Async Iterator

A Stream is to async Rust what Iterator is to sync Rust. It yields values asynchronously over time — perfect for network packets, log entries, WebSocket events, database rows.

**The rule**: Never use `while let Some(item) = stream.next().await`. Use combinator pipelines instead.

## Concurrency Primitive Decision Matrix

| Primitive | When to Use | Cancellation | Error Propagation | Bounded By Default |
|-----------|------------|-------------|-------------------|--------------------|
| `tokio::join!` | All futures must complete. Every result matters. | No cancellation — waits for all | Each branch returns its own Result | N/A (fixed count) |
| `tokio::select!` | First result wins. Timeouts, races, cancellation. | Drops all losing futures | Returns winning branch's Result | N/A (fixed count) |
| `for_each_concurrent(N, ...)` | Bounded fan-out with side effects. DB writes, network sends. | Cancels remaining on error | Short-circuits on first error | Yes (N parameter) |
| `buffer_unordered(N)` | Bounded fan-out collecting results. Aggregation, transformation. | Cancels remaining on drop | Results yielded as they complete | Yes (N parameter) |
| `join_all` | Small, statically-bounded collections ONLY. | No — waits for all | Collects all Results | NO — dangerous on dynamic collections |
| `FuturesUnordered` | Need to inspect individual futures as they complete | Drops all on drop | Manual per-future error handling | No (add futures dynamically) |

## Combinator Pipeline Examples

### Replace imperative stream loop with combinators

```rust
// BAD: Sequential, no concurrency
async fn process_events(stream: impl Stream<Item = Event>) {
    while let Some(event) = stream.next().await {
        if event.is_relevant() {
            let enriched = enrich(event);
            handle(enriched).await;
        }
    }
}

// GOOD: Concurrent, bounded, functional pipeline
async fn process_events(stream: impl Stream<Item = Event>) {
    stream
        .filter(|e| future::ready(e.is_relevant()))
        .map(enrich)
        .for_each_concurrent(32, |e| async move { handle(e).await })
        .await;
}
```

### buffer_unordered for aggregation

```rust
// When you need the results back
async fn fetch_all(urls: Vec<String>) -> Vec<Response> {
    futures::stream::iter(urls)
        .map(|url| async { reqwest::get(&url).await })
        .buffer_unordered(16)  // 16 concurrent requests, results in completion order
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .filter_map(|r| r.ok())
        .collect()
}
```

### Side effects with for_each_concurrent

```rust
// When results don't matter — DB writes, metrics, network sends
async fn save_all(items: Vec<Item>, repo: &dyn Repo) {
    futures::stream::iter(items)
        .for_each_concurrent(8, |item| async move {
            let _ = repo.save(item).await; // Error handled inside
        })
        .await;
}
```

### Batching with ready_chunks

```rust
// Batch items for bulk DB inserts
async fn bulk_insert(stream: impl Stream<Item = Row>, pool: &PgPool) {
    tokio_stream::wrappers::StreamExt::ready_chunks(stream, 64)
        .for_each_concurrent(4, |chunk| async move {
            db_bulk_insert(&chunk, pool).await
        })
        .await;
}
```

### select! for timeout patterns

```rust
tokio::select! {
    result = operation() => {
        tracing::info!(?result, "completed before timeout");
    }
    _ = tokio::time::sleep(Duration::from_secs(5)) => {
        tracing::warn!("operation timed out");
    }
}
```

**Warning**: `select!` drops the losing future. If `operation()` has side effects, they're cancelled. Use with care.

## FuturesUnordered vs JoinSet

| Feature | FuturesUnordered | JoinSet |
|---------|-----------------|---------|
| Add futures dynamically | Yes | Yes |
| Abort individual tasks | No | Yes (`abort_one`) |
| Detach tasks | No | Yes (`detach`) |
| Aborts remaining on drop | No | Yes |
| Yield results as they complete | Yes | Yes |
| Collection type | Stream of outputs | `join_next()` iterator |

**Recommendation**: Prefer `JoinSet` in most cases. It provides better cleanup guarantees (aborts on drop) and individual task control. Use `FuturesUnordered` only when you need stream-style iteration.

## Anti-Patterns

### join_all on unbounded collection
```rust
// DANGEROUS: If `futures` has 100k items, this creates 100k concurrent tasks
let results: Vec<_> = futures::future::join_all(futures).await;
```

### select! loop without Fuse
```rust
// BUG: After a stream ends, select! will spin on the exhausted stream
loop {
    tokio::select! {
        item = stream.next() => { /* ... */ }
        msg = rx.recv() => { /* ... */ }
    }
}

// FIX: Use Fuse for exhausted streams
let mut stream = Box::pin(stream.fuse());
loop {
    tokio::select! {
        item = stream.next() => {
            match item {
                Some(item) => { /* process */ }
                None => break, // Stream exhausted, exit loop
            }
        }
        msg = rx.recv() => { /* ... */ }
    }
}
```

### Unbuffered stream processing
```rust
// BAD: Processes one item at a time — wastes I/O concurrency
stream.for_each(|item| async move { fetch(item).await }).await;

// GOOD: Bounded concurrency
stream.for_each_concurrent(32, |item| async move { fetch(item).await }).await;
```
