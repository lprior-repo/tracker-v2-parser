# Async Verification Gate

## The Layered Verification Model

Verification runs from fast to slow. Each layer must pass before the next runs. Any failure stops the gate with a non-zero exit code.

## Layer 1: Async Clippy Lints (Seconds)

```bash
cargo clippy -- -D warnings \
  -D clippy::unused_async \
  -D clippy::await_holding_lock \
  -D clippy::await_holding_refcell_ref \
  -D clippy::large_futures \
  -W clippy::pedantic
```

**What it catches**:
- `unused_async`: Functions declared async but never awaiting
- `await_holding_lock`: std::sync::Mutex held across .await
- `await_holding_refcell_ref`: RefCell borrowed across .await
- `large_futures`: Futures exceeding size threshold (stack pressure)

## Layer 2: Domain Crate Dependency Scan (Seconds)

```bash
cargo metadata --format-version 1 --no-deps | \
  jq -r '.packages[] | select(.name == "domain") | .dependencies[].name' | \
  grep -E "tokio|futures|async-std|smol|async-trait|sqlx|reqwest" && \
  echo "FAIL: async/infra dependency in domain crate" || echo "OK: domain is sync-only"
```

**What it catches**: Async runtime or infrastructure dependencies leaking into the domain crate. Domain must be pure.

## Layer 3: Source Boundary Scan (Seconds)

```bash
# Check for .await in domain source
grep -rn "\.await" --include="*.rs" crates/domain/ && \
  echo "FAIL: .await in domain source" || echo "OK: no .await in domain"

# Check for spawn in domain source
grep -rn "tokio::spawn\|spawn_local\|spawn_blocking" --include="*.rs" crates/domain/ && \
  echo "FAIL: spawn in domain source" || echo "OK: no spawn in domain"

# Check for println in async functions (should use tracing)
grep -rn "println!\|eprintln!" --include="*.rs" crates/api/ crates/infra/ && \
  echo "WARN: println in async code — use tracing instead" || echo "OK: no println in shell"
```

## Layer 4: Functional-Rust Sync Gate (Seconds)

Inherited from holzman-rust — must also pass:

```bash
cargo fmt --check
cargo clippy -- -D warnings \
  -D clippy::unwrap_used \
  -D clippy::panic \
  -D clippy::expect_used \
  -W clippy::pedantic \
  -W clippy::nursery \
  -W clippy::complexity
```

## Layer 5: Tests (Seconds-Minutes)

```bash
cargo nextest run 2>&1 | tdd-guard-rust --project-root . --passthrough
```

Or with moon:
```bash
moon run :test
```

## Layer 6: Benchmarks (Minutes)

```bash
# Check if benchmarks exist
if [ -f "benches/async_perf.rs" ] || [ -f "benches/"*.rs ]; then
    cargo bench 2>&1 | tee bench_results.txt

    # Check for regressions
    if grep -q "worsened" bench_results.txt; then
        echo "FAIL: Performance regression detected"
        exit 1
    fi
    echo "OK: Benchmarks pass"
else
    echo "CRITICAL: No benchmarks found for async hot paths"
    echo "Create benches/async_perf.rs with criterion benchmarks"
    exit 1
fi
```

**If no benchmarks exist**: FLAG AS CRITICAL. Every async hot path must have baseline benchmarks.

## Layer 7: tokio-console Check (Seconds)

```bash
# Verify console-subscriber is available for production builds
grep -r "console-subscriber" Cargo.toml crates/*/Cargo.toml 2>/dev/null && \
  echo "OK: tokio-console available" || \
  echo "WARN: tokio-console not configured — recommended for production services"
```

## Layer 8: Runtime Profiling (Minutes, Manual)

Not automated — requires running under load with tokio-console connected.

