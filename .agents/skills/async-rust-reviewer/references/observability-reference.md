# Observability Reference

## Why Standard Logging Fails in Async

A single logical operation in async Rust spans multiple tasks, threads, and `.await` points. Traditional logging (log crate, println) loses the causal chain — you can't tell which log line belongs to which request when 10,000 are in flight.

The `tracing` crate solves this with structured, hierarchical spans that propagate context across async boundaries.

## #[tracing::instrument]

The `#[instrument]` macro creates a span for every function call, recording entry, exit, and any fields you specify.

```rust
use tracing::instrument;

#[instrument(skip(repo), fields(user_id = %req.user_id))]
async fn handle_order(req: OrderRequest, repo: &dyn OrderRepo) -> Result<Response, AppError> {
    let items = repo.fetch_items(req.user_id).await?;
    let price = compute_price(&items, req.discount); // Sync domain call
    tracing::info!(total_price = %price, "order computed");
    Ok(Response::new(price))
}
```

**Rules**:
- Every async function in the shell (presentation + infra layers) gets `#[instrument]`
- Use `skip()` for non-Debug parameters (repos, large payloads)
- Add structured fields for searchable values (user_id, order_id, request_id)
- Domain (sync) functions don't need instrument — they're pure and don't cross boundaries

## Span Propagation into Spawned Tasks

Spawned tasks start fresh — they don't inherit the parent's span context. You must explicitly propagate:

```rust
use tracing::{instrument, info_span, Instrument};

#[instrument(fields(order_id = %id))]
async fn process_order(id: OrderId) -> Result<(), AppError> {
    let data = fetch_data(id).await?;

    // WRONG: spawned task loses the order_id span
    tokio::spawn(async move { enrich_data(data).await });

    // CORRECT: span propagates into spawned task
    tokio::spawn(
        async move { enrich_data(data).await }
            .instrument(info_span!("enrichment", order_id = %id))
    );

    Ok(())
}
```

**The rule**: Every `tokio::spawn` must either:
1. Use `#[instrument]` on the spawned function, or
2. Chain `.instrument(span)` on the spawned future

Lost trace correlation = you cannot debug production issues.

## tracing-opentelemetry + OTLP Setup

Configure from day one. Do not retrofit after the first incident.

```toml
# Cargo.toml
[dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
opentelemetry = "0.29"
opentelemetry_sdk = { version = "0.29", features = ["rt-tokio"] }
tracing-opentelemetry = "0.30"
```

```rust
// main.rs or lib.rs initialization
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

fn init_telemetry() -> Result<(), Box<dyn std::error::Error>> {
    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint("http://localhost:4317");

    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(exporter)
        .install_batch(opentelemetry_sdk::runtime::Tokio)?;

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let fmt_layer = tracing_subscriber::fmt::layer().json();
    let filter = EnvFilter::from_default_env()
        .add_directive("info".parse()?);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    Ok(())
}
```

## tokio-console Setup

tokio-console provides live runtime introspection — you can see every task's busy/idle/scheduled time in real time.

```toml
# Cargo.toml (production dependency, behind feature flag)
[dependencies]
console-subscriber = "0.4"
tokio = { version = "1", features = ["full", "tracing"] }
```

```rust
// Enable in main before building runtime
#[cfg(feature = "console")]
console_subscriber::init();
```

```bash
# Terminal 1: Run your app with console enabled
RUSTFLAGS="--cfg console" cargo run --features console

# Terminal 2: Connect tokio-console
tokio-console
```

### Reading tokio-console Metrics

| Metric | What it means | Healthy range | Red flag |
|--------|--------------|---------------|----------|
| **Busy** | Time actively executing poll() | Low for I/O tasks | High busy + low throughput = CPU work on async runtime → offload to rayon |
| **Idle** | Time suspended waiting for I/O | High for I/O tasks | Very low idle = task never yields → blocking the runtime |
| **Scheduled** | Time waiting in runtime queue after wake | Near zero | High scheduled = thread starvation, too many tasks, or a blocked reactor thread |

**The "Scheduled" metric is the most important health indicator.** If scheduled times spike, a task is blocking the runtime. Find it via the "busy" metric and offload it.

### tokio-metrics for Historical Analysis

```toml
[dependencies]
tokio-metrics = "0.3"
```

```rust
use tokio_metrics::RuntimeMonitor;

let monitor = RuntimeMonitor::new(&runtime.handle());
let interval = monitor.intervals();

// Export to Prometheus, Grafana, CloudWatch
for metrics in interval {
    println!("budget_forced_yield: {}", metrics.budget_forced_yield_count);
    println!("num_workers: {}", metrics.num_workers);
    println!("queue_depth: {}", metrics.worker_total_queue_depth);
}
```

## Error Chain Preservation

```rust
use anyhow::Context;

// GOOD: Error context preserved at every boundary
async fn create_order(cmd: CreateOrderCmd, pool: &PgPool) -> Result<Order, anyhow::Error> {
    let draft = Order::draft(cmd.customer_id, cmd.items)
        .context("failed to create draft order")?;

    pool.acquire()
        .await
        .context("failed to acquire db connection")?
        .execute("INSERT INTO orders ...")
        .await
        .context("failed to persist order")?;

    Ok(draft)
}

// BAD: Context lost
async fn create_order_bad(cmd: CreateOrderCmd, pool: &PgPool) -> Result<Order, anyhow::Error> {
    let draft = Order::draft(cmd.customer_id, cmd.items)?;
    pool.acquire().await?.execute("INSERT INTO orders ...").await?;
    Ok(draft)
    // Error says "pool timed out" instead of "failed to acquire db connection while creating order for user X"
}
```

## Structured Logging Format

Always use structured fields, not string interpolation:

```rust
// BAD: Can't search, can't aggregate
tracing::info!("Processed order {} for user {}", order_id, user_id);

// GOOD: Searchable, aggregatable, parseable
tracing::info!(
    order_id = %order_id,
    user_id = %user_id,
    total = %price,
    item_count = items.len(),
    "order processed"
);
```
