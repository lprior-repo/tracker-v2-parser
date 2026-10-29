#[cfg(test)]
use std::future::Future;

use anyhow::{Context, Result};
use sqlx::PgPool;

use crate::{
    durable::pacing::{PersistentPacing, WireRequest},
    model::Target,
};

use super::{Transport, request_interval};

impl Transport {
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
            .send(
                identity,
                request_interval(target, self.options.rps)?,
                async {
                    client
                        .execute(request)
                        .await
                        .context("fetching source response")
                },
            )
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
                        request_interval(target, self.options.rps)?,
                        operation,
                    )
                    .await
            }
            None => operation.await,
        }
    }
}
