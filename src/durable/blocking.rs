use std::{
    fmt,
    sync::{Mutex, MutexGuard},
    task::Poll,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use futures::{TryStreamExt, future::poll_fn, stream};
use tokio::{
    sync::oneshot,
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

#[derive(Default)]
struct State {
    tasks: JoinSet<Result<()>>,
    failures: Vec<String>,
    failed: u64,
    stopping: bool,
}

impl State {
    fn observe(&mut self, result: std::result::Result<Result<()>, JoinError>) {
        let error = match result {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error,
            Err(error) => anyhow!(error).context("owned blocking stage join failed"),
        };
        self.failed = self.failed.saturating_add(1);
        tracing::error!(error = %error, "owned blocking stage failed");
        if self.failures.len() < MAX_TASKS {
            self.failures
                .push(format!("{error:#}").chars().take(2048).collect());
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
        ensure!(
            self.failed == 0,
            "{} owned blocking stages failed: {}",
            self.failed,
            self.failures.join("; ")
        );
        Ok(())
    }
}

#[derive(Default)]
pub struct BlockingStages {
    state: Mutex<State>,
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
        tokio::runtime::Handle::try_current()
            .context("owned blocking stages require a Tokio runtime")?;
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.state()?;
            state.reap();
            if state.stopping {
                return Err(BlockingStageUnavailable::Stopping.into());
            }
            if state.tasks.len() >= MAX_TASKS {
                return Err(BlockingStageUnavailable::Busy.into());
            }
            drop(state.tasks.spawn_blocking(move || {
                let result = operation();
                let error = result
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}").chars().take(2048).collect::<String>());
                drop(sender.send(result));
                error.map_or(Ok(()), |error| Err(anyhow!(error)))
            }));
        }
        let result = receiver
            .await
            .context("owned blocking stage ended without delivering its result")?;
        self.state()?.reap();
        result
    }

    async fn join_next(&self) -> Result<()> {
        poll_fn(|context| {
            let mut state = match self.state() {
                Ok(state) => state,
                Err(error) => return Poll::Ready(Err(error)),
            };
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
        let drain = stream::iter((0..MAX_TASKS).map(Ok::<_, anyhow::Error>))
            .try_for_each(|_| self.join_next());
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
