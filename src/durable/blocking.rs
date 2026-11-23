use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    task::Poll,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use futures::{TryStreamExt, future::poll_fn, stream};
use tokio::{
    sync::{Mutex as AsyncMutex, oneshot},
    task::{JoinError, JoinSet},
};

const MAX_TASKS: usize = 8;
const DRAIN_BUDGET: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockingStageUnavailable {
    Busy,
    Stopping,
}

impl fmt::Display for BlockingStageUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "owned blocking stage capacity of 8 is exhausted",
            Self::Stopping => "owned blocking stage intake has stopped",
        })
    }
}

impl std::error::Error for BlockingStageUnavailable {}

#[derive(Debug)]
enum BlockingJoinFailure {
    Panicked(JoinError),
    Cancelled(JoinError),
}

impl fmt::Display for BlockingJoinFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panicked(error) => write!(formatter, "owned blocking stage panicked: {error}"),
            Self::Cancelled(error) => write!(
                formatter,
                "owned blocking stage was cancelled before completion: {error}"
            ),
        }
    }
}

impl std::error::Error for BlockingJoinFailure {}

impl From<JoinError> for BlockingJoinFailure {
    fn from(error: JoinError) -> Self {
        if error.is_panic() {
            Self::Panicked(error)
        } else {
            Self::Cancelled(error)
        }
    }
}

#[derive(Default)]
struct Failures {
    details: Vec<String>,
    count: u64,
}

impl Failures {
    fn record(&mut self, detail: String) {
        self.count = self.count.saturating_add(1);
        if self.details.len() < MAX_TASKS {
            self.details.push(detail);
        }
    }
}

#[derive(Clone, Default)]
struct FailureLog(Arc<Mutex<Failures>>);

impl FailureLog {
    fn lock(&self) -> Result<MutexGuard<'_, Failures>> {
        self.0
            .lock()
            .map_err(|_| anyhow!("owned blocking stage failure log is poisoned"))
    }

    fn record(&self, error: &anyhow::Error) {
        let detail: String = format!("{error:#}").chars().take(2048).collect();
        tracing::error!(error = %detail, "owned blocking stage failed");
        match self.lock() {
            Ok(mut failures) => failures.record(detail),
            Err(error) => tracing::error!(%error, "could not retain blocking stage failure"),
        }
    }
}

struct UndeliveredFailure {
    error: Option<anyhow::Error>,
    log: FailureLog,
}

impl Drop for UndeliveredFailure {
    fn drop(&mut self) {
        if let Some(error) = &self.error {
            self.log.record(error);
        }
    }
}

#[derive(Default)]
struct State {
    tasks: JoinSet<Result<()>>,
    failures: FailureLog,
    stopping: bool,
}

impl State {
    fn observe(&self, result: std::result::Result<Result<()>, JoinError>) {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => self.failures.record(&error),
            Err(error) => self
                .failures
                .record(&BlockingJoinFailure::from(error).into()),
        }
    }

    fn reap(&mut self) {
        (0..MAX_TASKS).for_each(|_| {
            if let Some(result) = self.tasks.try_join_next() {
                self.observe(result);
            }
        });
    }

    fn outcome(&self) -> Result<()> {
        ensure!(
            self.tasks.is_empty(),
            "{} owned blocking stages remain running; blocking work cannot be cancelled",
            self.tasks.len()
        );
        let failures = self.failures.lock()?;
        ensure!(
            failures.count == 0,
            "{} owned blocking stages failed: {}",
            failures.count,
            failures.details.join("; ")
        );
        drop(failures);
        Ok(())
    }
}

type StageReceiver<T> = oneshot::Receiver<std::result::Result<T, UndeliveredFailure>>;

#[derive(Default)]
pub struct BlockingStages {
    state: Mutex<State>,
    admission: AsyncMutex<()>,
}

