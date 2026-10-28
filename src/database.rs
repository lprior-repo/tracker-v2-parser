use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::model::{AthleteRecord, Capture, Counts, ParsedPage, Source, Target};

mod acquisition_scope;
mod contact_discovery;
mod contacts;
mod export_snapshot;
mod identity;
mod keys;
mod queue;
mod records;
mod report;
mod results;
mod schools;
mod unassigned;

#[cfg(test)]
mod tests;

pub use export_snapshot::{
    EXPORT_BATCH_BYTES, EXPORT_BATCH_ROWS, EXPORT_COLLECTION_BYTES, EXPORT_COLLECTION_ROWS,
    EXPORT_COMPONENT_BYTES, EXPORT_MAX_OBSERVATIONS, ExportLimitExceeded, ExportSnapshot,
    ExportSnapshotInvalidated,
};

#[derive(Debug)]
pub struct Database {
    pool: PgPool,
}

#[derive(Debug)]
pub struct Job {
    pub id: i64,
    pub target: Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobClass {
    Profile,
    Roster,
    Contacts,
    Bulk,
}

impl JobClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Roster => "roster",
            Self::Contacts => "contacts",
            Self::Bulk => "bulk",
        }
    }
}

impl Database {
    #[tracing::instrument(skip(url))]
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = connect_pool(url).await?;
        if let Err(error) = sqlx::migrate!("./migrations").run(&pool).await {
            pool.close().await;
            return Err(error).context("applying PostgreSQL scraper migrations");
        }
        Ok(Self { pool })
    }

    #[tracing::instrument(skip(url))]
    pub async fn connect_for_export(url: &str) -> Result<Self> {
        let pool = connect_pool(url).await?;
        if let Err(error) = export_snapshot::validate_schema(&pool).await {
            pool.close().await;
            return Err(error)
                .context("export requires a compatible schema; run mutating setup separately");
        }
        Ok(Self { pool })
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    #[tracing::instrument(skip(self))]
    pub async fn check(&self) -> Result<String> {
        sqlx::query_scalar("SELECT version()")
            .fetch_one(&self.pool)
            .await
            .context("reading PostgreSQL server version")
    }

    #[tracing::instrument(skip(self))]
    pub async fn close(self) {
        self.pool.close().await;
    }

    #[tracing::instrument(skip(self, targets))]
    pub async fn enqueue(&self, targets: &[Target], refresh: bool) -> Result<u64> {
        let mut transaction = self.pool.begin().await?;
        let result = queue::enqueue(&mut transaction, targets, refresh).await;
        finish(transaction, result).await
    }

    #[tracing::instrument(skip(self, states, sources))]
    pub async fn backfill_contacts(
        &self,
        states: &[String],
        sources: &[Source],
        season: i32,
    ) -> Result<u64> {
        contact_discovery::backfill(&self.pool, states, sources, season).await
    }

    #[tracing::instrument(skip(self, states))]
    pub async fn next_job(
        &self,
        source: Option<Source>,
        max_attempts: u32,
        states: &[String],
    ) -> Result<Option<Job>> {
        self.next_job_with_priority(source, max_attempts, JobClass::Profile, states)
            .await
    }

    #[tracing::instrument(skip(self, states))]
    pub async fn next_job_with_priority(
        &self,
        source: Option<Source>,
        max_attempts: u32,
        preferred: JobClass,
        states: &[String],
    ) -> Result<Option<Job>> {
        queue::next(&self.pool, source, max_attempts, preferred, states).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn start_attempt(&self, page_id: i64, max_attempts: u32) -> Result<bool> {
        queue::start_attempt(&self.pool, page_id, max_attempts).await
    }

    #[tracing::instrument(skip(self, target, capture, page))]
    pub async fn persist(
        &self,
        job_id: Option<i64>,
        target: &Target,
        capture: &Capture,
        page: &ParsedPage,
    ) -> Result<i64> {
        let mut transaction = self.pool.begin().await?;
        let result = records::persist(&mut transaction, job_id, target, capture, page).await;
        finish(transaction, result).await
    }

    #[tracing::instrument(skip(self, job, capture, detail))]
    pub async fn fail_job(
        &self,
        job: &Job,
        capture: Option<&Capture>,
        detail: &str,
        blocked: bool,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        let result = queue::fail(&mut transaction, job, capture, detail, blocked, None).await;
        finish(transaction, result).await
    }

    #[tracing::instrument(skip_all)]
    pub async fn fail_acquisition(
        &self,
        job: &Job,
        failure: &crate::transport::Failure,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        let result = queue::fail(
            &mut transaction,
            job,
            failure.capture.as_ref(),
            &failure.detail,
            failure.blocked,
            failure.stopped_origin.as_deref(),
        )
        .await;
        finish(transaction, result).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn counts(&self) -> Result<Counts> {
        report::counts(&self.pool).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn athlete_records(&self) -> Result<Vec<AthleteRecord>> {
        self.athlete_records_for(&[], &[]).await
    }

    #[tracing::instrument(skip(self, states, years))]
    pub async fn athlete_records_for(
        &self,
        states: &[String],
        years: &[i32],
    ) -> Result<Vec<AthleteRecord>> {
        let (records, _) = self.export_snapshot(states, years).await?;
        Ok(records)
    }

    #[tracing::instrument(skip(self, states, years))]
    pub async fn export_snapshot(
        &self,
        states: &[String],
        years: &[i32],
    ) -> Result<(Vec<AthleteRecord>, Vec<crate::export::ExportIssue>)> {
        let snapshot = self.begin_export_snapshot(states, years).await?;
        export_snapshot::collect(snapshot).await
    }

    #[tracing::instrument(skip(self, states, years))]
    pub async fn begin_export_snapshot(
        &self,
        states: &[String],
        years: &[i32],
    ) -> Result<ExportSnapshot<'_>> {
        ExportSnapshot::begin(&self.pool, states, years).await
    }

    #[tracing::instrument(skip(self, options))]
    pub async fn write_export(
        &self,
        output: &std::path::Path,
        options: &crate::export::ExportOptions,
    ) -> Result<Vec<std::path::PathBuf>> {
        self.write_export_with_layout(output, options, crate::export::ExportLayout::Detailed)
            .await
    }

    #[tracing::instrument(skip(self, options))]
    pub async fn write_export_with_layout(
        &self,
        output: &std::path::Path,
        options: &crate::export::ExportOptions,
        layout: crate::export::ExportLayout,
    ) -> Result<Vec<std::path::PathBuf>> {
        let snapshot = self
            .begin_export_snapshot(&options.states, &options.graduation_years)
            .await?;
        export_snapshot::write(snapshot, output, options, layout).await
    }
}

#[tracing::instrument(skip(url))]
async fn connect_pool(url: &str) -> Result<PgPool> {
    if !url.starts_with("postgres://") && !url.starts_with("postgresql://") {
        bail!("DATABASE_URL must use postgres:// or postgresql://");
    }
    let options = PgConnectOptions::from_str(url).context("parsing PostgreSQL DATABASE_URL")?;
    PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .context("connecting to PostgreSQL")
}

#[tracing::instrument(skip(transaction, result))]
async fn finish<T>(
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    result: Result<T>,
) -> Result<T> {
    match result {
        Ok(value) => {
            transaction
                .commit()
                .await
                .context("committing scraper transaction")?;
            Ok(value)
        }
        Err(error) => {
            transaction
                .rollback()
                .await
                .with_context(|| format!("rolling back after: {error:#}"))?;
            Err(error)
        }
    }
}
