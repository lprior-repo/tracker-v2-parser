mod slots;

use anyhow::Context;
use futures::{StreamExt, TryStreamExt, stream};
use restate_sdk::{
    context::{DurableFuture, InvocationHandle},
    prelude::*,
};

use super::AthleticCrawl;
use crate::durable::{
    model::{BatchProgress, CrawlRequest, QueueState},
    page::{PageOutcome, PageScope},
};
use slots::ActivePages;

struct RollingWindow<F> {
    pending: DurableFuturesUnordered<F>,
    active: ActivePages,
    progress: BatchProgress,
    dispatched: usize,
    error: Option<HandlerError>,
}

impl<F> Default for RollingWindow<F> {
    fn default() -> Self {
        Self {
            pending: DurableFuturesUnordered::new(),
            active: ActivePages::default(),
            progress: BatchProgress::default(),
            dispatched: 0,
            error: None,
        }
    }
}

impl<F> RollingWindow<F> {
    fn failed(&mut self, error: HandlerError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    fn finish(self) -> HandlerResult<BatchProgress> {
        if !self.pending.is_empty() {
            return Err(
                TerminalError::new("rolling window failed to drain within page budget").into(),
            );
        }
        self.error.map_or_else(|| Ok(self.progress), Err)
    }
}

impl<F: DurableFuture<Output = Result<Json<PageOutcome>, TerminalError>>> RollingWindow<F> {
    #[tracing::instrument(level = "debug", skip_all)]
    async fn settle(mut self) -> HandlerResult<Self> {
        if let Some((index, outcome)) = self.pending.next().await? {
            let recorded = match (self.active.remove(index), outcome) {
                (Ok(id), Ok(Json(outcome))) => self
                    .progress
                    .clone()
                    .record(id, &outcome)
                    .map_err(HandlerError::from),
                (Err(error), _) => Err(TerminalError::new(error.to_string()).into()),
                (_, Err(error)) => Err(error.into()),
            };
            match recorded {
                Ok(progress) => self.progress = progress,
                Err(error) => self.failed(error),
            }
        }
        Ok(self)
    }
}

impl AthleticCrawl {
    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn rolling_pipeline(
        &self,
        ctx: &WorkflowContext<'_>,
        request: &CrawlRequest,
    ) -> HandlerResult<BatchProgress> {
        let scope = request.page_scope().terminal()?;
        let attach = |handle: InvocationHandle| handle.attach::<Json<PageOutcome>>();
        let window = stream::iter((0..request.config.limits.max_pages).map(Ok::<_, HandlerError>))
            .try_fold(RollingWindow::default(), |window, _| {
                let scope = &scope;
                let attach = &attach;
                async move {
                    self.refill(ctx, request, scope, window, attach)
                        .await
                        .settle()
                        .await
                }
            })
            .await?;
        window.finish()
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn refill<F, A>(
        &self,
        ctx: &WorkflowContext<'_>,
        request: &CrawlRequest,
        scope: &PageScope,
        mut window: RollingWindow<F>,
        attach: &A,
    ) -> RollingWindow<F>
    where
        A: Fn(InvocationHandle) -> F + Sync,
        F: Send,
    {
        if window.error.is_some() || window.progress.queue == QueueState::NoEligiblePage {
            return window;
        }
        let width = match slots::available(
            request.config.limits.concurrency,
            window.pending.len(),
            window.dispatched,
            request.config.limits.max_pages,
        ) {
            Ok(width) => width,
            Err(error) => {
                window.failed(error.into());
                return window;
            }
        };
        stream::iter(0..width)
            .fold((window, false), |(mut window, stopped), _| async move {
                if stopped || window.error.is_some() {
                    return (window, true);
                }
                let owned = window.active.ids();
                match self
                    .dispatch(ctx, request, scope, window.dispatched, 0, &owned)
                    .await
                {
                    Ok(Some((page, handle))) => {
                        let index = window.pending.push(attach(handle));
                        let result = window
                            .active
                            .insert(index, page, request.config.limits.concurrency)
                            .and_then(|()| {
                                window.dispatched = window
                                    .dispatched
                                    .checked_add(1)
                                    .context("rolling dispatch counter overflow")?;
                                Ok(())
                            });
                        if let Err(error) = result {
                            window.failed(error.into());
                        }
                        (window, false)
                    }
                    Ok(None) => {
                        if window.pending.is_empty() {
                            window.progress.queue = QueueState::NoEligiblePage;
                        }
                        (window, true)
                    }
                    Err(error) => {
                        window.failed(error);
                        (window, true)
                    }
                }
            })
            .await
            .0
    }
}