impl BlockingStages {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow!("owned blocking stage registry is poisoned"))
    }

    #[tracing::instrument(skip_all)]
    pub async fn run<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let receiver = {
            let _admission = self
                .admission
                .try_lock()
                .map_err(|_| BlockingStageUnavailable::Busy)?;
            self.admit(&mut Some(operation))?
                .ok_or(BlockingStageUnavailable::Busy)?
        };
        self.receive(receiver).await
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn run_when_available<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let receiver = self.wait_to_admit(operation).await?;
        self.receive(receiver).await
    }

    async fn wait_to_admit<T, F>(&self, operation: F) -> Result<StageReceiver<T>>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let _admission = self.admission.lock().await;
        let mut operation = Some(operation);
        loop {
            if let Some(receiver) = self.admit(&mut operation)? {
                return Ok(receiver);
            }
            self.join_next(true).await?;
        }
    }

    fn admit<T, F>(&self, operation: &mut Option<F>) -> Result<Option<StageReceiver<T>>>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        tokio::runtime::Handle::try_current()
            .context("owned blocking stages require a Tokio runtime")?;
        let mut state = self.state()?;
        state.reap();
        if state.stopping {
            return Err(BlockingStageUnavailable::Stopping.into());
        }
        if state.tasks.len() >= MAX_TASKS {
            return Ok(None);
        }
        let operation = operation
            .take()
            .context("blocking stage was already admitted")?;
        let (sender, receiver) = oneshot::channel();
        let log = state.failures.clone();
        let span = tracing::Span::current();
        drop(state.tasks.spawn_blocking(move || {
            let _entered = span.enter();
            let result = operation().map_err(|error| UndeliveredFailure {
                error: Some(error),
                log,
            });
            drop(sender.send(result));
            Ok(())
        }));
        drop(state);
        Ok(Some(receiver))
    }

    async fn receive<T>(&self, receiver: StageReceiver<T>) -> Result<T> {
        let result = receiver
            .await
            .context("owned blocking stage ended without delivering its result")?;
        let result = match result {
            Ok(value) => Ok(value),
            Err(mut failure) => Err(failure
                .error
                .take()
                .context("blocking failure missing its cause")?),
        };
        self.state()?.reap();
        result
    }

    async fn join_next(&self, only_when_full: bool) -> Result<()> {
        poll_fn(|context| {
            let mut state = match self.state() {
                Ok(state) => state,
                Err(error) => return Poll::Ready(Err(error)),
            };
            if only_when_full && (state.stopping || state.tasks.len() < MAX_TASKS) {
                return Poll::Ready(Ok(()));
            }
            let result = match state.tasks.poll_join_next(context) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Some(result)) => {
                    state.observe(result);
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(None) => Poll::Ready(Ok(())),
            };
            drop(state);
            result
        })
        .await
    }

    #[tracing::instrument(skip_all)]
    pub async fn drain(&self) -> Result<()> {
        tokio::runtime::Handle::try_current()
            .context("owned blocking stage drain requires a Tokio runtime")?;
        {
            let mut state = self.state()?;
            state.stopping = true;
            state.reap();
        }
        let drain = async {
            let _admission = self.admission.lock().await;
            stream::iter((0..MAX_TASKS).map(Ok::<_, anyhow::Error>))
                .try_for_each(|_| self.join_next(false))
                .await
        };
        if let Ok(result) = tokio::time::timeout(DRAIN_BUDGET, drain).await {
            result?;
        } else {
            let mut state = self.state()?;
            state.reap();
            let outstanding = state.tasks.len();
            drop(state);
            return Err(anyhow!(
                "owned blocking stage drain exceeded 90 seconds; {outstanding} stages still owned; running blocking work cannot be cancelled"
            ));
        }
        self.state()?.outcome()
    }
}

impl Drop for BlockingStages {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            state.reap();
            if !state.tasks.is_empty() {
                tracing::error!(
                    outstanding = state.tasks.len(),
                    "blocking stage owner dropped before successful drain; running blocking work cannot be cancelled"
                );
            }
        } else {
            tracing::error!("poisoned blocking stage owner dropped; task completion is unverified");
        }
    }
}

#[cfg(test)]
#[path = "blocking_tests.rs"]
mod tests;
