use anyhow::Context;
use futures::{StreamExt, TryStreamExt, stream};
use restate_sdk::{context::InvocationHandle, prelude::*};

use super::{AthleticCrawl, AthleticPageClient, PageExecution};
use crate::durable::{
    effects,
    model::{BatchProgress, CrawlRequest, QueueState},
    page::{self, PageOutcome, PageScope},
};

#[derive(Default)]
struct Window {
    owned: Vec<(i64, InvocationHandle)>,
    stopped: bool,
    error: Option<HandlerError>,
}

impl AthleticCrawl {
    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn pipeline(
        &self,
        ctx: &WorkflowContext<'_>,
        request: &CrawlRequest,
    ) -> HandlerResult<BatchProgress> {
        let scope = request.page_scope().terminal()?;
        stream::iter((0..request.config.limits.max_pages).map(Ok::<_, HandlerError>))
            .try_fold(BatchProgress::default(), |progress, _| {
                let scope = &scope;
                async move {
                    let selected = usize::try_from(progress.selected).terminal()?;
                    if progress.queue == QueueState::NoEligiblePage
                        || selected >= request.config.limits.max_pages
                    {
                        return Ok(progress);
                    }
                    let remaining = request.config.limits.max_pages.saturating_sub(selected);
                    let width = remaining.min(request.config.limits.concurrency);
                    let window = self.fill(ctx, request, scope, selected, width).await;
                    Self::drain(progress, window).await
                }
            })
            .await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn fill(
        &self,
        ctx: &WorkflowContext<'_>,
        request: &CrawlRequest,
        scope: &PageScope,
        offset: usize,
        width: usize,
    ) -> Window {
        stream::iter(0..width)
            .fold(Window::default(), |mut window, slot| async move {
                if window.stopped {
                    return window;
                }
                let owned: Vec<_> = window.owned.iter().map(|(id, _)| *id).collect();
                match self
                    .dispatch(ctx, request, scope, offset, slot, &owned)
                    .await
                {
                    Ok(Some(owned)) => window.owned.push(owned),
                    Ok(None) => window.stopped = true,
                    Err(error) => {
                        window.error = Some(error);
                        window.stopped = true;
                    }
                }
                window
            })
            .await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn dispatch(
        &self,
        ctx: &WorkflowContext<'_>,
        request: &CrawlRequest,
        scope: &PageScope,
        offset: usize,
        slot: usize,
        owned: &[i64],
    ) -> HandlerResult<Option<(i64, InvocationHandle)>> {
        let index = offset
            .checked_add(slot)
            .context("pipeline index overflow")
            .terminal()?;
        let now = effects::now!(ctx)?;
        let scheduling =
            page::Scheduling::for_batch_turn(&request.config, request.batch, index).terminal()?;
        let selected = effects::record!(ctx, &format!("select-page-{index:06}"), || {
            page::select_scheduled(
                &self.runtime.database,
                &request.config,
                scope,
                now,
                owned,
                scheduling,
            )
        })?;
        let Some(page) = selected else {
            return Ok(None);
        };
        effects::record!(ctx, &format!("reserve-page-{index:06}"), || {
            page::reserve_configured(&self.runtime.database, &request.config, &page, now)
        })?;
        let id = page.binding.page_id;
        let handle = ctx
            .service_client::<AthleticPageClient>()
            .handle(Json(PageExecution {
                request: page,
                config: request.config.clone(),
            }))
            .idempotency_key(format!("{}-page-{index:06}", request.workflow_key()))
            .send()
            .await?;
        Ok(Some((id, handle)))
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn drain(mut progress: BatchProgress, window: Window) -> HandlerResult<BatchProgress> {
        if window.owned.is_empty() && window.error.is_none() {
            progress.queue = QueueState::NoEligiblePage;
        }
        let (progress, error) = stream::iter(window.owned)
            .fold(
                (progress, window.error),
                |(progress, first), (id, handle)| async move {
                    let result = handle.attach::<Json<PageOutcome>>().await;
                    match result {
                        Ok(Json(outcome)) => match progress.clone().record(id, &outcome) {
                            Ok(progress) => (progress, first),
                            Err(error) => {
                                (progress, first.or_else(|| Some(HandlerError::from(error))))
                            }
                        },
                        Err(error) => (progress, first.or_else(|| Some(HandlerError::from(error)))),
                    }
                },
            )
            .await;
        error.map_or_else(|| Ok(progress), Err)
    }
}
