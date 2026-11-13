use anyhow::Context;
use futures::{StreamExt, TryStreamExt, stream};
use restate_sdk::{context::InvocationHandle, prelude::*};

use super::{AthleticCrawl, AthleticPageClient, PageExecution};
use crate::durable::{
    effects,
    model::{BatchProgress, CrawlRequest, QueueState},
    page::{self, PageOutcome, PageScope, SelectionStep},
};

mod step;
pub(super) use step::{DispatchStep, SelectionStop, advance_turn};

#[derive(Default)]
struct Window {
    owned: Vec<(i64, InvocationHandle)>,
    stopped: SelectionStop,
    error: Option<HandlerError>,
    turns: usize,
}

impl AthleticCrawl {
    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn pipeline(
        &self,
        ctx: &WorkflowContext<'_>,
        request: &CrawlRequest,
    ) -> HandlerResult<BatchProgress> {
        let scope = request.page_scope().terminal()?;
        let (progress, _, _) =
            stream::iter((0..request.config.limits.max_pages).map(Ok::<_, HandlerError>))
                .try_fold(
                    (BatchProgress::default(), 0_usize, SelectionStop::Open),
                    |(progress, turns, stopped), _| {
                        let scope = &scope;
                        async move {
                            if progress.queue == QueueState::NoEligiblePage
                                || turns >= request.config.limits.max_pages
                                || stopped == SelectionStop::DeadlineReached
                            {
                                return Ok((progress, turns, stopped));
                            }
                            let remaining = request.config.limits.max_pages.saturating_sub(turns);
                            let width = remaining.min(request.config.limits.concurrency);
                            let window = self.fill(ctx, request, scope, turns, width).await;
                            let used = window.turns;
                            let stopped = window.stopped;
                            let progress = Self::drain(progress, window).await?;
                            let turns = turns
                                .checked_add(used)
                                .context("pipeline turn counter overflow")
                                .terminal()?;
                            Ok((progress, turns, stopped))
                        }
                    },
                )
                .await?;
        Ok(progress)
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
                if window.stopped != SelectionStop::Open || window.error.is_some() {
                    return window;
                }
                let owned: Vec<_> = window.owned.iter().map(|(id, _)| *id).collect();
                match self
                    .dispatch(ctx, request, scope, offset, slot, &owned)
                    .await
                {
                    Ok(DispatchStep::Page(id, handle)) => {
                        window.owned.push((id, handle));
                        window.record_turn(width);
                    }
                    Ok(DispatchStep::PolicyAdvanced) => window.record_turn(width),
                    Ok(DispatchStep::Empty) => window.stopped = SelectionStop::Empty,
                    Ok(DispatchStep::DeadlineReached) => {
                        window.stopped = SelectionStop::DeadlineReached;
                    }
                    Err(error) => {
                        window.error = Some(error);
                    }
                }
                window
            })
            .await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn dispatch(
        &self,
        ctx: &WorkflowContext<'_>,
        request: &CrawlRequest,
        scope: &PageScope,
        offset: usize,
        slot: usize,
        owned: &[i64],
    ) -> HandlerResult<DispatchStep> {
        let index = offset
            .checked_add(slot)
            .context("pipeline index overflow")
            .terminal()?;
        let now = effects::now!(ctx)?;
        let scheduling =
            page::Scheduling::for_batch_turn(&request.config, request.batch, index).terminal()?;
        let selected = effects::record!(ctx, &format!("select-page-{index:06}"), || async {
            let selected = page::select_step_with_extension(
                &self.runtime.database,
                &request.config,
                scope,
                now,
                owned,
                scheduling,
                request.deadline_extension.as_ref(),
            )
            .await?;
            Ok(match selected {
                SelectionStep::Page(page)
                    if request.scheduler_version.is_some()
                        && owned.contains(&page.binding.page_id) =>
                {
                    SelectionStep::Empty
                }
                selected => selected,
            })
        })?;
        let page = match selected {
            SelectionStep::Page(page) => *page,
            SelectionStep::Empty => return Ok(DispatchStep::Empty),
            SelectionStep::PolicyAdvanced => return Ok(DispatchStep::PolicyAdvanced),
            SelectionStep::DeadlineReached => return Ok(DispatchStep::DeadlineReached),
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
                scheduler_version: request.scheduler_version,
            }))
            .idempotency_key(format!("{}-page-{index:06}", request.workflow_key()))
            .send()
            .await?;
        Ok(DispatchStep::Page(id, handle))
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn drain(mut progress: BatchProgress, window: Window) -> HandlerResult<BatchProgress> {
        if window.stopped == SelectionStop::Empty
            && window.owned.is_empty()
            && window.error.is_none()
        {
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

impl Window {
    fn record_turn(&mut self, maximum: usize) {
        match advance_turn(self.turns, maximum) {
            Ok(turns) => self.turns = turns,
            Err(error) => {
                self.error = Some(error.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AthleticCrawl, BatchProgress, QueueState, SelectionStop, Window};

    #[tokio::test]
    async fn policy_only_window_preserves_processing_until_empty_is_observed()
    -> restate_sdk::prelude::HandlerResult<()> {
        let mut window = Window::default();
        window.record_turn(3);
        assert_eq!(window.turns, 1);
        let progress = AthleticCrawl::drain(BatchProgress::default(), window).await?;
        assert_eq!(progress.queue, QueueState::Processing);
        assert_eq!(progress.selected, 0);
        let empty = Window {
            stopped: SelectionStop::Empty,
            ..Window::default()
        };
        let progress = AthleticCrawl::drain(progress, empty).await?;
        assert_eq!(progress.queue, QueueState::NoEligiblePage);
        Ok(())
    }

    #[tokio::test]
    async fn deadline_stop_never_establishes_queue_exhaustion()
    -> restate_sdk::prelude::HandlerResult<()> {
        let stopped = Window {
            stopped: SelectionStop::DeadlineReached,
            ..Window::default()
        };
        let progress = AthleticCrawl::drain(BatchProgress::default(), stopped).await?;
        assert_eq!(progress.queue, QueueState::Processing);
        assert_eq!(progress.selected, 0);
        Ok(())
    }
}
