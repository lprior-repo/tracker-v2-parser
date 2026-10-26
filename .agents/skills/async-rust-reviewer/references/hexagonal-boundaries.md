# Hexagonal Architecture Boundaries

## The Layer Structure

Hexagonal architecture (Ports & Adapters) separates domain logic from infrastructure. Rust's trait system makes this natural.

| Layer | Rust Mechanism | Dependencies | May contain .await |
|-------|---------------|-------------|-------------------|
| Domain (core) | Structs, enums, pure fn | None — no external crates | NO |
| Application (use cases) | Trait-bounded generic functions | Domain only | NO |
| Infrastructure (adapters) | Trait impl for DB, HTTP, config | Domain + Application | YES |
| Presentation (edge) | Axum/Actix handlers | Application layer | YES |

## Ports (Traits)

Define traits where your domain logic lives. These are the "ports" — abstract interfaces that the domain uses without knowing how they're implemented.

```rust
// crates/domain/repo.rs — lives in domain crate, no infra deps
pub trait OrderRepo {
    async fn save(&self, order: &Order) -> Result<(), DomainError>;
    async fn find_by_id(&self, id: OrderId) -> Result<Option<Order>, DomainError>;
}
```

Note: The trait itself may use `async fn` — that's fine. What matters is that the domain crate doesn't depend on any specific async runtime. The trait is an abstract contract.

## Adapters (Implementations)

Implement traits in infrastructure crates that own the concrete technology.

```rust
// crates/infra/postgres/order_repo.rs — lives in infra crate
pub struct PostgresOrderRepo {
    pool: PgPool,
}

impl OrderRepo for PostgresOrderRepo {
    async fn save(&self, order: &Order) -> Result<(), DomainError> {
        sqlx::query("INSERT INTO orders ...")
            .bind(order.id())
            .execute(&self.pool)
            .await
            .map_err(|e| DomainError::Persistence(e.to_string()))?;
        Ok(())
    }
}
```

## Use Cases: Sync Core

Application logic (use cases) should be sync, delegating async I/O to the trait boundary.

```rust
// crates/app/use_cases.rs — sync, depends only on domain traits
pub fn create_order<R: OrderRepo>(repo: &R, cmd: CreateOrderCmd) -> Result<Order, DomainError> {
    let draft = Order::draft(cmd.customer_id, cmd.items);
    let validated = draft.validate()?;
    Ok(validated) // Sync — no .await, fully testable without runtime
}

// The async shell at the presentation layer:
// crates/api/handlers.rs
pub async fn handle_create(
    State(state): State<AppState>,
    Json(cmd): Json<CreateOrderCmd>,
) -> Result<Json<OrderResponse>, AppError> {
    let order = create_order(&state.repo, cmd)?;  // Sync domain call
    state.repo.save(&order).await?;                 // Async infra call
    Ok(Json(OrderResponse::from(order)))
}
```

## The Orphan Rule as Boundary Enforcement

Rust's orphan rule: you can only implement a trait for a type if you own the trait OR the type.

**This is a feature, not a limitation.** The compiler enforces your architecture:

| Situation | What it means |
|-----------|--------------|
| Implementing `domain::OrderRepo` for `infra::PostgresRepo` | Correct: you own the type (PostgresRepo) |
| Implementing `sqlx::FromRow` for `domain::Order` | Wrong: you don't own `FromRow` — create a wrapper type |
| Needing a wrapper type to satisfy orphan rule | Signal: crate boundaries may be wrong |
| Can't implement external trait on domain type | Good: domain shouldn't depend on infra frameworks |

**When wrapper types are justified**: For integration with external frameworks (sqlx, serde) at the boundary between infra and domain, thin wrapper types (DTOs) are acceptable. The wrapper lives in the infra crate, not the domain crate.

## Dependency Direction Enforcement

```
domain     ← no deps (pure Rust)
  ↑
app        ← depends on domain only
  ↑
infra      ← depends on domain + app + external crates (sqlx, reqwest, etc.)
  ↑
api        ← depends on app + infra + web framework (axum, actix)
```

**The rule**: Dependencies point inward. The domain crate knows nothing about databases, HTTP, or async runtimes.

## Crate Boundary Verification

```bash
# Verify domain has zero async deps
cargo metadata --format-version 1 --no-deps | \
  jq -r '.packages[] | select(.name == "domain") | .dependencies[].name' | \
  grep -E "tokio|futures|async-std|smol|async-trait|sqlx|reqwest" && \
  echo "FAIL: async/infra dependency in domain" || echo "OK"

# Verify domain source has no .await
grep -rn "\.await" --include="*.rs" crates/domain/ && \
  echo "FAIL: .await in domain" || echo "OK"

# Verify domain source has no spawn
grep -rn "tokio::spawn\|spawn_local\|spawn_blocking" --include="*.rs" crates/domain/ && \
  echo "FAIL: spawn in domain" || echo "OK"
```

## Testing the Architecture

Because domain logic is sync and has no infra dependencies:

```rust
// Domain tests: no runtime needed, pure unit tests
#[test]
fn validate_order_rejects_empty_items() {
    let draft = Order::draft(customer_id(), vec![]);
    assert!(matches!(draft.validate(), Err(DomainError::NoItems)));
}

// Infra tests: mock the port, test the adapter
#[tokio::test]
async fn postgres_repo_saves_and_finds_order() {
    let repo = PostgresOrderRepo::new(test_pool());
    let order = Order::validated(customer_id(), sample_items());
    repo.save(&order).await.unwrap();
    let found = repo.find_by_id(order.id()).await.unwrap();
    assert_eq!(found.unwrap().id(), order.id());
}

// Integration tests: wire everything together
#[tokio::test]
async fn create_order_handler_persists_to_db() {
    let repo = PostgresOrderRepo::new(test_pool());
    let state = AppState { repo: Arc::new(repo) };
    let response = handle_create(
        State(state),
        Json(CreateOrderCmd { customer_id: "c1".into(), items: vec!["item1".into()] }),
    ).await.unwrap();
    assert!(response.0.id().len() > 0);
}
```
