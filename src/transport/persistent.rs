#[cfg(test)]
use std::future::Future;

use anyhow::{Context, Result, ensure};
use sqlx::PgPool;

use crate::{
    durable::pacing::{PersistentPacing, WireRequest},
    model::{PageKind, Target},
};

use super::{Transport, request_interval};

impl Transport {
    pub fn with_contact_rate(mut self, rate: Option<f64>) -> Result<Self> {
        ensure!(
            rate.is_none_or(|rate| rate.is_finite() && rate > 0.0 && rate <= 3.0),
            "contact request rate must be finite, positive and at most 3"
        );
        self.contact_requests_per_second = rate;
        Ok(self)
    }

    pub(super) fn request_interval(&self, target: &Target) -> Result<std::time::Duration> {
        match (target.kind, self.contact_requests_per_second) {
            (PageKind::Contacts, Some(rate)) => {
                std::time::Duration::try_from_secs_f64(self.options.rps.min(rate).recip())
                    .context("contact request interval is not representable")
            }
            _ => request_interval(target, self.options.rps),
        }
    }

    #[must_use]
    pub fn with_persistent_pacing(mut self, pool: PgPool) -> Self {
        self.clients.clear();
        let pacing = Some(PersistentPacing::new(pool));
        self.browser = self
            .browser
            .take()
            .map(|browser| browser.with_persistent_pacing(pacing.clone()));
        self.persistent_pacing = pacing;
        self
    }

    #[must_use]
    pub fn with_start_spacing(mut self) -> Self {
        if let Some(pacing) = self.persistent_pacing.clone() {
            self.replace_pacing(pacing.with_start_spacing());
        }
        self
    }

    pub fn set_wire_context(&mut self, operation_id: &str, attempt_id: &str) -> Result<()> {
        let pacing = self
            .persistent_pacing
            .as_ref()
            .context("durable wire context requires persistent PostgreSQL pacing")?
            .clone()
            .with_wire_context(operation_id, attempt_id)?;
        self.replace_pacing(pacing);
        Ok(())
    }

    pub(super) fn reset_wire_sequence(&mut self) -> Result<()> {
        if let Some(pacing) = &self.persistent_pacing {
            let pacing = pacing.restart_wire_sequence()?;
            self.replace_pacing(pacing);
        }
        Ok(())
    }

    fn replace_pacing(&mut self, pacing: PersistentPacing) {
        self.browser = self
            .browser
            .take()
            .map(|browser| browser.with_persistent_pacing(Some(pacing.clone())));
        self.persistent_pacing = Some(pacing);
    }

    pub(super) async fn send_wire(
        &self,
        target: &Target,
        address: &str,
        client: reqwest::Client,
        request: reqwest::Request,
    ) -> Result<reqwest::Response> {
        let Some(pacing) = &self.persistent_pacing else {
            return client
                .execute(request)
                .await
                .context("fetching source response");
        };
        let body = request.body().map_or(Ok(&[][..]), |body| {
            body.as_bytes()
                .context("streaming source requests cannot have a durable wire identity")
        })?;
        let identity = WireRequest::new(target.source, address, request.method().as_str(), body)?;
        pacing
            .send(identity, self.request_interval(target)?, async {
                client
                    .execute(request)
                    .await
                    .context("fetching source response")
            })
            .await
    }

    #[cfg(test)]
    pub(crate) fn with_http_fixture(
        mut self,
        source_origin: &str,
        wire_url: url::Url,
    ) -> Result<Self> {
        self.clients.insert(
            source_origin.to_owned(),
            reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        );
        self.http_test_url = Some(wire_url);
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) fn with_pacing_test_polls(mut self, polls: usize) -> Result<Self> {
        let pacing = self
            .persistent_pacing
            .clone()
            .context("pacing test requires persistent PostgreSQL pacing")?;
        self.replace_pacing(pacing.with_admission_test_polls(polls));
        Ok(self)
    }

    #[cfg(test)]
    pub(super) async fn paced_wire<T, F>(
        &self,
        target: &Target,
        address: &str,
        operation: F,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        match &self.persistent_pacing {
            Some(pacing) => {
                pacing
                    .execute(
                        target.source,
                        address,
                        self.request_interval(target)?,
                        operation,
                    )
                    .await
            }
            None => operation.await,
        }
    }
}
