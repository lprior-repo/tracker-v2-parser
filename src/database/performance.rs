use std::{collections::BTreeMap, future::Future, time::Instant};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Stage {
    PrepareReplay,
    ContactReview,
    Schools,
    SchoolConflicts,
    Profiles,
    Results,
    Unassigned,
    Contacts,
    RevisionReview,
    Enqueue,
    Checkpoint,
    PoolAndBegin,
    ReservationLock,
    ReceiptLookup,
    Domain,
    ReceiptWrite,
    Commit,
}

pub(super) struct Timings {
    sampled: bool,
    boundary: &'static str,
    started: Instant,
    values: BTreeMap<Stage, (u128, bool)>,
}

impl Timings {
    pub(super) fn new(page_id: Option<i64>, boundary: &'static str) -> Self {
        Self {
            sampled: page_id.is_some_and(|id| id.rem_euclid(16) == 0),
            boundary,
            started: Instant::now(),
            values: BTreeMap::new(),
        }
    }

    pub(super) async fn measure<T, E>(
        &mut self,
        stage: Stage,
        action: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        let started = Instant::now();
        let result = action.await;
        if self.sampled {
            self.values
                .insert(stage, (started.elapsed().as_micros(), result.is_ok()));
        }
        result
    }
}

impl Drop for Timings {
    fn drop(&mut self) {
        if self.sampled {
            tracing::info!(
                target: "scraper_performance",
                boundary = self.boundary,
                elapsed_us = self.started.elapsed().as_micros(),
                stages = ?self.values,
                "sampled persistence timings"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Stage, Timings};

    #[tokio::test]
    async fn bounded_sampling_preserves_success_and_failure() {
        let mut sampled = Timings::new(Some(32), "test");
        let value = sampled
            .measure(Stage::Results, async { Ok::<_, &str>(7) })
            .await;
        assert_eq!(value, Ok(7));
        let error = sampled
            .measure(Stage::Commit, async { Err::<(), _>("refused") })
            .await;
        assert_eq!(error, Err("refused"));
        assert_eq!(sampled.values.len(), 2);
        assert!(sampled.values[&Stage::Results].1);
        assert!(!sampled.values[&Stage::Commit].1);
        let mut skipped = Timings::new(Some(33), "test");
        assert_eq!(
            skipped
                .measure(Stage::Results, async { Ok::<_, ()>(1) })
                .await,
            Ok(1)
        );
        assert!(skipped.values.is_empty());
        assert!(!Timings::new(None, "test").sampled);
    }
}
