use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow, ensure};
use restate_sdk::prelude::*;
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet};

use super::{effects, page, services::PageExecution};
use crate::{
    database::Database,
    durable::page::{FailureClass, FailureRecord, PageOutcome, ReservationDecision, RetryPolicy},
};

struct ExtensionPage {
    database: Arc<Database>,
    page_id: i64,
}

#[restate_sdk::service(
    name = "AthleticPage",
    idempotency_retention = "90 days",
    journal_retention = "90 days"
)]
impl ExtensionPage {
    #[handler]
    async fn handle(
        &self,
        ctx: Context<'_>,
        Json(input): Json<PageExecution>,
    ) -> HandlerResult<Json<PageOutcome>> {
        page::validate_request(&input.request).terminal()?;
        if input.request.binding.page_id != self.page_id
            || input.request.deadline_extension.is_none()
        {
            return Err(TerminalError::new(
                "extension fixture received a different page or unextended request",
            )
            .into());
        }
        ctx.sleep(Duration::from_secs(30)).await?;
        if crate::durable::model::page_config_digest(&input.config).terminal()?
            != input.request.binding.scope.config_sha256
        {
            return Err(
                TerminalError::new("extension fixture configuration binding mismatch").into(),
            );
        }
        let now = effects::now!(&ctx)?;
        let policy = RetryPolicy::configured(&input.config, &input.request).terminal()?;
        let decision = effects::record!(
            &ctx,
            "reserve-page-attempt",
            || page::reserve_configured(&self.database, &input.config, &input.request, now)
        )?;
        let ReservationDecision::Reserved { reservation } = decision else {
            return Err(
                TerminalError::new("extended operation was not reservable after restart").into(),
            );
        };
        let outcome = effects::record!(&ctx, "fail-extended-attempt", || async {
            self.database
                .durable_fail_with_policy(
                    &reservation,
                    FailureRecord {
                        class: FailureClass::Transient,
                        detail: "controlled post-extension transient failure",
                        capture: None,
                        stopped_origin: None,
                        observed_at_ms: now,
                    },
                    &policy,
                )
                .await
        })?;
        if !matches!(
            &outcome,
            PageOutcome::Deferred { retry_at_ms, .. }
                if *retry_at_ms > input.request.binding.scope.deadline_ms
                    && *retry_at_ms < input.request.effective_deadline_ms()
        ) {
            return Err(TerminalError::new(
                "extended deadline did not preserve the transient retry",
            )
            .into());
        }
        Ok(Json(outcome))
    }
}

pub struct ExtensionWorker {
    database: Arc<Database>,
    endpoint: String,
    cancel: oneshot::Sender<()>,
    tasks: JoinSet<()>,
}

impl ExtensionWorker {
    pub(crate) async fn start(database_url: &str, page_id: i64) -> Result<Self> {
        let url = url::Url::parse(database_url)?;
        ensure!(
            url.query_pairs().any(|(key, value)| key == "options"
                && value.strip_prefix("-csearch_path=").is_some_and(|schema| {
                    schema.starts_with("restate_e2e_upgrade_")
                        && schema.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                        })
                })),
            "deadline extension fixture requires its owned upgrade schema"
        );
        let database = Arc::new(Database::connect(database_url).await?);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let service = Endpoint::builder()
            .bind(ExtensionPage {
                database: database.clone(),
                page_id,
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

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) async fn stop(mut self) -> Result<()> {
        let cancelled = self
            .cancel
            .send(())
            .map_err(|()| anyhow!("extension fixture ended before cancellation"));
        let joined = tokio::time::timeout(Duration::from_secs(15), self.tasks.join_next())
            .await
            .context("extension fixture shutdown exceeded bound");
        self.database.pool().close().await;
        cancelled?;
        joined?.context("extension fixture task absent")??;
        ensure!(
            self.tasks.is_empty(),
            "extension fixture task remains running"
        );
        Ok(())
    }
}
