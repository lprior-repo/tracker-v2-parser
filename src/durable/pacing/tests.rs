use std::time::Duration;

use anyhow::Result;

use super::{Spacing, WireScope, admitted_token};
use crate::{durable::effects, model::Source};

mod locking;

#[test]
fn contention_budget_exhaustion_keeps_a_typed_retryable_cause_and_preserves_reserved_tokens()
-> Result<()> {
    assert_eq!(
        admitted_token(Some("reserved-token".into()))?,
        "reserved-token"
    );
    let failure = admitted_token(None).expect_err("unreserved admission must fail");
    assert!(failure.is::<crate::durable::AcquisitionBusy>());
    assert_eq!(
        failure.to_string(),
        "persistent wire admission exceeded bounded contention budget"
    );
    let classified = effects::classify(failure.context("journaled source download"));
    assert!(format!("{classified:?}").starts_with("HandlerError(Retryable("));
    Ok(())
}

#[test]
fn invalid_configured_pacing_capacity_remains_terminal() {
    let failure = WireScope::parse(
        Source::MileSplit,
        "https://wi.milesplit.com",
        Duration::ZERO,
        Spacing::CompletedHeaders,
    )
    .err()
    .expect("zero pacing interval must fail before admission");
    assert_eq!(
        failure.to_string(),
        "persistent pacing interval must be positive"
    );
    let classified = effects::classify(failure.context("journaled source download"));
    assert!(format!("{classified:?}").starts_with("HandlerError(Terminal("));
}
