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
use sqlx::{Postgres, Transaction};
use tokio::time::Instant;

use super::{PersistentPacing, WireReceipt, WireRequest, before, preserve_status, settle};
use crate::durable::pacing::{
    MAX_ADMISSION_POLLS, MAX_ADMISSION_WAIT, Spacing, WIRE_TIMEOUT, WireScope, admission_delay,
};

struct OverlappedWire<'a> {
    request: &'a WireRequest,
    receipt: &'a WireReceipt,
}

struct StartLease<'a> {
    scope: WireScope,
    token: &'a str,
    deadline: Instant,
}

struct StartedWire {
    instant: Instant,
    timestamp: chrono::DateTime<chrono::Utc>,
    transaction: Transaction<'static, Postgres>,
}

impl PersistentPacing {
    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn send_overlapped<F>(
        &self,
        request: &WireRequest,
        receipt: &WireReceipt,
        interval: Duration,
        operation: F,
        deadline: Option<Instant>,
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
        let owned_until =
            before(deadline, self.admit_overlapped(request, receipt, interval)).await?;
        let lease = StartLease {
            scope,
            token: store::start_token(receipt)?,
            deadline: deadline.map_or(owned_until, |deadline| deadline.min(owned_until)),
        };
        let wire = OverlappedWire { request, receipt };
        self.execute_overlapped(wire, interval, operation, lease)
            .await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute_overlapped<F>(
        &self,
        wire: OverlappedWire<'_>,
        interval: Duration,
        operation: F,
        lease: StartLease<'_>,
    ) -> Result<Response>
    where
        F: Future<Output = Result<Response>> + Send,
    {
        let deadline = lease.deadline;
        let mut operation = std::pin::pin!(operation);
        let start = before(Some(deadline), async {
            let mut fence = self.starts.admit(wire.request.source, interval).await?;
            let transaction = self.lock_start(wire.request, &lease).await?;
            let instant = Instant::now();
            anyhow::ensure!(instant < deadline, crate::durable::AcquisitionBusy);
            *fence = Some(
                instant
                    .checked_add(interval)
                    .context("client start fence overflow")?,
            );
            let timestamp = chrono::Utc::now();
            let immediate = poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx))).await;
            drop(fence);
            Ok((
                StartedWire {
                    instant,
                    timestamp,
                    transaction,
                },
                immediate,
            ))
        })
        .await;
        let (started, immediate) = match start {
            Ok(start) => start,
            Err(error) => return self.settle_unstarted(wire, &lease, error).await,
        };
        let denial = matches!(&immediate, Poll::Ready(Ok(response)) if crate::transport::access_stop_status(response.status().as_u16()));
        let response = async {
            match immediate {
                Poll::Ready(result) => result,
                Poll::Pending => before(Some(deadline), operation).await,
            }
        };
        let address = wire.request.address.as_str();
        let record = self.settle_response(wire, response, Some(started.timestamp));
        let release = || {
            settle(Self::complete_locked(
                started.transaction,
                &lease.scope,
                lease.token,
                Some(started.instant),
            ))
        };
        let (result, completed) = if denial {
            (record.await, release().await)
        } else {
            tokio::join!(record, release())
        };
        finish(
            result,
            completed,
            address,
            "wire start pacing completion failed",
        )
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn settle_unstarted(
        &self,
        wire: OverlappedWire<'_>,
        lease: &StartLease<'_>,
        error: anyhow::Error,
    ) -> Result<Response> {
        let address = wire.request.address.as_str();
        let (result, completed) = tokio::join!(
            self.settle_response(wire, async { Err(error) }, None),
            settle(self.complete(&lease.scope, lease.token, None)),
        );
        finish(
            result,
            completed,
            address,
            "wire start pacing completion failed",
        )
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn settle_response(
        &self,
        wire: OverlappedWire<'_>,
        response: impl Future<Output = Result<Response>>,
        executed_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Response> {
        let result = response.await;
        let status = result
            .as_ref()
            .ok()
            .map(|response| response.status().as_u16());
        let recorded = settle(self.complete_wire_executed(wire.receipt, status, executed_at)).await;
        finish(
            result,
            recorded,
            &wire.request.address,
            "wire result recording failed",
        )
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