```bash
# 1. Build with console support
RUSTFLAGS="--cfg console" cargo build --features console

# 2. Run under load
./target/debug/my-app &
APP_PID=$!
hey -n 10000 -c 100 http://localhost:8080/api/orders

# 3. Connect console (separate terminal)
tokio-console

# 4. Check for:
# - Tasks with high busy time → CPU on async runtime (bad)
# - Tasks with high scheduled time → thread starvation (bad)
# - Growing task count → task leak (bad)

kill $APP_PID 2>/dev/null
```

## Full Gate Script

```bash
#!/bin/bash
# scripts/async-verify.sh — Complete async verification gate

set -euo pipefail

echo "=== Layer 1: Async clippy lints ==="
cargo clippy -- -D warnings \
  -D clippy::unused_async \
  -D clippy::await_holding_lock \
  -D clippy::await_holding_refcell_ref \
  -D clippy::large_futures \
  -W clippy::pedantic

echo "=== Layer 2: Domain dependency scan ==="
if cargo metadata --format-version 1 --no-deps | \
   jq -r '.packages[] | select(.name == "domain") | .dependencies[].name' | \
   grep -qE "tokio|futures|async-std"; then
    echo "FAIL: async dependency in domain crate"
    exit 1
fi
echo "OK: domain is sync-only"

echo "=== Layer 3: Source boundary scan ==="
if grep -rn "\.await\|tokio::spawn" --include="*.rs" crates/domain/ 2>/dev/null; then
    echo "FAIL: .await or spawn found in domain source"
    exit 1
fi
echo "OK: boundary clean"

echo "=== Layer 4: Functional-rust sync gate ==="
cargo fmt --check
cargo clippy -- -D warnings -D clippy::unwrap_used -D clippy::panic -D clippy::expect_used -W clippy::pedantic

echo "=== Layer 5: Tests ==="
cargo nextest run 2>&1 | tdd-guard-rust --project-root . --passthrough

echo "=== Layer 6: Benchmarks ==="
if ls benches/*.rs 2>/dev/null; then
    cargo bench 2>&1 | tee bench_results.txt
    if grep -q "worsened" bench_results.txt; then
        echo "FAIL: performance regression"
        exit 1
    fi
    echo "OK: benchmarks pass"
else
    echo "CRITICAL: no benchmarks found — create benches/async_perf.rs"
    exit 1
fi

echo "=== Layer 7: tokio-console check ==="
if grep -rq "console-subscriber" Cargo.toml crates/*/Cargo.toml 2>/dev/null; then
    echo "OK: tokio-console available"
else
    echo "WARN: tokio-console not configured (recommended for production)"
fi

echo ""
echo "=== ALL GATES PASSED ==="
```

## Concurrent Test Patterns

### Testing async code with tokio::test

```rust
#[tokio::test]
async fn create_order_persists() {
    let repo = PostgresOrderRepo::new(test_pool()).await;
    let order = create_order(&repo, sample_cmd()).unwrap();
    let found = repo.find_by_id(order.id()).await.unwrap();
    assert_eq!(found.unwrap().id(), order.id());
}
```

### Testing with time control

```rust
#[tokio::test(start_paused = true)]
async fn timeout_expires_after_5_seconds() {
    let start = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::time::sleep(Duration::from_secs(60)),
    ).await;
    assert!(result.is_err());
    assert_eq!(start.elapsed(), Duration::from_secs(5));
}
```

### Testing cancellation behavior

```rust
#[tokio::test]
async fn graceful_shutdown_drains_in_flight() {
    let (tx, rx) = tokio::sync::mpsc::channel::<u32>(10);
    let token = CancellationToken::new();

    let handle = tokio::spawn(worker(rx, token.clone()));

    // Send work
    tx.send(1).await.unwrap();
    tx.send(2).await.unwrap();

    // Cancel
    token.cancel();
    drop(tx); // Close channel

    let processed = handle.await.unwrap();
    assert!(processed >= 2, "should have processed in-flight items");
}
```

### Serial tests for shared resources

```rust
// When tests share a database or port
#[serial_test::serial]
#[tokio::test]
async fn concurrent_writes_to_shared_db() {
    // Only one test hits the DB at a time
}
```
