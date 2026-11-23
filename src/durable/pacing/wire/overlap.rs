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
    MAX_ADMISSION_POLLS, MAX_ADMISSION_WAIT, Spacing, WIRE_TIMEOUT, WireScope, admission_delay,
};

struct StartLease<'a> {
    scope: WireScope,
    token: &'a str,
    deadline: Instant,
    started: Option<Instant>,
    executed_at: Option<chrono::DateTime<chrono::Utc>>,
    observed_status: Option<u16>,
    completion_attempted: bool,
}

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
        let scope = WireScope::parse(
            request.source,
            &request.address,
            interval,
            Spacing::RequestStarts,
        )?;
        let token = store::start_token(receipt)?;
        let deadline = self.admit_overlapped(request, receipt, interval).await?;
        let mut lease = StartLease {
            scope,
            token,
            deadline,
            started: None,
            executed_at: None,
            observed_status: None,
            completion_attempted: false,
        };
        let result = tokio::time::timeout_at(
            deadline,
            self.execute_overlapped(request, interval, operation, &mut lease),
        )
        .await
        .context("durable overlapped HTTP ownership deadline reached")
        .and_then(std::convert::identity);
        let result = if lease.completion_attempted {
            result
        } else {
            finish(
                result,
                self.release_start(&mut lease).await,
                &request.address,
                "wire start pacing completion failed",
            )
        };
        let status = result
            .as_ref()
            .ok()
            .map(|response| response.status().as_u16())
            .or(lease.observed_status);
        let recorded = self
            .complete_wire_executed(receipt, status, lease.executed_at)
            .await;
        finish_observed(result, recorded, status, &request.address)
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute_overlapped<F>(
        &self,
        request: &WireRequest,
        interval: Duration,
        operation: F,
        lease: &mut StartLease<'_>,
    ) -> Result<Response>
    where
        F: Future<Output = Result<Response>> + Send,
    {
        let mut start = self.starts.admit(request.source, interval).await?;
        self.check_wire_origin(request).await?;
        self.check_contact_origin(request).await?;
        if Instant::now() >= lease.deadline {
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
        lease.started = Some(Instant::now());
        lease.executed_at = Some(chrono::Utc::now());
        let immediate = poll_fn(|cx| {
            Poll::Ready(match operation.as_mut().poll(cx) {
                Poll::Ready(result) => Some(result),
                Poll::Pending => None,
            })
        })
        .await;
        lease.observed_status = immediate
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|response| response.status().as_u16());
        *start = Some(
            Instant::now()
                .checked_add(interval)
                .context("client start fence overflow")?,
        );
        drop(start);
        let completed = self.release_start(lease).await;
        if let Some(result) = immediate {
            finish(
                result,
                completed,
                &request.address,
                "wire start pacing completion failed",
            )
        } else {
            completed.context("wire start pacing completion failed")?;
            operation.await
        }
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn release_start(&self, lease: &mut StartLease<'_>) -> Result<()> {
        lease.completion_attempted = true;
        self.complete(&lease.scope, lease.token, lease.started)
            .await
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

fn finish_observed(
    result: Result<Response>,
    recorded: Result<()>,
    status: Option<u16>,
    address: &str,
) -> Result<Response> {
    finish(result, recorded, address, "wire result recording failed").map_err(|error| {
        if error.is::<crate::transport::AccessStop>()
            || error.is::<crate::transport::AccessStopCaptureError>()
        {
            error
        } else {
            preserve_status(status, error, address)
        }
    })
}

fn finish(
    result: Result<Response>,
    completed: Result<()>,
    address: &str,
    context: &str,
) -> Result<Response> {
    match (result, completed) {
        (Ok(response), Ok(())) => Ok(response),
        (Err(error), Ok(())) => Err(error),
        (Ok(response), Err(error)) => Err(preserve_status(
            Some(response.status().as_u16()),
            error,
            address,
        )),
        (Err(error), Err(completion)) => Err(error.context(format!("{context}: {completion:#}"))),
    }
}
