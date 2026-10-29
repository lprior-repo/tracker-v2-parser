use std::time::Duration;

use futures::{TryStreamExt, stream};
use restate_sdk::prelude::*;

use super::{AthleticCrawl, AthleticOperationsClient, AthleticPageClient, PageExecution};
use crate::durable::{
    effects,
    model::{self, BatchProgress, CrawlPhase, CrawlReport, CrawlRequest, QueueState},
    operations::{OperationRequest, OperationSpec},
    page,
};

#[restate_sdk::workflow(
    workflow_completion_retention = "180 days",
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
impl AthleticCrawl {
    #[handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(request): Json<CrawlRequest>,
    ) -> HandlerResult<Json<CrawlReport>> {
        let now = effects::now!(&ctx)?;
        request.validate(now.min(request.deadline_ms)).terminal()?;
        if ctx.key() != request.workflow_key() {
            return Err(TerminalError::new("workflow key does not match crawl binding").into());
        }
        if now >= request.deadline_ms {
            let report = CrawlReport {
                generation: request.generation.clone(),
                batch: request.batch,
                phase: request.phase,
                progress: BatchProgress::default(),
                export: None,
                continuation: None,
                stopped_reason: Some(
                    "durable crawl deadline reached; retained checkpoints are resumable".into(),
                ),
            };
            ctx.set("report", Json(report.clone()));
            return Ok(Json(report));
        }
        effects::record!(&ctx, "bootstrap-discovery-and-contact-queue", || {
            crate::runner::durable::bootstrap(&self.runtime.database, &request.config)
        })?;
        let progress = if request.phase == CrawlPhase::ContactRefresh {
            let operation = OperationRequest {
                operation_id: format!("{}-wiaa", request.workflow_key()),
                spec: OperationSpec::WiaaDirectory {
                    config: request.config.clone(),
                },
            };
            ctx.service_client::<AthleticOperationsClient>()
                .execute(Json(operation))
                .idempotency_key(format!("{}-wiaa", request.workflow_key()))
                .call()
                .await?;
            BatchProgress {
                queue: QueueState::NoEligiblePage,
                ..BatchProgress::default()
            }
        } else {
            self.batch(&ctx, &request).await?
        };
        let operation = OperationRequest {
            operation_id: format!("{}-export", request.workflow_key()),
            spec: OperationSpec::Export {
                config: request.config.clone(),
            },
        };
        let Json(export) = ctx
            .service_client::<AthleticOperationsClient>()
            .execute(Json(operation))
            .idempotency_key(format!("{}-export", request.workflow_key()))
            .call()
            .await?;
        let now = effects::now!(&ctx)?;
        let scope = request.page_scope().terminal()?;
        let due = effects::record!(&ctx, "read-durable-cooldown", || self
            .runtime
            .database
            .durable_due(&scope.run_generation))?;
        let phase = next_phase(&request, &progress, due);
        let mut report = CrawlReport {
            generation: request.generation.clone(),
            batch: request.batch,
            phase,
            progress,
            export: Some(export),
            continuation: None,
            stopped_reason: None,
        };
        let next = request.next(phase).terminal()?;
        if now >= request.deadline_ms {
            report.stopped_reason =
                Some("durable crawl deadline reached; retained checkpoints are resumable".into());
        } else if next.batch >= next.max_batches {
            report.stopped_reason =
                Some("bounded batch count reached; retained checkpoints are resumable".into());
        } else if phase == CrawlPhase::Complete {
            report.stopped_reason =
                Some("no eligible or deferred pages remain in configured scope".into());
        } else {
            let delay =
                if report.progress.queue == QueueState::NoEligiblePage && phase == request.phase {
                    model::delay_until(now, due.unwrap_or_else(|| now.saturating_add(30_000)))
                        .terminal()?
                } else {
                    Duration::ZERO
                };
            let key = next.workflow_key();
            ctx.workflow_client::<AthleticCrawlClient>(&key)
                .run(Json(next))
                .send_after(delay)
                .await?;
            report.continuation = Some(key);
        }
        ctx.set("report", Json(report.clone()));
        Ok(Json(report))
    }

    #[handler]
    async fn status(
        &self,
        ctx: SharedWorkflowContext<'_>,
    ) -> HandlerResult<Json<Option<CrawlReport>>> {
        Ok(Json(
            ctx.get::<Json<CrawlReport>>("report")
                .await?
                .map(|Json(value)| value),
        ))
    }

    async fn batch(
        &self,
        ctx: &WorkflowContext<'_>,
        request: &CrawlRequest,
    ) -> HandlerResult<BatchProgress> {
        if request.config.limits.concurrency > 1 {
            return self.pipeline(ctx, request).await;
        }
        let scope = request.page_scope().terminal()?;
        stream::iter((0..request.config.limits.max_pages).map(Ok::<_, HandlerError>))
            .try_fold(BatchProgress::default(), |mut progress, index| {
                let scope = &scope;
                async move {
                    if progress.queue == QueueState::NoEligiblePage {
                        return Ok(progress);
                    }
                    let now = effects::now!(ctx)?;
                    let selected = effects::record!(
                        ctx,
                        &format!("select-page-{index:06}"),
                        || page::select(&self.runtime.database, &request.config, scope, now)
                    )?;
                    let Some(page) = selected else {
                        progress.queue = QueueState::NoEligiblePage;
                        return Ok(progress);
                    };
                    let id = page.binding.page_id;
                    let Json(outcome) = ctx
                        .service_client::<AthleticPageClient>()
                        .handle(Json(PageExecution {
                            request: page,
                            config: request.config.clone(),
                        }))
                        .idempotency_key(format!("{}-page-{index:06}", request.workflow_key()))
                        .call()
                        .await?;
                    progress.record(id, &outcome).map_err(HandlerError::from)
                }
            })
            .await
    }
}

fn next_phase(request: &CrawlRequest, progress: &BatchProgress, due: Option<i64>) -> CrawlPhase {
    if progress.queue == QueueState::Processing || due.is_some() {
        return request.phase;
    }
    match request.phase {
        CrawlPhase::SchoolChains
            if request.config.states.iter().any(|state| state == "WI")
                && request.config.acquisition.includes_contacts() =>
        {
            CrawlPhase::ContactRefresh
        }
        CrawlPhase::SchoolChains | CrawlPhase::ContactRefresh => CrawlPhase::PublicResults,
        CrawlPhase::PublicResults | CrawlPhase::Complete => CrawlPhase::Complete,
    }
}
