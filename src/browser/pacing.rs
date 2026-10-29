use std::{sync::Mutex, time::Duration};

use anyhow::{Context, Result, anyhow};
use futures::{StreamExt, TryStreamExt, future, stream};
use tokio::{sync::Mutex as AsyncMutex, time::Instant};

use crate::transport::AccessStop;

pub(super) struct Pacing {
    interval: Duration,
    state: AsyncMutex<State>,
    stopped: Mutex<Option<AccessStop>>,
}

struct State {
    next: Instant,
    generation: u64,
    active: Option<u64>,
}

pub(super) enum Lease {
    Unpaced,
    Reserved { generation: u64 },
}

enum Admission {
    Reserved(Lease),
    Wait(Duration),
}

#[cfg(test)]
mod tests;

impl Pacing {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            state: AsyncMutex::new(State {
                next: Instant::now(),
                generation: 0,
                active: None,
            }),
            stopped: Mutex::new(None),
        }
    }

    pub async fn admit(&self) -> Result<Lease> {
        self.check_stop()?;
        if self.interval.is_zero() {
            return Ok(Lease::Unpaced);
        }
        let admitted = stream::iter(0..16_384)
            .then(|_| self.try_admit())
            .try_filter_map(|lease| future::ready(Ok(lease)));
        futures::pin_mut!(admitted);
        admitted
            .try_next()
            .await?
            .context("browser wire admission exceeded bounded contention budget")
    }

    async fn try_admit(&self) -> Result<Option<Lease>> {
        match self.reserve().await? {
            Admission::Reserved(lease) => Ok(Some(lease)),
            Admission::Wait(delay) => {
                tokio::time::sleep(delay).await;
                Ok(None)
            }
        }
    }

    async fn reserve(&self) -> Result<Admission> {
        let mut state = self.state.lock().await;
        self.check_stop()?;
        let now = Instant::now();
        if state.next > now {
            return Ok(Admission::Wait(
                state
                    .next
                    .duration_since(now)
                    .min(Duration::from_millis(25)),
            ));
        }
        let generation = state
            .generation
            .checked_add(1)
            .context("browser wire lease generation overflow")?;
        let reservation = super::COMMAND_TIMEOUT
            .checked_add(self.interval)
            .context("browser wire reservation overflow")?;
        let next = now
            .checked_add(reservation)
            .context("browser wire reservation deadline overflow")?;
        state.next = next;
        state.generation = generation;
        state.active = Some(generation);
        drop(state);
        Ok(Admission::Reserved(Lease::Reserved { generation }))
    }

    async fn complete(&self, lease: Lease) -> Result<()> {
        let Lease::Reserved { generation } = lease else {
            return Ok(());
        };
        let mut state = self.state.lock().await;
        anyhow::ensure!(
            state.active == Some(generation),
            "browser wire lease changed before completion"
        );
        state.next = Instant::now()
            .checked_add(self.interval)
            .context("browser wire deadline overflow")?;
        state.active = None;
        drop(state);
        Ok(())
    }

    pub async fn complete_response(
        &self,
        lease: Lease,
        result: Result<reqwest::Response>,
        address: &str,
    ) -> Result<reqwest::Response> {
        let completed = self.complete(lease).await;
        match (result, completed) {
            (Ok(response), Ok(())) => Ok(response),
            (Ok(response), Err(error))
                if crate::transport::access_stop_status(response.status().as_u16()) =>
            {
                Err(crate::transport::AccessStopCaptureError::new(
                    response.status().as_u16(),
                    error,
                    address,
                )
                .into())
            }
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => {
                Err(error.context(format!("browser pacing completion failed: {cleanup:#}")))
            }
        }
    }

    pub fn stop(&self, origin: String, status: u16) -> Result<()> {
        let mut stopped = self
            .stopped
            .lock()
            .map_err(|_| anyhow!("browser stop state poisoned"))?;
        if stopped.is_none() {
            *stopped = Some(AccessStop {
                origin,
                status: Some(status),
            });
        }
        drop(stopped);
        Ok(())
    }

    pub fn check_stop(&self) -> Result<()> {
        let stopped = self
            .stopped
            .lock()
            .map_err(|_| anyhow!("browser stop state poisoned"))?;
        stopped
            .as_ref()
            .map_or_else(|| Ok(()), |stop| Err(stop.clone().into()))
    }
}
