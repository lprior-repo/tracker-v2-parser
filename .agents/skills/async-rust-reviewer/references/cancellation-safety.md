# Cancellation Safety & Pin Awareness

## How Cancellation Works in Rust

Cancellation in async Rust is not a cooperative signal or a boolean flag. It is achieved by simply dropping the Future. When dropped:
- Execution immediately and irreversibly halts
- RAII/Drop cleanup runs for any owned resources
- No further `.await` points are reached

This is elegant but dangerous: `tokio::select!` and `tokio::time::timeout` drop all "losing" futures automatically. If those futures were mid-operation, partial state may be lost.

## The Design Rule

**Every .await point must leave the system in a valid, recoverable state.**

If a Future is dropped at any `.await`, no data corruption, resource leak, or inconsistent state should result.

## Tokio Primitive Cancellation Safety

### Safe (can be used in select! without wrapping)

| Primitive | Why it's safe |
|-----------|--------------|
| `TcpListener::accept` | No state mutated before accepting |
| `UdpSocket::recv` | Data stays in kernel buffer if not read |
| `mpsc::Sender::send` | Message not consumed until send completes |
| `oneshot::Sender::send` | Value moved atomically |
| `fs::read` | File not modified |
| `io::AsyncReadExt::read` | Data remains in kernel buffer |

### Unsafe (MUST be wrapped or restructured)

| Primitive | Why it's unsafe | Fix |
|-----------|----------------|-----|
| `AsyncWriteExt::write` | Partial writes possible — data consumed but not fully sent | Use `write_all` or track bytes written |
| `AsyncBufReadExt::read_line` | Buffer contents lost on drop — data consumed from internal buffer | Read into owned buffer, track position |
| `AsyncSeekExt::seek` | Cursor position may be in inconsistent state | Seek in isolation, verify position |
| Any shared state mutation before .await | State partially updated, no rollback | Two-phase commit pattern |

## Two-Phase Commit Pattern

For critical state transitions across `.await`:

```rust
// Phase 1: Prepare (sync — no await, instantly recoverable if dropped)
let validated = validate_order(&draft)?;
let reservation = reserve_inventory(&validated)?;

// Phase 2: Commit (await — if dropped, reservation times out)
let confirmed = persist_order(reservation).await?;
```

If the Future is dropped between validate and persist, the reservation expires via TTL. No corrupted state.

## CancellationToken for Graceful Shutdown

```rust
use tokio_util::sync::CancellationToken;

async fn worker(token: CancellationToken) {
    let mut stream = incoming_requests();

    loop {
        tokio::select! {
            Some(req) = stream.next() => {
                process(req).await;
            }
            _ = token.cancelled() => {
                tracing::info!("draining in-flight work");
                break;
            }
        }
    }

    // Drain phase: finish in-flight work before exiting
    drain_pending_work().await;
}
```

## Pin Awareness

### What Pin solves

When `async fn` compiles to a state machine, local variables can reference each other across `.await` points. If the Future is moved in memory, those internal references become dangling pointers.

`Pin<P>` guarantees the pointed-to data will never move. The `poll` method takes `Pin<&mut Self>` to enforce this.

### When you need to think about Pin

| Situation | Action |
|-----------|--------|
| Writing `async fn` | Don't think about Pin — compiler handles it |
| Storing a Future in a struct | `Pin<Box<dyn Future>>` — the future is self-referential |
| Recursive async fn | `Box::pin` the recursive call — compiler can't size the state machine |
| Implementing custom Future/Stream | `poll` takes `Pin<&mut Self>` — understand what this means |
| Most standard types | `Unpin` auto-trait — safe to move even when pinned |

### Box::pin for recursive async

```rust
fn recursive_tree_walk(node: &Node) -> Pin<Box<dyn Future<Output = Result<()>>>> {
    Box::pin(async move {
        process(node).await?;
        for child in &node.children {
            recursive_tree_walk(child).await?; // Needs Box::pin for recursion
        }
        Ok(())
    })
}
```

### tokio::pin! for zero-alloc stack pinning

```rust
let future = some_async_operation();
tokio::pin!(future); // Pins to stack, no heap allocation

// Now future is Unpin-safe for select!, etc.
tokio::select! {
    result = &mut future => { /* ... */ }
    _ = tokio::time::sleep(Duration::from_secs(5)) => { /* ... */ }
}
```

## State Machine Valid at Every .await

The key insight: design your async state machines so that at every `.await` point, the state is valid and self-consistent. If the Future is dropped at that exact moment, nothing is corrupted.

**Good pattern**: Prepare sync, then single atomic await.
```rust
async fn transfer(from: &Account, to: &Account, amount: Cents) -> Result<(), Error> {
    // Sync: validate and prepare (instantly recoverable)
    validate_balance(from, amount)?;
    let tx = prepare_transaction(from, to, amount);

    // Single await: atomic commit
    commit_transaction(tx).await
}
```

**Bad pattern**: Multiple awaits with mutable shared state between them.
```rust
async fn transfer_bad(state: &mut SharedState, amount: Cents) -> Result<(), Error> {
    state.balance -= amount;        // Mutated
    let receipt = persist(state).await?;  // Dropped here = balance is wrong
    state.last_tx = receipt;        // Never reached
    Ok(())
}
```
