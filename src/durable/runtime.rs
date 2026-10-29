use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use restate_sdk::prelude::*;
use tokio::{
    net::TcpListener,
    signal::unix::{SignalKind, signal},
};

use crate::database::Database;

use super::services::{AthleticCrawl, AthleticOperations, AthleticPage};

pub(super) struct Runtime {
    pub database: Database,
    pub database_url: String,
    pub artifact_root: PathBuf,
    pub blocking: super::blocking::BlockingStages,
    pub published_workbook: Option<PathBuf>,
}

#[must_use]
pub fn serve(
    database_url: String,
    artifact_root: PathBuf,
    address: SocketAddr,
    published_workbook: Option<PathBuf>,
) -> futures::future::BoxFuture<'static, Result<()>> {
    Box::pin(serve_inner(
        database_url,
        artifact_root,
        address,
        published_workbook,
    ))
}

async fn serve_inner(
    database_url: String,
    artifact_root: PathBuf,
    address: SocketAddr,
    published_workbook: Option<PathBuf>,
) -> Result<()> {
    ensure!(
        address.ip().is_loopback(),
        "durable worker must bind loopback"
    );
    ensure!(
        artifact_root.is_absolute(),
        "durable artifact root must be absolute"
    );
    if let Some(path) = &published_workbook {
        ensure!(
            path.is_absolute()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "xlsx")
                && path.parent().is_some_and(std::path::Path::is_dir),
            "published workbook must be an absolute xlsx path in an existing directory"
        );
    }
    tokio::fs::create_dir_all(&artifact_root).await?;
    let artifact_root = tokio::fs::canonicalize(artifact_root).await?;
    let database = Database::connect(&database_url).await?;
    let runtime = Arc::new(Runtime {
        database,
        database_url,
        artifact_root,
        blocking: super::blocking::BlockingStages::new(),
        published_workbook,
    });
    let endpoint = Endpoint::builder()
        .bind(AthleticCrawl {
            runtime: runtime.clone(),
        })
        .bind(AthleticPage {
            runtime: runtime.clone(),
        })
        .bind(AthleticOperations {
            runtime: runtime.clone(),
        })
        .build();
    let listener = TcpListener::bind(address)
        .await
        .context("binding durable worker")?;
    let mut terminate = signal(SignalKind::terminate()).context("installing terminate handler")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("installing interrupt handler")?;
    tracing::info!(%address, "durable worker ready");
    HttpServer::new(endpoint)
        .serve_with_cancel(listener, async move {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        })
        .await;
    let drained = runtime.blocking.drain().await;
    runtime.database.pool().close().await;
    drained
}
