use anyhow::{Result, anyhow};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use super::AthleticPage;
use crate::durable::{
    effects,
    page::{
        self, CaptureRef, FetchDecision, PageFiles, PageOutcome, PageRequest, PageReservation,
        ParseDecision, ParsedRef, ReservationDecision,
    },
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_version: Option<crate::durable::model::SchedulerVersion>,
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
        validate_execution(&input)?;
        let reservation = match self.reserve_attempt(&ctx, &input).await? {
            ReservationDecision::Reserved { reservation } => reservation,
            ReservationDecision::Settled { outcome } => return Ok(Json(outcome)),
        };
        let capture = match effects::record!(&ctx, "fetch-and-retain-page", || {
            self.fetch_live(&input, &reservation)
        })? {
            FetchDecision::Captured { capture } => capture,
            FetchDecision::Settled { outcome } => return Ok(Json(outcome)),
        };
        let parsed = match self.parse_capture(&ctx, &reservation, &capture).await? {
            ParseDecision::Parsed { parsed } => parsed,
            ParseDecision::Settled { outcome } => return Ok(Json(outcome)),
        };
        self.commit_page(&ctx, &reservation, &capture, &parsed)
            .await
            .map(Json)
    }
}

impl AthleticPage {
    fn files(&self) -> PageFiles<'_> {
        PageFiles::new(&self.runtime.artifact_root, &self.runtime.blocking)
    }

    async fn reserve_attempt(
        &self,
        ctx: &Context<'_>,
        input: &PageExecution,
    ) -> HandlerResult<ReservationDecision> {
        let now = effects::now!(ctx)?;
        effects::record!(ctx, "reserve-page-attempt", || page::reserve_configured(
            &self.runtime.database,
            &input.config,
            &input.request,
            now
        ))
    }

    async fn fetch_live(
        &self,
        input: &PageExecution,
        reservation: &PageReservation,
    ) -> Result<FetchDecision> {
        let database = &self.runtime.database;
        let mut transport = self.live_transport(input)?;
        let guard = crate::durable::admission::acquire(
            database,
            input.config.limits.concurrency,
            transport.uses_browser(&input.request.target),
        )
        .await?;
        let result = self
            .fetch_admitted(input, reservation, &mut transport)
            .await;
        let result = finish(result, transport.close().await);
        finish(result, guard.rollback().await.map_err(anyhow::Error::from))
    }

    fn live_transport(&self, input: &PageExecution) -> Result<Transport> {
        let mut transport = transport(&input.config, &input.request)?
            .with_shared_persistent_pacing(self.runtime.pacing.clone())
            .with_shared_http_clients(self.runtime.http_clients.clone());
        if input.config.limits.concurrency > 1 {
            transport = transport.with_start_spacing();
            if input.scheduler_version.is_some() {
                transport = transport.with_overlapped_starts();
            }
        }
        Ok(transport)
    }

    async fn fetch_admitted(
        &self,
        input: &PageExecution,
        reservation: &PageReservation,
        transport: &mut Transport,
    ) -> Result<FetchDecision> {
        let database = &self.runtime.database;
        crate::runner::durable::check_stops(database, &input.config).await?;
        transport.set_wire_context(
            reservation.request.binding.operation_id(),
            &reservation.attempt_id,
        )?;
        page::fetch_configured(
            database,
            self.files(),
            reservation,
            transport,
            chrono::Utc::now().timestamp_millis(),
            &input.config,
        )
        .await
    }

    async fn parse_capture(
        &self,
        ctx: &Context<'_>,
        reservation: &PageReservation,
        capture: &CaptureRef,
    ) -> HandlerResult<ParseDecision> {
        let now = effects::now!(ctx)?;
        effects::record!(ctx, "parse-retained-page", || page::parse(
            &self.runtime.database,
            self.files(),
            reservation,
            capture,
            now
        ))
    }

    async fn commit_page(
        &self,
        ctx: &Context<'_>,
        reservation: &PageReservation,
        capture: &CaptureRef,
        parsed: &ParsedRef,
    ) -> HandlerResult<PageOutcome> {
        effects::record!(ctx, "commit-page-and-receipt", || page::persist(
            &self.runtime.database,
            self.files(),
            reservation,
            capture,
            parsed
        ))
    }
}

fn validate_execution(input: &PageExecution) -> HandlerResult<()> {
    page::validate_request(&input.request).terminal()?;
    if crate::durable::model::page_config_digest(&input.config).terminal()?
        != input.request.binding.scope().config_sha256
    {
        return Err(TerminalError::new("page configuration binding mismatch").into());
    }
    Ok(())
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
