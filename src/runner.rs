use std::path::Path;

use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};

pub mod durable;
mod replay;

use crate::{config::Config, database::Database, model::Target, sources};
use replay::replay_page;

#[derive(Default)]
pub struct Runner;

impl Runner {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn replay(
        &self,
        config: &Config,
        database_url: &str,
        input: &Path,
        target: Target,
        evidence_root: &Path,
    ) -> Result<()> {
        let mut config = config.clone();
        config.validate()?;
        ensure!(
            evidence_root.is_absolute(),
            "offline evidence root must be absolute"
        );
        let database = Database::connect(database_url).await?;
        let result = self
            .replay_locked(&database, &config, input, target, evidence_root)
            .await;
        database.close().await;
        result
    }

    async fn replay_locked(
        &self,
        database: &Database,
        config: &Config,
        input: &Path,
        target: Target,
        evidence_root: &Path,
    ) -> Result<()> {
        let mut connection = database
            .pool()
            .acquire()
            .await
            .context("acquiring scraper lock connection")?;
        let locked: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock(hashtext(current_schema()), 194832741)",
        )
        .fetch_one(&mut *connection)
        .await
        .context("acquiring scraper advisory lock")?;
        ensure!(locked, "another scraper owns the acquisition session");
        let result = replay_page(database, config, input, target, evidence_root).await;
        let unlock: Result<bool> =
            sqlx::query_scalar("SELECT pg_advisory_unlock(hashtext(current_schema()), 194832741)")
                .fetch_one(&mut *connection)
                .await
                .context("releasing scraper advisory lock");
        finish_lock(result, unlock)
    }

    pub async fn export(&self, config: &Config, output: &Path, database_url: &str) -> Result<()> {
        let mut config = config.clone();
        config.validate()?;
        let database = Database::connect_for_export(database_url).await?;
        let result = export_records(&database, &config, output).await;
        database.close().await;
        result
    }

    fn discover_targets(config: &Config) -> Result<Vec<Target>> {
        let targets = config.enabled_sources().into_iter().try_fold(
            Vec::new(),
            |mut targets, source| -> Result<_> {
                targets.extend(sources::seeds(source, &config.states, config.season)?);
                Ok(targets)
            },
        )?;
        ensure!(
            !targets.is_empty(),
            "no source targets discovered from configuration"
        );
        Ok(targets)
    }
}

async fn check_source_stops(database: &Database, config: &Config) -> Result<()> {
    stream::iter(config.enabled_sources().into_iter().map(Ok::<_, anyhow::Error>)).try_for_each(|source| async move {
        let stops: Vec<(String, i32, serde_json::Value,Option<String>,bool)> = sqlx::query_as(
            "SELECT p.url,p.attempts,p.cursor,p.last_error,NOT EXISTS(SELECT 1 FROM durable_page_operations d JOIN durable_page_attempts a USING(operation_id) WHERE d.page_id=p.id AND (a.capture IS NOT NULL OR a.parsed IS NOT NULL)) AND NOT EXISTS(SELECT 1 FROM durable_page_receipts r WHERE r.page_id=p.id) AND NOT EXISTS(SELECT 1 FROM durable_page_operations d JOIN durable_wire_requests w USING(operation_id) WHERE d.page_id=p.id) FROM scrape_pages p WHERE p.source=$1 AND p.status='blocked' AND p.cursor->>'kind' <> 'contacts' ORDER BY p.id LIMIT 1001",
        )
        .bind(source.as_str())
        .fetch_all(database.pool())
        .await
        .context("checking source access stops")?;
        ensure!(
            stops.len() <= 1000,
            "too many source stops; human review required"
        );
        stops
            .iter()
            .try_for_each(|(address, attempts, cursor,detail,no_remote_evidence)| -> Result<()> {
                ensure!(
                    config
                        .acquisition
                        .permits_evidenced_stop(&crate::acquisition::PreservedStop {source,address,attempts:*attempts,cursor,detail:detail.as_deref(),no_remote_evidence:*no_remote_evidence})?,
                    "{} has a blocked page requiring human review",
                    source.as_str()
                );
                Ok(())
            })
    }).await
}

async fn export_records(database: &Database, config: &Config, output: &Path) -> Result<()> {
    database
        .write_export_with_review(
            output,
            &crate::export::ExportOptions {
                states: config.states.clone(),
                graduation_years: config.graduation_years.clone(),
                rows_per_file: config.export.rows_per_file,
                per_athlete_sheets: config.export.per_athlete_sheets,
            },
            config.export.layout,
            config.export.review_sheet,
        )
        .await?;
    Ok(())
}

fn finish_lock(result: Result<()>, unlock: Result<bool>) -> Result<()> {
    let cleanup = unlock.and_then(|released| {
        ensure!(
            released,
            "scraper advisory lock was not held during cleanup"
        );
        Ok(())
    });
    combine(result, cleanup)
}

fn combine(result: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("cleanup also failed: {cleanup:#}")))
        }
    }
}
