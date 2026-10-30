use anyhow::{Result, anyhow};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use super::AthleticPage;
use crate::durable::{
    effects,
    page::{self, FetchDecision, PageOutcome, PageRequest, ParseDecision, ReservationDecision},
};
use crate::{
    config::{Config, TransportMode},
    transport::{Mode, Options, Transport},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageExecution {
    pub request: PageRequest,
    pub config: Config,
}

#[restate_sdk::service(
    idempotency_retention = "90 days",
    journal_retention = "90 days",
    invocation_retry_policy(
        initial_interval = "1s",
        factor = 2.0,
        max_interval = "5m",
        max_attempts = 10000,
        on_max_attempts = "pause"
    )
)]
impl AthleticPage {
    #[handler]
    async fn handle(
        &self,
        ctx: Context<'_>,
        Json(input): Json<PageExecution>,
    ) -> HandlerResult<Json<PageOutcome>> {
        page::validate_request(&input.request).terminal()?;
        if crate::durable::model::page_config_digest(&input.config).terminal()?
            != input.request.binding.scope.config_sha256
        {
            return Err(TerminalError::new("page configuration binding mismatch").into());
        }
        let database = &self.runtime.database;
        let now = effects::now!(&ctx)?;
        let reservation =
            match effects::record!(&ctx, "reserve-page-attempt", || page::reserve_configured(
                database,
                &input.config,
                &input.request,
                now
            ))? {
                ReservationDecision::Reserved { reservation } => reservation,
                ReservationDecision::Settled { outcome } => return Ok(Json(outcome)),
            };
        let capture = match effects::record!(&ctx, "fetch-and-retain-page", || async {
            let mut transport = transport(&input.config, &input.request)?
                .with_persistent_pacing(database.pool().clone());
            if input.config.limits.concurrency > 1 {
                transport = transport.with_start_spacing();
            }
            let guard = crate::durable::admission::acquire(
                database,
                input.config.limits.concurrency,
                transport.uses_browser(&input.request.target),
            )
            .await?;
            crate::runner::durable::check_stops(database, &input.config).await?;
            transport.set_wire_context(
                &reservation.request.binding.operation_id,
                &reservation.attempt_id,
            )?;
            let result = page::fetch_configured(
                database,
                &self.runtime.artifact_root,
                &reservation,
                &mut transport,
                chrono::Utc::now().timestamp_millis(),
                &input.config,
            )
            .await;
            let result = finish(result, transport.close().await);
            finish(result, guard.rollback().await.map_err(anyhow::Error::from))
        })? {
            FetchDecision::Captured { capture } => capture,
            FetchDecision::Settled { outcome } => return Ok(Json(outcome)),
        };
        let now = effects::now!(&ctx)?;
        let parsed = match effects::record!(&ctx, "parse-retained-page", || page::parse(
            database,
            &self.runtime.artifact_root,
            &reservation,
            &capture,
            now
        ))? {
            ParseDecision::Parsed { parsed } => parsed,
            ParseDecision::Settled { outcome } => return Ok(Json(outcome)),
        };
        Ok(Json(effects::record!(
            &ctx,
            "commit-page-and-receipt",
            || page::persist(
                database,
                &self.runtime.artifact_root,
                &reservation,
                &capture,
                &parsed
            )
        )?))
    }
}

fn transport(config: &Config, request: &PageRequest) -> Result<Transport> {
    let mode = match config.settings(request.target.source).transport {
        TransportMode::Auto => Mode::Auto,
        TransportMode::Http => Mode::Http,
        TransportMode::Browser => Mode::Browser,
    };
    Transport::new(Options {
        mode,
        driver_url: None,
        profile: std::env::temp_dir().join(format!(
            "tracker-durable-{}",
            request.target.source.as_str()
        )),
        headless: true,
        rps: config.limits.requests_per_second,
    })?
    .with_contact_rate(config.limits.contact_requests_per_second)
}

fn finish<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(anyhow!("{error:#}; cleanup also failed: {cleanup:#}")),
    }
}
