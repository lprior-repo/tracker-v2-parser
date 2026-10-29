mod identity;
mod store;

#[cfg(test)]
mod tests;

use std::{future::Future, time::Duration};

use anyhow::Result;
use reqwest::Response;

use super::PersistentPacing;

pub use identity::WireRequest;
pub(super) use identity::{WireContext, WireReceipt};

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
        F: Future<Output = Result<Response>>,
    {
        let receipt = self
            .wire_context
            .as_ref()
            .map(|context| context.next(&request))
            .transpose()?;
        if let Some(receipt) = &receipt {
            self.check_wire_origin(&request).await?;
            self.check_wire(receipt, &request).await?;
        }
        self.execute_guarded(
            request.source,
            &request.address,
            interval,
            self.send_recorded(&request, receipt.as_ref(), operation),
            |response| Some(response.status().as_u16()),
        )
        .await
    }

    async fn send_recorded<F>(
        &self,
        request: &WireRequest,
        receipt: Option<&WireReceipt>,
        operation: F,
    ) -> Result<Response>
    where
        F: Future<Output = Result<Response>>,
    {
        self.check_wire_origin(request).await?;
        self.check_contact_origin(request).await?;
        if let Some(receipt) = receipt {
            self.begin_wire(receipt, request).await?;
        }
        let result = operation.await;
        let status = result
            .as_ref()
            .ok()
            .map(|response| response.status().as_u16());
        let recorded = match receipt {
            Some(receipt) => self.complete_wire(receipt, status).await,
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
