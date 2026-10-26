# Benchmark Patterns for Async Rust

## The Principle: No Numbers = No Merge

Performance claims without `cargo bench` output are worthless. The compiler verifies types. Criterion verifies throughput. Both must pass.

## criterion Setup

```toml
# Cargo.toml
[dev-dependencies]
criterion = { version = "0.5", features = ["async_tokio"] }

[[bench]]
name = "async_perf"
harness = false
```

## Benchmark 1: Sync Core vs Async Shell

Verify that the async wrapper doesn't regress performance for CPU-bound domain logic.

```rust
// benches/async_perf.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use tokio::runtime::Runtime;

fn bench_sync_vs_async(c: &mut Criterion) {
    let items = generate_test_items(1000);

    let mut group = c.benchmark_group("compute_price");

    // Sync: pure domain function
    group.bench_function("sync_core", |b| {
        b.iter(|| compute_price(black_box(&items), black_box(0.1)))
    });

    // Async: shell wrapper
    let rt = Runtime::new().unwrap();
    group.bench_function("async_shell", |b| {
        b.to_async(&rt).iter(|| async {
            handle_order(
                OrderRequest { user_id: 1, discount: 0.1 },
                &MockRepo::new(&items),
            ).await
        })
    });

    group.finish();
}

criterion_group!(benches, bench_sync_vs_async);
criterion_main!(benches);
```

**The rule**: If `async_shell` is slower than `sync_core` for the same CPU-bound operation, the async is unjustified. The sync core should be used directly.

## Benchmark 2: Throughput Baselines

Every public async endpoint should have a throughput baseline.

```rust
fn bench_endpoint_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let repo = Arc::new(PostgresOrderRepo::new(test_pool()));

    let mut group = c.benchmark_group("create_order_throughput");
    group.sample_size(100);
    group.throughput(criterion::Throughput::Elements(1));

    group.bench_function("create_order", |b| {
        b.to_async(&rt).iter(|| {
            let repo = repo.clone();
            async move {
                create_order(
                    CreateOrderCmd {
                        customer_id: "customer-1".into(),
                        items: vec!["item-1".into()],
                    },
                    &*repo,
                ).await
            }
        })
    });

    group.finish();
}
```

**The rule**: Greater than 10% regression from baseline = FAIL. Commit baseline results alongside code changes.

## Benchmark 3: Concurrency Scaling

Verify that stream pipelines scale near-linearly with concurrency.

```rust
fn bench_concurrency_scaling(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let urls: Vec<String> = (0..100).map(|i| format!("http://localhost/test/{}", i)).collect();

    let mut group = c.benchmark_group("fetch_all_scaling");

    for concurrency in [1, 8, 64] {
        group.bench_with_input(
            BenchmarkId::new("buffer_unordered", concurrency),
            &concurrency,
            |b, &concurrency| {
                let urls = urls.clone();
                b.to_async(&rt).iter(|| {
                    let urls = urls.clone();
                    async move {
                        futures::stream::iter(urls)
                            .map(|url| async move { mock_fetch(&url).await })
                            .buffer_unordered(concurrency)
                            .collect::<Vec<_>>()
                            .await
                    }
                })
            },
        );
    }

    group.finish();
}
```

**The rule**: Throughput at N=8 should be roughly 8x N=1 (minus overhead). If N=64 is slower than N=8, you have lock contention or shared state bottlenecks.

## Benchmark 4: Channel Throughput

For message-passing architectures, benchmark the channel itself.

```rust
fn bench_mpsc_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("mpsc_channel");
    group.throughput(criterion::Throughput::Elements(10_000));

    group.bench_function("bounded_32", |b| {
        b.to_async(&rt).iter(|| async {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<u64>(32);
            tokio::spawn(async move {
                for i in 0..10_000 {
                    tx.send(i).await.unwrap();
                }
            });
            let mut count = 0u64;
            while let Some(_) = rx.recv().await {
                count += 1;
            }
            count
        })
    });

    group.finish();
}
```

## tokio-console Profiling Workflow

For runtime-level performance analysis (not benchmark, but diagnostic):

1. **Enable console-subscriber** in your app (see observability-reference.md)
2. **Run under load** with a realistic traffic generator (wrk, hey, vegeta)
3. **Connect tokio-console** and examine:

| What to look for | What it means | Action |
|------------------|--------------|--------|
| Task with high busy time | CPU work on async runtime | Move to rayon or spawn_blocking |
| All tasks with high scheduled time | Thread starvation | Find the blocking task, offload it |
| Task with very low idle time | Never yields | Check for sync loops, add yield points |
| Growing number of tasks | Task leak | Check for tasks never completing, missing abort |

4. **Export with tokio-metrics** to Prometheus for historical comparison

## Running the Benchmarks

```bash
# Run all async benchmarks
cargo bench --bench async_perf 2>&1 | tee bench_results.txt

# Compare against baseline (if criterion HTML reports are configured)
# Open target/criterion/report/index.html

# CI integration: fail on regression
cargo bench --bench async_perf 2>&1 | grep -E "change.*time.*\[[0-9]+\.[0-9]+%.*worsened" && \
  echo "FAIL: performance regression detected" || echo "OK: no regression"
```

## CI Benchmark Gate

```bash
#!/bin/bash
# scripts/bench-gate.sh — run in CI

set -euo pipefail

echo "=== Running async performance benchmarks ==="
cargo bench --bench async_perf 2>&1 | tee bench_results.txt

echo "=== Checking for regressions ==="
# Criterion outputs regression indicators in its report
if grep -q "Performance has regression" bench_results.txt; then
    echo "FAIL: Performance regression detected"
    echo "Review: target/criterion/report/index.html"
    exit 1
fi

echo "=== Benchmark gate passed ==="
```
