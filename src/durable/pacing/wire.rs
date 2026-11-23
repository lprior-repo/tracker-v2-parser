mod identity;
mod overlap;
mod store;

#[cfg(test)]
mod tests;

use std::{future::Future, sync::OnceLock, time::Duration};

use anyhow::{Context, Result};
use futures::{FutureExt, future::Either};
use reqwest::Response;
use tokio::time::Instant;

use super::{PersistentPacing, WireScope};

pub use identity::WireRequest;
pub(super) use identity::{WireContext, WireReceipt};

const SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(5);

impl PersistentPacing {
    pub fn with_wire_context(mut self, operation_id: &str, attempt_id: &str) -> Result<Self> {
        self.wire_context = Some(WireContext::new(operation_id, attempt_id)?);
        Ok(self)
    }

    pub(crate) fn restart_wire_sequence(&self) -> Result<Self> {
        let mut pacing = Self {
            pool: self.pool.clone(),
            wire_context: None,
            spacing: self.spacing,
            overlapped: self.overlapped,
            starts: self.starts.clone(),
            #[cfg(test)]
            admission_test_polls: self.admission_test_polls,
        };
        if let Some(context) = &self.wire_context {
            pacing.wire_context = Some(WireContext::new(
                &context.operation_id,
                &context.attempt_id,
            )?);
        }
        Ok(pacing)
    }

    #[tracing::instrument(skip_all)]
    pub async fn send<F>(
        &self,
        request: WireRequest,
        interval: Duration,
        operation: F,
    ) -> Result<Response>
    where
        F: Future<Output = Result<Response>> + Send,
    {
        self.send_with_deadline(request, interval, operation, None)
            .await
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn send_with_deadline<F>(
        &self,
        request: WireRequest,
        interval: Duration,
        operation: F,
        deadline: Option<Instant>,
    ) -> Result<Response>
    where
        F: Future<Output = Result<Response>> + Send,
    {
        let receipt = before(deadline, self.prepare_wire(&request)).await?;
        if self.overlapped {
            let response = self.send_overlapped(
                &request,
                receipt
                    .as_ref()
                    .context("overlapped admission requires durable wire identity")?,
                interval,
                operation,
                deadline,
            );
            return response.await;
        }
        let scope = WireScope::parse(request.source, &request.address, interval, self.spacing)?;
        let started = OnceLock::new();
        self.execute_guarded(
            &scope,
            &request.address,
            deadline,
            |deadline| {
                self.send_recorded(&request, receipt.as_ref(), operation, &started, deadline)
            },
            |response| Some(response.status().as_u16()),
            &started,
        )
        .await
    }

    #[tracing::instrument(skip_all)]
    async fn prepare_wire(&self, request: &WireRequest) -> Result<Option<WireReceipt>> {
        let receipt = self
            .wire_context
            .as_ref()
            .map(|context| context.next(request))
            .transpose()?;
        if let Some(receipt) = &receipt {
            self.check_wire_origin(request).await?;
            self.check_wire(receipt, request).await?;
        }
        Ok(receipt)
    }

    async fn send_recorded<F>(
        &self,
        request: &WireRequest,
        receipt: Option<&WireReceipt>,
        operation: F,
        started: &OnceLock<Instant>,
        deadline: Instant,
    ) -> Result<Response>
    where
        F: Future<Output = Result<Response>>,
    {
        before(Some(deadline), async {
            self.check_wire_origin(request).await?;
            self.check_contact_origin(request).await?;
            if let Some(receipt) = receipt {
                self.begin_wire(receipt, request).await?;
            }
            Ok(())
        })
        .await?;
        let result = before(Some(deadline), async {
            let now = Instant::now();
            anyhow::ensure!(now < deadline, "source HTTP request timed out");
            started
                .set(now)
                .map_err(|_| anyhow::anyhow!("wire operation start was already recorded"))?;
            operation.await
        })
        .await;
        let status = result
            .as_ref()
            .ok()
            .map(|response| response.status().as_u16());
        let recorded = match receipt {
            Some(receipt) => settle(self.complete_wire(receipt, status)).await,
            None => Ok(()),
        };
        match (result, recorded) {
            (Ok(response), Ok(())) => Ok(response),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(preserve_status(status, error, &request.address)),
            (Err(error), Err(recording)) => {
                Err(error.context(format!("wire result recording failed: {recording:#}")))
            }
        }
    }
}

pub(super) fn before<T>(
    deadline: Option<Instant>,
    operation: impl Future<Output = Result<T>>,
) -> impl Future<Output = Result<T>> {
    match deadline {
        Some(deadline) => Either::Left(
            tokio::time::timeout_at(deadline, operation)
                .map(|result| result.context("source HTTP request timed out")?),
        ),
        None => Either::Right(operation),
    }
}

pub(super) fn settle(action: impl Future<Output = Result<()>>) -> impl Future<Output = Result<()>> {
    tokio::time::timeout(SETTLEMENT_TIMEOUT, action)
        .map(|result| result.context("durable wire settlement timed out")?)
}

pub(super) fn preserve_status(
    status: Option<u16>,
    error: anyhow::Error,
    address: &str,
) -> anyhow::Error {
    match status.filter(|status| crate::transport::access_stop_status(*status)) {
        Some(status) => {
            crate::transport::AccessStopCaptureError::new(status, error, address).into()
        }
        None => error,
    }
}
