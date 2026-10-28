use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::page::{
        GetFrameTreeParams, SetWebLifecycleStateParams, SetWebLifecycleStateState,
    },
};
use futures::StreamExt;
use tokio::{
    task::{JoinError, JoinSet},
    time::timeout,
};

use super::Interception;

const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LIFECYCLE_EVENTS: usize = 2048;

impl Interception {
    pub async fn prepare_close(&mut self, page: &Page, complete: bool) -> Result<()> {
        let result = timeout(DRAIN_TIMEOUT, async {
            if complete {
                self.wait_for_idle(page).await?;
            }
            page.execute(SetWebLifecycleStateParams::new(
                SetWebLifecycleStateState::Frozen,
            ))
            .await?;
            self.admission.quiesce().await?;
            if complete {
                drain(&mut self.tasks, false).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("browser interception exceeded drain deadline; acquisition incomplete")
        .and_then(std::convert::identity);
        if !complete || result.is_err() {
            self.tasks.abort_all();
            let cancelled = drain(&mut self.tasks, true).await;
            return result.and(cancelled);
        }
        result
    }

    async fn wait_for_idle(&mut self, page: &Page) -> Result<()> {
        let frame = page
            .execute(GetFrameTreeParams::default())
            .await?
            .result
            .frame_tree
            .frame;
        let expected_frame_id = frame.id;
        let expected_loader_id = frame.loader_id;
        let idle = self
            .idle
            .by_ref()
            .take(MAX_LIFECYCLE_EVENTS)
            .filter(|event| {
                futures::future::ready(
                    event.frame_id == expected_frame_id
                        && event.loader_id == expected_loader_id
                        && event.name == "networkIdle",
                )
            })
            .next()
            .await;
        ensure!(
            idle.is_some(),
            "browser acquisition incomplete: main document never certified network idle"
        );
        Ok(())
    }

    pub async fn finish(mut self, access_stop: bool) -> Result<()> {
        let result = timeout(DRAIN_TIMEOUT, async {
            self.admission.stop().await;
            if let Some(stop) = self.audit_stop.take()
                && stop.send(()).is_err()
            {
                tracing::debug!("browser acquisition audit already stopped");
            }
            let requests = drain(&mut self.tasks, false).await;
            let guards = drain(&mut self.guards, false).await;
            requests.and(guards)
        })
        .await;
        match result {
            Ok(result) => result?,
            Err(_) => self.abort_and_drain().await?,
        }
        ensure!(
            access_stop || !self.blocked.load(std::sync::atomic::Ordering::Acquire),
            "browser acquisition incomplete: a requested destination was blocked"
        );
        Ok(())
    }

    async fn abort_and_drain(&mut self) -> Result<()> {
        let aborted = self.tasks.len().saturating_add(self.guards.len());
        self.tasks.abort_all();
        self.guards.abort_all();
        let requests = drain(&mut self.tasks, true).await;
        let guards = drain(&mut self.guards, true).await;
        tracing::warn!(aborted, "browser interception exceeded drain deadline");
        requests.and(guards)?;
        Err(anyhow!(
            "browser interception exceeded drain deadline; acquisition incomplete"
        ))
    }
}

pub(super) async fn drain(tasks: &mut JoinSet<Result<usize>>, cancelled: bool) -> Result<()> {
    let mut result = Ok(());
    for _ in 0..tasks.len() {
        result = result.and(joined(tasks.join_next().await, cancelled));
    }
    result
}

fn joined(result: Option<Result<Result<usize>, JoinError>>, cancelled: bool) -> Result<()> {
    match result {
        Some(Ok(result)) => {
            let completed =
                result.context("browser acquisition incomplete: interception task failed")?;
            tracing::debug!(completed, "browser interception task drained");
            Ok(())
        }
        Some(Err(error)) if cancelled && error.is_cancelled() => Ok(()),
        Some(Err(error)) => Err(anyhow!(error).context("joining browser interception")),
        None => Ok(()),
    }
}
