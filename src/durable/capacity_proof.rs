use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use restate_sdk::prelude::*;
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet};

use super::{AthleticOperations, blocking::BlockingStages, runtime::Runtime};
use crate::database::Database;

pub struct Worker {
    runtime: Arc<Runtime>,
    endpoint: String,
    releases: Vec<mpsc::Sender<()>>,
    cancel: Option<oneshot::Sender<()>>,
    tasks: JoinSet<Result<()>>,
}

impl Worker {
    #[tracing::instrument(skip_all)]
    pub async fn start(database: &str, artifacts: &Path, delivered: &Path) -> Result<Self> {
        let url = url::Url::parse(database)?;
        ensure!(
            url.query_pairs().any(|(key, value)| key == "options"
                && value
                    .strip_prefix("-csearch_path=")
                    .is_some_and(|schema| schema.starts_with("restate_e2e_")
                        && schema
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))),
            "capacity proof requires an isolated recovery schema"
        );
        ensure!(
            artifacts.is_absolute() && delivered.is_absolute(),
            "capacity proof paths must be absolute"
        );
        tokio::fs::create_dir(artifacts).await?;
        let runtime = Arc::new(Runtime {
            database: Database::connect(database).await?,
            database_url: database.to_owned(),
            artifact_root: tokio::fs::canonicalize(artifacts).await?,
            blocking: BlockingStages::new(),
            published_workbook: Some(delivered.to_owned()),
        });
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let endpoint_service = Endpoint::builder()
            .bind(AthleticOperations {
                runtime: runtime.clone(),
            })
            .build();
        let (cancel, cancelled) = oneshot::channel();
        let mut tasks = JoinSet::new();
        drop(tasks.spawn(async move {
            HttpServer::new(endpoint_service)
                .serve_with_cancel(listener, async move {
                    drop(cancelled.await);
                })
                .await;
            Ok(())
        }));
        let mut worker = Self {
            runtime,
            endpoint,
            releases: Vec::new(),
            cancel: Some(cancel),
            tasks,
        };
        if let Err(error) = worker.saturate().await {
            return match worker.stop().await {
                Ok(()) => Err(error),
                Err(cleanup) => {
                    Err(error.context(format!("capacity proof cleanup failed: {cleanup:#}")))
                }
            };
        }
        Ok(worker)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn saturate(&mut self) -> Result<()> {
        let ready = (0..8)
            .map(|_| {
                let (release, wait) = mpsc::channel();
                let (started, ready) = oneshot::channel();
                self.releases.push(release);
                let runtime = self.runtime.clone();
                drop(self.tasks.spawn(async move {
                    runtime
                        .blocking
                        .run(move || {
                            started.send(()).map_err(|()| {
                                anyhow!("capacity proof readiness receiver dropped")
                            })?;
                            wait.recv()
                                .context("capacity proof release sender dropped")?;
                            Ok(())
                        })
                        .await
                }));
                ready
            })
            .collect::<Vec<_>>();
        tokio::time::timeout(
            Duration::from_secs(10),
            stream::iter(ready.into_iter().map(Ok::<_, anyhow::Error>)).try_for_each(
                |ready| async { ready.await.context("capacity proof task never admitted") },
            ),
        )
        .await
        .context("capacity proof saturation timed out")??;
        Ok(())
    }

    pub fn release(&mut self) -> Result<()> {
        std::mem::take(&mut self.releases)
            .into_iter()
            .try_for_each(|release| {
                release
                    .send(())
                    .context("capacity proof worker ended before release")
            })
    }

    #[tracing::instrument(skip_all)]
    pub async fn stop(mut self) -> Result<()> {
        let released = self.release();
        let cancelled = self.cancel.take().map_or(Ok(()), |cancel| {
            cancel
                .send(())
                .map_err(|()| anyhow!("capacity proof HTTP server ended before cancellation"))
        });
        let joined = tokio::time::timeout(
            Duration::from_secs(30),
            stream::iter(0..9).fold((self.tasks, None), |(mut tasks, failure), _| async move {
                let observed = tasks.join_next().await.map_or_else(
                    || Ok(()),
                    |result| {
                        result
                            .context("capacity proof task join failed")
                            .and_then(std::convert::identity)
                    },
                );
                (tasks, failure.or_else(|| observed.err()))
            }),
        )
        .await
        .context("capacity proof task shutdown exceeded bound");
        let drained = self.runtime.blocking.drain().await;
        self.runtime.database.pool().close().await;
        released?;
        cancelled?;
        let (tasks, failure) = joined?;
        ensure!(tasks.is_empty(), "capacity proof tasks remain running");
        if let Some(error) = failure {
            return Err(error);
        }
        drained
    }
}
