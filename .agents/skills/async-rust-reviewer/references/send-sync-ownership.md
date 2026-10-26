# Send + Sync & Ownership Design

## The Priority Ladder

When multiple async tasks need access to the same data, use this priority order. Each option is strictly better than the one below it.

### 1. Ownership Transfer (Best)

Move data by value into the spawned task. No sharing needed.

```rust
let order = parse_order(payload)?; // Owned value
tokio::spawn(async move {
    process(order).await // order moved in, no sharing
});
```

No locks, no channels, no Arc. The task owns the data exclusively.

### 2. Message Passing

Channels eliminate shared mutable state entirely. "Share memory by communicating."

```rust
// Fan-out: mpsc for multiple producers, single consumer
let (tx, mut rx) = tokio::sync::mpsc::channel::<Command>(32);

tokio::spawn(async move {
    while let Some(cmd) = rx.recv().await {
        handle_command(cmd); // Single owner, no contention
    }
});

// Request-reply: oneshot for single response
let (tx, rx) = tokio::sync::oneshot::channel();
worker.send(Work { response: tx });
let result = rx.await?;
```

**Channel selection guide:**

| Channel | Use case | Bound |
|---------|----------|-------|
| `mpsc` | Fan-out commands to a single consumer | Yes (capacity parameter) |
| `oneshot` | Request-reply, single-value responses | N/A (single value) |
| `broadcast` | Pub-sub, all consumers get every message | Yes (capacity) |
| `watch` | Latest-value-only, consumers read current state | Single value |

### 3. Atomic Operations

For counters, flags, and small shared values — no locking at all.

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

let counter = Arc::new(AtomicU64::new(0));

// Lock-free increment from any task
counter.fetch_add(1, Ordering::Relaxed);

// Lock-free read
let current = counter.load(Ordering::Relaxed);
```

**arc-swap** (from holzman-rust Core 10) for lock-free reads of larger values:

```rust
use arc_swap::ArcSwap;

let config = ArcSwap::from_pointee(Config::default());

// Writers: atomic swap, no lock
config.store(Arc::new(new_config));

// Readers: lock-free, wait-free
let current = config.load();
```

### 4. dashmap for Concurrent Maps

When you need a concurrent HashMap, use dashmap instead of `Arc<Mutex<HashMap>>`:

```rust
use dashmap::DashMap;

let map: DashMap<String, Order> = DashMap::new();

// Fine-grained locking — only the bucket containing the key is locked
map.insert("order-1".to_string(), order);
let order = map.get(&"order-1".to_string());

// Sharded internally — multiple writers don't contend unless same bucket
```

dashmap shards the map internally, so concurrent operations on different keys don't contend.

### 5. Arc<tokio::sync::Mutex<T>> (Last Resort)

Only when genuinely shared mutable state is unavoidable AND cannot be modeled as:
- An actor with message passing
- Atomic operations
- A concurrent map

```rust
// If you MUST use Arc<Mutex>, encapsulate it
pub struct OrderCache {
    inner: Arc<tokio::sync::Mutex<HashMap<OrderId, Order>>>,
}

impl OrderCache {
    pub async fn get(&self, id: OrderId) -> Option<Order> {
        let guard = self.inner.lock().await;
        guard.get(&id).cloned()
    }
}
```

Callers interact with `OrderCache`, not `Arc<Mutex<...>>`. The lock is an implementation detail.

## The Actor Model for Non-Send State

Some types are `!Send` (Rc, RefCell, raw pointers, C types). These cannot cross `.await` boundaries in multi-threaded Tokio. The solution: isolate them in a single-threaded actor.

```rust
// Non-Send state
struct NativeRenderer {
    context: *mut ffi::GraphicsContext, // !Send
}

// Actor: single task, exclusive ownership
async fn renderer_actor(mut rx: mpsc::Receiver<RenderCommand>) {
    let mut renderer = NativeRenderer::new(); // Lives entirely in this task

    while let Some(cmd) = rx.recv().await {
        renderer.execute(cmd); // No Send bound needed
    }
}

// Spawning the actor
let (tx, rx) = mpsc::channel::<RenderCommand>(32);
tokio::spawn(renderer_actor(rx));

// Callers send commands via channel
tx.send(RenderCommand::Draw(shape)).await?;
```

The actor owns `!Send` state exclusively. All interaction happens through typed message channels. The borrow checker is satisfied, the runtime is happy, and there's zero lock contention.

## Arc vs Rc Decision

| Context | Use | Why |
|---------|-----|-----|
| Sync code, single-threaded | `Rc<T>` | Cheaper — no atomic reference counting |
| Async code, crossing .await | `Arc<T>` | Must be Send for multi-threaded runtime |
| Shared config, read-heavy | `Arc<T>` + `arc-swap` | Lock-free reads |
| Shared map, concurrent writes | `DashMap` | Sharded, no global lock |
| Shared counter | `AtomicU64` | No lock at all |

## std::sync::Mutex vs tokio::sync::Mutex

| Mutex | Use When | Cost |
|-------|---------|------|
| `std::sync::Mutex` | Lock held during sync computation only, never across .await | Cheaper (no allocation) |
| `tokio::sync::Mutex` | Lock MUST be held across .await | More expensive (requires allocation) |

**The rule**: If the lock guard crosses an `.await`, you MUST use `tokio::sync::Mutex`. Otherwise, `std::sync::Mutex` is fine and preferred.

```rust
// BAD: std::sync::Mutex held across .await — can deadlock on multi-threaded runtime
let guard = std_mutex.lock().unwrap();
some_async_work().await; // Guard still held — if task moves to another thread, UB

// GOOD: Drop the lock before awaiting
{
    let guard = std_mutex.lock().unwrap();
    let data = guard.clone(); // Copy what you need
} // Lock dropped
some_async_work(data).await; // Safe
```
