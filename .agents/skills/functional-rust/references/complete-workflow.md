# Complete Workflow Example (v5.0.0 - Zero Copy & Holzmann)

```rust
use std::borrow::Cow;
use rayon::prelude::*;
use thiserror::Error;
use smallvec::SmallVec;
use bytes::Bytes;

// DATA: Zero-copy newtypes, SmallVec, state machine enum
#[derive(Clone, Debug)] pub struct CustomerId<'a>(pub Cow<'a, str>);
#[derive(Clone, Debug)] pub struct Email<'a>(pub &'a str);

impl<'a> Email<'a> {
    pub fn parse(s: &'a str) -> Result<Self, EmailError> {
        s.contains('@').then(|| Self(s)).ok_or(EmailError)
    }
}

#[derive(Clone, Debug)] pub struct Cents(pub u64);
pub enum Order<'a> { Draft(Draft<'a>), Validated(Validated<'a>), Priced(Priced<'a>) }

// Using SmallVec to prevent heap allocations for typical order sizes
pub struct Draft<'a> { pub customer_id: CustomerId<'a>, pub items: SmallVec<[&'a str; 4]> }
pub struct Validated<'a> { pub customer_id: CustomerId<'a>, pub items: SmallVec<[&'a str; 4]> }
pub struct Priced<'a> { pub customer_id: CustomerId<'a>, pub total: Cents }

// CALCULATIONS: pure fn, Rayon, No Unwrap, Linear Flow, No Imperative Loops
#[derive(Debug, Error)] pub enum ValidationError { #[error("no items")] NoItems }
#[derive(Debug, Error)] pub enum EmailError {}

pub fn validate_order<'a>(draft: Draft<'a>) -> Result<Validated<'a>, ValidationError> {
    // Holzmann Guard: Linear flow, early exit, no swallowed errors
    if draft.items.is_empty() { return Err(ValidationError::NoItems); } 
    Ok(Validated { customer_id: draft.customer_id, items: draft.items })
}

pub fn price_order_parallel<'a>(validated: Validated<'a>) -> Priced<'a> {
    // Functional performance: Pure parallel fold. Naturally bounded by collection size.
    let total_cents = validated.items
        .par_iter()
        .map(|_item| 100) // Pure transformation
        .sum();           // Parallel reduction into CPU registers
        
    Priced { customer_id: validated.customer_id, total: Cents(total_cents) }
}

// ACTIONS: shell only, explicit I/O
pub async fn process_webhook(payload: Bytes) -> Result<(), anyhow::Error> {
    // Zero-copy parse boundary using bytes::Bytes and Cow
    // Normally you would deserialize payload safely here. 
    let mut items = SmallVec::new();
    items.push("item_a");
    
    let draft = Draft { customer_id: CustomerId(Cow::Borrowed("user_1")), items };
    
    // Railway-oriented state transition
    let validated = validate_order(draft)?;
    let priced = price_order_parallel(validated);
    
    // Explicit I/O side-effect
    println!("Saving order for {} total {}", priced.customer_id.0, priced.total.0);
    Ok(())
}
```
