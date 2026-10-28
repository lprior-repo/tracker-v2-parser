use std::{sync::Mutex, time::Duration};

use anyhow::{Context, Result, anyhow};
use tokio::{sync::Mutex as AsyncMutex, time::Instant};

use crate::transport::AccessStop;

pub(super) struct Pacing {
    interval: Duration,
    next: AsyncMutex<Instant>,
    stopped: Mutex<Option<AccessStop>>,
}

impl Pacing {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: AsyncMutex::new(Instant::now()),
            stopped: Mutex::new(None),
        }
    }

    pub async fn admit(&self) -> Result<()> {
        let mut next = self.next.lock().await;
        self.check_stop()?;
        tokio::time::sleep_until(*next).await;
        self.check_stop()?;
        *next = Instant::now()
            .checked_add(self.interval)
            .context("browser wire deadline overflow")?;
        drop(next);
        Ok(())
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
