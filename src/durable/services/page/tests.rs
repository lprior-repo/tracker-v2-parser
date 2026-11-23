use anyhow::anyhow;

use super::finish;
use crate::durable::effects;

#[test]
fn given_retryable_page_failure_when_cleanup_also_fails_then_restate_keeps_retrying() {
    let page = Err::<(), _>(
        anyhow::Error::from(sqlx::Error::PoolTimedOut).context("retaining fetched page"),
    );
    let cleanup = Err(anyhow!("owned browser cleanup failed"));

    let failure = finish(page, cleanup).expect_err("both stages failed");
    let detail = format!("{failure:#}");
    assert!(detail.contains("retaining fetched page"));
    assert!(detail.contains("owned browser cleanup failed"));
    let classified = effects::classify(failure);
    assert!(
        format!("{classified:?}").starts_with("HandlerError(Retryable("),
        "cleanup erased the retryable page failure: {classified:?}"
    );
}

#[test]
fn given_invalid_page_when_cleanup_also_fails_then_restate_preserves_terminal_failure() {
    let page = Err::<(), _>(anyhow!("durable page request binding mismatch"));
    let cleanup = Err(anyhow!("owned browser cleanup failed"));

    let failure = finish(page, cleanup).expect_err("both stages failed");
    let detail = format!("{failure:#}");
    assert!(detail.contains("durable page request binding mismatch"));
    assert!(detail.contains("owned browser cleanup failed"));
    let classified = effects::classify(failure);
    assert!(
        format!("{classified:?}").starts_with("HandlerError(Terminal("),
        "invalid input became retryable: {classified:?}"
    );
}
