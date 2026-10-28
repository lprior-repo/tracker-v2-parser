mod evidence;
mod preparation;
mod proof;
mod schedule;

use std::path::Path;

use anyhow::{Context, Result, ensure};
use futures::future::BoxFuture;
use futures::{TryStreamExt, stream};
use serde::Serialize;

use crate::database::Database;

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub associations_imported: usize,
    pub coaches_imported: usize,
    pub programs_imported: usize,
    pub unconfirmed_programs: Vec<String>,
    pub captures_validated: usize,
    pub manifest_sha256: String,
}

#[must_use]
pub fn import<'a>(database_url: &'a str, manifest_path: &'a Path) -> BoxFuture<'a, Result<Report>> {
    Box::pin(async move {
        let path = manifest_path.to_path_buf();
        let (pages, report) = tokio::task::spawn_blocking(move || preparation::prepare(&path))
            .await
            .context("joining bounded co-op evidence validation")??;
        let database = Database::connect(database_url).await?;
        let result = locked(&database, &pages).await;
        database.close().await;
        result?;
        Ok(report)
    })
}

async fn locked(database: &Database, pages: &[preparation::Prepared]) -> Result<()> {
    let mut transaction = database.pool().begin().await?;
    let acquired: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtext(current_schema()),194832741)",
    )
    .fetch_one(&mut *transaction)
    .await?;
    ensure!(acquired, "another scraper owns the acquisition session");
    let result = persist(database, pages).await;
    let cleanup = transaction
        .rollback()
        .await
        .context("releasing co-op import lock");
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("cleanup also failed: {cleanup:#}")))
        }
    }
}

async fn persist(database: &Database, pages: &[preparation::Prepared]) -> Result<()> {
    let stopped: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM scrape_pages p CROSS JOIN LATERAL unnest(ARRAY[p.url,p.cursor->>'access_stop_url']) AS stop(url) WHERE p.status='blocked' AND p.cursor->>'kind'='contacts' AND lower(substring(stop.url from '(?i)^https?://([^/?#]+)')) IN ('schools.wiaawi.org','schools.wiaawi.org:443','schools.wiaawi.org:80'))"
    ).fetch_one(database.pool()).await?;
    ensure!(
        !stopped,
        "WIAA contact authority has a preserved access stop requiring human review"
    );
    stream::iter(pages.iter().map(Ok::<_, anyhow::Error>))
        .try_for_each(|page| async move {
            let identity: Option<(String, Option<String>)> =
                sqlx::query_as("SELECT name,state FROM schools WHERE source=$1 AND source_id=$2")
                    .bind(page.source.as_str())
                    .bind(&page.school_id)
                    .fetch_optional(database.pool())
                    .await?;
            let (name, state) =
                identity.context("co-op provider identity is absent from database")?;
            ensure!(
                proof::normalized(&name) == proof::normalized(&page.school_name)
                    && state.as_deref() == Some("WI"),
                "co-op manifest conflicts with existing provider school identity"
            );
            Ok(())
        })
        .await?;
    stream::iter(pages.iter().map(Ok::<_, anyhow::Error>))
        .try_for_each(|page| async move {
            database
                .persist(None, &page.target, &page.capture, &page.page)
                .await?;
            Ok(())
        })
        .await
}
