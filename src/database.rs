use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::model::{AthleteRecord, Capture, Counts, ParsedPage, Source, Target};

mod contacts;
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
    Bulk,
}

impl JobClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Roster => "roster",
            Self::Bulk => "bulk",
        }
    }
}

impl Database {
    #[tracing::instrument(skip(url))]
    pub async fn connect(url: &str) -> Result<Self> {
        if !url.starts_with("postgres://") && !url.starts_with("postgresql://") {
            bail!("DATABASE_URL must use postgres:// or postgresql://");
        }
        let options = PgConnectOptions::from_str(url).context("parsing PostgreSQL DATABASE_URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .context("connecting to PostgreSQL")?;
        if let Err(error) = sqlx::migrate!("./migrations").run(&pool).await {
            pool.close().await;
            return Err(error).context("applying PostgreSQL scraper migrations");
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

    #[tracing::instrument(skip(self))]
    pub async fn next_job(&self, source: Option<Source>, max_attempts: u32) -> Result<Option<Job>> {
        self.next_job_with_priority(source, max_attempts, JobClass::Profile)
            .await
    }

    #[tracing::instrument(skip(self))]
    pub async fn next_job_with_priority(
        &self,
        source: Option<Source>,
        max_attempts: u32,
        preferred: JobClass,
    ) -> Result<Option<Job>> {
        queue::next(&self.pool, source, max_attempts, preferred).await
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
        let result = queue::fail(&mut transaction, job, capture, detail, blocked).await;
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
        report::athlete_records(&self.pool, states, years).await
    }

    pub async fn export_snapshot(
        &self,
        states: &[String],
        years: &[i32],
    ) -> Result<(Vec<AthleteRecord>, Vec<crate::export::ExportIssue>)> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await?;
        let records: Vec<serde_json::Value> =
            sqlx::query_scalar(include_str!("database/athlete_records.sql"))
                .bind(states)
                .bind(years)
                .fetch_all(&mut *transaction)
                .await
                .context("loading selected canonical athletes with source records")?;
        let records = records
            .into_iter()
            .map(|record| {
                serde_json::from_value(record).context("decoding canonical athlete report")
            })
            .collect::<Result<Vec<_>>>()?;
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT source, url, detail FROM ( \
             SELECT source, url, concat_ws(': ', 'page ' || status, last_error) AS detail \
             FROM scrape_pages WHERE status <> 'succeeded' \
             UNION ALL \
             SELECT source, url, 'page note: ' || note FROM scrape_pages CROSS JOIN LATERAL unnest(notes) AS note \
             UNION ALL \
             SELECT source, source_url, concat_ws(' | ', 'unassigned result', event, raw_mark, status) \
             FROM results WHERE athlete_id IS NULL \
             ) issues ORDER BY source, url, detail",
        )
        .fetch_all(&mut *transaction)
        .await
        .context("loading page coverage and unassigned result issues")?;
        transaction.commit().await?;
        let issues = rows
            .into_iter()
            .map(|(source, url, detail)| crate::export::ExportIssue {
                source,
                url,
                detail,
            })
            .collect();
        Ok((records, issues))
    }
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
