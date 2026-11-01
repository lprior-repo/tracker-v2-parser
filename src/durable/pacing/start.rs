use std::time::Duration;

use anyhow::{Context, Result};
use tokio::{
    sync::{Mutex, MutexGuard},
    time::Instant,
};

use crate::model::Source;

#[derive(Default)]
pub(super) struct StartFences {
    athleticnet: Mutex<Option<Instant>>,
    milesplit: Mutex<Option<Instant>>,
}

impl StartFences {
    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn admit(
        &self,
        source: Source,
        interval: Duration,
    ) -> Result<MutexGuard<'_, Option<Instant>>> {
        let guard = match source {
            Source::AthleticNet => self.athleticnet.lock().await,
            Source::MileSplit => self.milesplit.lock().await,
        };
        let next = match *guard {
            Some(next) => next,
            None => Instant::now()
                .checked_add(interval)
                .context("initial client start fence overflow")?,
        };
        tokio::time::sleep_until(next).await;
        Ok(guard)
    }
}
