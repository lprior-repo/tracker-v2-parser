mod slots;

use futures::{StreamExt, TryStreamExt, stream};
use restate_sdk::{
    context::{DurableFuture, InvocationHandle},
    prelude::*,
};

use super::{
    AthleticCrawl,
    pipeline::{DispatchStep, SelectionStop, advance_turn},
};
use crate::{
    config::Limits,
    durable::{
        model::{BatchProgress, CrawlRequest, QueueState},
        page::{PageOutcome, PageScope},
    },
};
use slots::ActivePages;

struct RollingWindow<F> {
    pending: DurableFuturesUnordered<F>,
    active: ActivePages,
    progress: BatchProgress,
    turns: usize,
    stopped: SelectionStop,
    error: Option<HandlerError>,
}

struct PageSelection<'a> {
    request: &'a CrawlRequest,
    scope: &'a PageScope,
}

impl<F> Default for RollingWindow<F> {
    fn default() -> Self {
        Self {
            pending: DurableFuturesUnordered::new(),
            active: ActivePages::default(),
            progress: BatchProgress::default(),
            turns: 0,
            stopped: SelectionStop::Open,
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

    fn refill_width(&mut self, limits: &Limits) -> Option<usize> {
        if self.error.is_some()
            || self.progress.queue == QueueState::NoEligiblePage
            || self.stopped == SelectionStop::DeadlineReached
        {
            return None;
        }
        match slots::available(
            limits.concurrency,
            self.pending.len(),
            self.turns,
            limits.max_pages,
        ) {
            Ok(width) => Some(width),
            Err(error) => {
                self.failed(error.into());
                None
            }
        }
    }

    fn apply_dispatch(
        &mut self,
        result: HandlerResult<DispatchStep>,
        limits: &Limits,
        attach: impl FnOnce(InvocationHandle) -> F,
    ) -> bool {
        match result {
            Ok(DispatchStep::Page(page, handle)) => {
                self.track_page(limits, page, attach(handle));
                return false;
            }
            Ok(DispatchStep::PolicyAdvanced) => {
                self.advance(limits.max_pages);
                return false;
            }
            Ok(DispatchStep::Empty) => self.record_empty_selection(),
            Ok(DispatchStep::DeadlineReached) => self.stopped = SelectionStop::DeadlineReached,
            Err(error) => self.failed(error),
        }
        true
    }

    fn track_page(&mut self, limits: &Limits, page: i64, future: F) {
        let index = self.pending.push(future);
        match self.active.insert(index, page, limits.concurrency) {
            Ok(()) => self.advance(limits.max_pages),
            Err(error) => self.failed(error.into()),
        }
    }

    fn advance(&mut self, max_pages: usize) {
        match advance_turn(self.turns, max_pages) {
            Ok(turns) => self.turns = turns,
            Err(error) => self.failed(error.into()),
        }
    }

    fn record_empty_selection(&mut self) {
        if self.pending.is_empty() {
            self.progress.queue = QueueState::NoEligiblePage;
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
        let selection = &PageSelection {
            request,
            scope: &scope,
        };
        let attach = |handle: InvocationHandle| handle.attach::<Json<PageOutcome>>();
        let window =
            stream::iter((0..request.config().limits.max_pages).map(Ok::<_, HandlerError>))
                .try_fold(RollingWindow::default(), |window, _| async move {
                    self.refill(ctx, selection, window, &attach)
                        .await
                        .settle()
                        .await
                })
                .await?;
        window.finish()
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn refill<F: Send>(
        &self,
        ctx: &WorkflowContext<'_>,
        selection: &PageSelection<'_>,
        mut window: RollingWindow<F>,
        attach: &(impl Fn(InvocationHandle) -> F + Sync),
    ) -> RollingWindow<F> {
        let Some(width) = window.refill_width(&selection.request.config().limits) else {
            return window;
        };
        stream::iter(0..width)
            .fold((window, false), |(mut window, stopped), _| async move {
                if stopped || window.error.is_some() {
                    return (window, true);
                }
                let stopped = self.refill_slot(ctx, selection, &mut window, attach).await;
                (window, stopped)
            })
            .await
            .0
    }

    async fn refill_slot<F: Send>(
        &self,
        ctx: &WorkflowContext<'_>,
        selection: &PageSelection<'_>,
        window: &mut RollingWindow<F>,
        attach: &(impl Fn(InvocationHandle) -> F + Sync),
    ) -> bool {
        let owned = window.active.ids();
        let result = self
            .dispatch(
                ctx,
                selection.request,
                selection.scope,
                window.turns,
                0,
                &owned,
            )
            .await;
        window.apply_dispatch(result, &selection.request.config().limits, attach)
    }
}

#[cfg(test)]
mod tests {
    use super::{DispatchStep, Limits, QueueState, RollingWindow};

    #[test]
    fn empty_selection_closes_refill_only_when_no_pages_are_pending() {
        let limits = Limits {
            concurrency: 2,
            max_pages: 3,
            ..Limits::default()
        };
        let mut busy = RollingWindow::default();
        busy.track_page(&limits, 7, ());
        let mut idle = RollingWindow::default();

        assert!(busy.apply_dispatch(Ok(DispatchStep::Empty), &limits, |_| ()));
        assert!(idle.apply_dispatch(Ok(DispatchStep::Empty), &limits, |_| ()));

        assert_eq!(busy.progress.queue, QueueState::Processing);
        assert_eq!(busy.refill_width(&limits), Some(1));
        assert_eq!(idle.progress.queue, QueueState::NoEligiblePage);
        assert_eq!(idle.refill_width(&limits), None);
    }
}
