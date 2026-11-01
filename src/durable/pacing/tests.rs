use std::time::Duration;

use anyhow::Result;

use super::{Spacing, WireScope, admission_delay, admitted_token};
use crate::{durable::effects, model::Source};

mod locking;

#[test]
fn adaptive_lease_polling_is_bounded_and_never_sleeps_past_known_admission_time() {
    let remaining = Duration::from_secs(45);
    for (poll, millis) in [
        (0, 25),
        (7, 25),
        (8, 50),
        (15, 50),
        (16, 100),
        (usize::MAX, 100),
    ] {
        assert_eq!(
            admission_delay(remaining, true, poll),
            Duration::from_millis(millis)
        );
        assert_eq!(
            admission_delay(Duration::from_micros(3), true, poll),
            Duration::from_micros(3)
        );
    }
    assert_eq!(
        admission_delay(remaining, false, usize::MAX),
        Duration::from_millis(500)
    );
    assert_eq!(
        admission_delay(Duration::from_millis(10), false, 0),
        Duration::from_millis(10)
    );
}

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
