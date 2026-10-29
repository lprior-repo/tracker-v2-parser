mod store;
mod wire;

pub use wire::WireRequest;

#[cfg(test)]
pub(crate) mod test_support;

use std::{future::Future, time::Duration};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use sqlx::PgPool;
use url::Url;

use crate::model::Source;

const WIRE_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_ADMISSION_POLLS: usize = 512;

#[derive(Clone)]
pub struct PersistentPacing {
    pool: PgPool,
    wire_context: Option<wire::WireContext>,
}

struct WireScope {
    keys: [String; 2],
    interval_micros: i64,
    reservation_micros: i64,
}

enum Admission {
    Wait(Duration),
    Reserved(String),
}

impl PersistentPacing {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            wire_context: None,
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn execute<T, F>(
        &self,
        source: Source,
        address: &str,
        interval: Duration,
        operation: F,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        self.execute_guarded(source, address, interval, operation, |_| None)
            .await
    }

    async fn execute_guarded<T, F, S>(
        &self,
        source: Source,
        address: &str,
        interval: Duration,
        operation: F,
        status: S,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>>,
        S: Fn(&T) -> Option<u16>,
    {
        let scope = WireScope::parse(source, address, interval)?;
        let token = self.admit(&scope).await?;
        let result = tokio::time::timeout(WIRE_TIMEOUT, operation)
            .await
            .context("persistent paced wire request timed out")
            .and_then(std::convert::identity);
        let completed = self.complete(&scope, &token).await;
        match (result, completed) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(value), Err(error)) => Err(wire::preserve_status(status(&value), error, address)),
            (Err(error), Err(cleanup)) => {
                Err(error.context(format!("wire pacing completion failed: {cleanup:#}")))
            }
        }
    }

    async fn admit(&self, scope: &WireScope) -> Result<String> {
        let result = stream::iter(0..MAX_ADMISSION_POLLS)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(None, |token, _| async move {
                if token.is_some() {
                    return Ok(token);
                }
                match self.reserve(scope).await? {
                    Admission::Reserved(token) => Ok(Some(token)),
                    Admission::Wait(delay) => {
                        tokio::time::sleep(delay).await;
                        Ok(None)
                    }
                }
            })
            .await?;
        result.context("persistent wire admission exceeded bounded contention budget")
    }
}

impl WireScope {
    fn parse(source: Source, address: &str, interval: Duration) -> Result<Self> {
        ensure!(
            !interval.is_zero(),
            "persistent pacing interval must be positive"
        );
        let url = Url::parse(address).context("parsing persistent pacing origin")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none(),
            "persistent pacing requires an HTTP origin without credentials"
        );
        let interval_micros = i64::try_from(interval.as_micros())
            .context("persistent pacing interval exceeds database range")?;
        ensure!(
            interval_micros > 0,
            "persistent pacing interval is below one microsecond"
        );
        let reservation = interval
            .checked_add(WIRE_TIMEOUT)
            .context("persistent wire reservation overflow")?;
        let reservation_micros = i64::try_from(reservation.as_micros())
            .context("persistent wire reservation exceeds database range")?;
        let keys = [
            format!("provider:{}", source.as_str()),
            format!("origin:{}", url.origin().ascii_serialization()),
        ];
        ensure!(
            keys.iter().all(|key| key.len() <= 2048),
            "persistent pacing origin is too long"
        );
        Ok(Self {
            keys,
            interval_micros,
            reservation_micros,
        })
    }
}
