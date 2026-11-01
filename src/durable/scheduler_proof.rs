use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow, ensure};
use restate_sdk::prelude::*;
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet};

use super::{effects, page, services::PageExecution};
use crate::database::Database;

struct FixturePage {
    database: Arc<Database>,
    slow_page: i64,
}

#[restate_sdk::service(
    name = "AthleticPage",
    idempotency_retention = "90 days",
    journal_retention = "90 days"
)]
impl FixturePage {
    #[handler]
    async fn handle(
        &self,
        ctx: Context<'_>,
        Json(input): Json<PageExecution>,
    ) -> HandlerResult<Json<page::PageOutcome>> {
        page::validate_request(&input.request).terminal()?;
        if super::model::page_config_digest(&input.config).terminal()?
            != input.request.binding.scope.config_sha256
        {
            return Err(
                TerminalError::new("scheduler proof configuration binding mismatch").into(),
            );
        }
        if input.request.binding.page_id == self.slow_page {
            ctx.sleep(Duration::from_secs(45)).await?;
        }
        let now = effects::now!(&ctx)?;
        let decision = effects::record!(
            &ctx,
            "reserve-page-attempt",
            || page::reserve_configured(&self.database, &input.config, &input.request, now)
        )?;
        match decision {
            page::ReservationDecision::Settled { outcome } => Ok(Json(outcome)),
            page::ReservationDecision::Reserved { .. } => Err(TerminalError::new(
                "scheduler proof fixture unexpectedly acquired a source attempt",
            )
            .into()),
        }
    }
}

pub struct Worker {
    database: Arc<Database>,
    endpoint: String,
    cancel: oneshot::Sender<()>,
    tasks: JoinSet<()>,
}

impl Worker {
    #[tracing::instrument(skip_all)]
    pub async fn start(database: &str, slow_page: i64) -> Result<Self> {
        let url = url::Url::parse(database)?;
        ensure!(
            url.query_pairs().any(|(key, value)| key == "options"
                && value.strip_prefix("-csearch_path=").is_some_and(|schema| {
                    schema.starts_with("restate_e2e_upgrade_")
                        && schema.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                        })
                })),
            "scheduler proof requires its owned upgrade schema"
        );
        ensure!(slow_page > 0, "scheduler proof page ID must be positive");
        let database = Arc::new(Database::connect(database).await?);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let service = Endpoint::builder()
            .bind(FixturePage {
                database: database.clone(),
                slow_page,
            })
            .build();
        let (cancel, cancelled) = oneshot::channel();
        let mut tasks = JoinSet::new();
        drop(tasks.spawn(async move {
            HttpServer::new(service)
                .serve_with_cancel(listener, async move {
                    drop(cancelled.await);
                })
                .await;
        }));
        Ok(Self {
            database,
            endpoint,
            cancel,
            tasks,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    #[tracing::instrument(skip_all)]
    pub async fn stop(mut self) -> Result<()> {
        let cancelled = self
            .cancel
            .send(())
            .map_err(|()| anyhow!("scheduler proof HTTP server ended before cancellation"));
        let joined = tokio::time::timeout(Duration::from_secs(15), self.tasks.join_next())
            .await
            .context("scheduler proof HTTP shutdown exceeded bound");
        self.database.pool().close().await;
        cancelled?;
        joined?.context("scheduler proof task absent")??;
        ensure!(
            self.tasks.is_empty(),
            "scheduler proof task remains running"
        );
        Ok(())
    }
}
