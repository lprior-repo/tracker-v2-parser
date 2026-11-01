mod store;
#[cfg(test)]
mod tests;

use std::{
    future::{Future, poll_fn},
    task::Poll,
    time::Duration,
};

use anyhow::{Context, Result};
use futures::{TryStreamExt, stream};
use reqwest::Response;
use tokio::time::Instant;

use super::{PersistentPacing, WireReceipt, WireRequest, preserve_status};
use crate::durable::pacing::{
    MAX_ADMISSION_POLLS, MAX_ADMISSION_WAIT, WIRE_TIMEOUT, admission_delay,
};

impl PersistentPacing {
    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn send_overlapped<F>(
        &self,
        request: &WireRequest,
        receipt: &WireReceipt,
        interval: Duration,
        operation: F,
    ) -> Result<Response>
    where
        F: Future<Output = Result<Response>> + Send,
    {
        let deadline = self.admit_overlapped(request, receipt, interval).await?;
        let mut executed_at = None;
        let result = tokio::time::timeout_at(deadline, async {
            let mut start = self.starts.admit(request.source, interval).await?;
            self.check_wire_origin(request).await?;
            self.check_contact_origin(request).await?;
            if Instant::now() >= deadline {
                return Err(
                    anyhow::Error::from(crate::durable::AcquisitionBusy).context(
                        "durable start admission exhausted its HTTP ownership deadline before wire",
                    ),
                );
            }
            *start = Some(
                Instant::now()
                    .checked_add(interval)
                    .context("client start fence overflow")?,
            );
            let mut operation = std::pin::pin!(operation);
            executed_at = Some(chrono::Utc::now());
            let immediate = poll_fn(|cx| {
                Poll::Ready(match operation.as_mut().poll(cx) {
                    Poll::Ready(result) => Some(result),
                    Poll::Pending => None,
                })
            })
            .await;
            *start = Some(
                Instant::now()
                    .checked_add(interval)
                    .context("client start fence overflow")?,
            );
            drop(start);
            match immediate {
                Some(result) => result,
                None => operation.await,
            }
        })
        .await
        .context("durable overlapped HTTP ownership deadline reached")
        .and_then(std::convert::identity);
        let status = result
            .as_ref()
            .ok()
            .map(|response| response.status().as_u16());
        let recorded = self
            .complete_wire_executed(receipt, status, executed_at)
            .await;
        match (result, recorded) {
            (Ok(response), Ok(())) => Ok(response),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(preserve_status(status, error, &request.address)),
            (Err(error), Err(recording)) => {
                Err(error.context(format!("wire result recording failed: {recording:#}")))
            }
        }
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn admit_overlapped(
        &self,
        request: &WireRequest,
        receipt: &WireReceipt,
        interval: Duration,
    ) -> Result<Instant> {
        let admission = stream::iter((0..MAX_ADMISSION_POLLS).map(Ok::<_, anyhow::Error>))
            .try_fold(None, |admitted, poll| async move {
                if admitted.is_some() {
                    return Ok(admitted);
                }
                let started = Instant::now();
                let deadline = started
                    .checked_add(WIRE_TIMEOUT)
                    .context("overlapped HTTP deadline overflow")?;
                match self.reserve_start(request, receipt, interval).await? {
                    Some((remaining, leased)) => {
                        tokio::time::sleep(admission_delay(remaining, leased, poll)).await;
                        Ok(None)
                    }
                    None => Ok(Some(deadline)),
                }
            });
        tokio::time::timeout(MAX_ADMISSION_WAIT, admission)
            .await
            .context("overlapped wire admission exceeded bounded wait budget")??
            .ok_or(crate::durable::AcquisitionBusy)
            .context("overlapped wire admission exceeded bounded contention budget")
    }
}
