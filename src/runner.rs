use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::{
    config::{Config, TransportMode},
    database::{Database, Job},
    model::{Capture, Source, Target},
    sources,
    transport::{MAX_BODY_BYTES, Mode, Options, Transport, validate_target},
};

#[derive(Default)]
pub struct Runner;

impl Runner {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn scrape(&self, config: &Config, database_url: &str) -> Result<()> {
        self.scrape_with_refresh(config, database_url, false).await
    }

    pub async fn scrape_with_refresh(
        &self,
        config: &Config,
        database_url: &str,
        refresh: bool,
    ) -> Result<()> {
        let mut config = config.clone();
        config.validate()?;
        let database = Database::connect(database_url).await?;
        let result = self.scrape_locked(&database, &config, refresh).await;
        database.close().await;
        result
    }

    async fn scrape_locked(
        &self,
        database: &Database,
        config: &Config,
        refresh: bool,
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
        let result = self.scrape_session(database, config, refresh).await;
        let unlock: Result<bool> =
            sqlx::query_scalar("SELECT pg_advisory_unlock(hashtext(current_schema()), 194832741)")
                .fetch_one(&mut *connection)
                .await
                .context("releasing scraper advisory lock");
        finish_lock(result, unlock)
    }

    async fn scrape_session(
        &self,
        database: &Database,
        config: &Config,
        refresh: bool,
    ) -> Result<()> {
        let targets = Self::discover_targets(config)?;
        targets.iter().try_for_each(validate_target)?;
        database.enqueue(&targets, refresh).await?;
        let mut athleticnet = make_transport(config, Source::AthleticNet)?;
        let mut milesplit = make_transport(config, Source::MileSplit)?;
        let result = self
            .run_jobs(database, config, &mut athleticnet, &mut milesplit)
            .await;
        let first = athleticnet
            .close()
            .await
            .context("closing Athletic.net transport");
        let second = milesplit
            .close()
            .await
            .context("closing MileSplit transport");
        combine(combine(result, first), second)
    }

    async fn run_jobs(
        &self,
        database: &Database,
        config: &Config,
        athleticnet: &mut Transport,
        milesplit: &mut Transport,
    ) -> Result<()> {
        for _ in 0..config.limits.max_pages {
            let job = next_enabled_job(database, config).await?;
            let Some(job) = job else { break };
            let transport = match job.target.source {
                Source::AthleticNet => &mut *athleticnet,
                Source::MileSplit => &mut *milesplit,
            };
            acquire_job(database, config, &job, transport).await?;
        }
        check_source_stops(database, config).await?;
        Ok(())
    }

    pub async fn replay(
        &self,
        config: &Config,
        database_url: &str,
        input: &Path,
        target: Target,
    ) -> Result<()> {
        let mut config = config.clone();
        config.validate()?;
        let database = Database::connect(database_url).await?;
        let result = self.replay_locked(&database, &config, input, target).await;
        database.close().await;
        result
    }

    async fn replay_locked(
        &self,
        database: &Database,
        config: &Config,
        input: &Path,
        target: Target,
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
        let result = replay_page(database, config, input, target).await;
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
        let database = Database::connect(database_url).await?;
        let result = export_records(&database, &config, output).await;
        database.close().await;
        result
    }

    fn discover_targets(config: &Config) -> Result<Vec<Target>> {
        let mut targets = Vec::new();
        for source in config.enabled_sources() {
            targets.extend(sources::seeds(source, &config.states, config.season)?);
        }
        ensure!(
            !targets.is_empty(),
            "no source targets discovered from configuration"
        );
        Ok(targets)
    }
}

async fn acquire_job(
    database: &Database,
    config: &Config,
    job: &Job,
    transport: &mut Transport,
) -> Result<()> {
    for _ in 0..config.limits.max_attempts {
        ensure!(
            database
                .start_attempt(job.id, config.limits.max_attempts)
                .await?,
            "checkpoint {} has exhausted its attempts",
            job.id
        );
        info!("acquiring {}", job.target.url);
        match transport.fetch(&job.target).await {
            Ok(capture) => return persist_capture(database, job, &capture).await,
            Err(failure) => {
                let error = anyhow!(
                    "acquisition failed for {}: {}",
                    job.target.url,
                    failure.detail
                );
                let detail = format!("{error:#}");
                database
                    .fail_job(job, failure.capture.as_ref(), &detail, failure.blocked)
                    .await
                    .with_context(|| format!("recording page failure after: {detail}"))?;
                if failure.blocked {
                    return Err(error);
                }
            }
        }
    }
    bail!("checkpoint {} has exhausted its attempts", job.id)
}

fn make_transport(config: &Config, source: Source) -> Result<Transport> {
    let mode = match config.settings(source).transport {
        TransportMode::Http => Mode::Http,
        TransportMode::Browser => Mode::Browser,
        TransportMode::Auto => Mode::Auto,
    };
    Transport::new(Options {
        mode,
        driver_url: None,
        profile: PathBuf::from(format!("/tmp/track-scrapper-profile-{}", source.as_str())),
        headless: true,
        rps: config.limits.requests_per_second,
    })
}

async fn next_enabled_job(database: &Database, config: &Config) -> Result<Option<Job>> {
    check_source_stops(database, config).await?;
    for source in config.enabled_sources() {
        if let Some(job) = database
            .next_job(Some(source), config.limits.max_attempts)
            .await?
        {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

async fn check_source_stops(database: &Database, config: &Config) -> Result<()> {
    for source in config.enabled_sources() {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM scrape_pages WHERE source=$1 AND status='blocked')",
        )
        .bind(source.as_str())
        .fetch_one(database.pool())
        .await
        .context("checking source access stops")?;
        ensure!(
            !blocked,
            "{} has a blocked page requiring human review",
            source.as_str()
        );
        let exhausted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM scrape_pages WHERE source=$1 AND status IN ('pending','failed') AND attempts >= $2)",
        )
        .bind(source.as_str())
        .bind(i32::try_from(config.limits.max_attempts).context("converting maximum attempts")?)
        .fetch_one(database.pool())
        .await
        .context("checking exhausted checkpoints")?;
        ensure!(
            !exhausted,
            "{} has exhausted unfinished checkpoints",
            source.as_str()
        );
    }
    Ok(())
}

async fn persist_capture(database: &Database, job: &Job, capture: &Capture) -> Result<()> {
    let parsed = match std::str::from_utf8(&capture.body)
        .context("source response is not UTF-8")
        .and_then(|body| sources::parse(&job.target, body))
    {
        Ok(parsed) => parsed,
        Err(error) => {
            let blocked = error.is::<sources::AccessDenied>();
            return record_failure(
                database,
                job,
                Some(capture),
                blocked,
                error.context("parsing source page"),
            )
            .await;
        }
    };
    match database
        .persist(Some(job.id), &job.target, capture, &parsed)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            record_failure(
                database,
                job,
                Some(capture),
                false,
                error.context("persisting source page"),
            )
            .await
        }
    }
}

async fn record_failure(
    database: &Database,
    job: &Job,
    capture: Option<&Capture>,
    blocked: bool,
    error: anyhow::Error,
) -> Result<()> {
    let detail = format!("{error:#}");
    database
        .fail_job(job, capture, &detail, blocked)
        .await
        .with_context(|| format!("recording page failure after: {detail}"))?;
    Err(error)
}

async fn replay_page(
    database: &Database,
    config: &Config,
    input: &Path,
    target: Target,
) -> Result<()> {
    validate_target(&target)?;
    database
        .enqueue(std::slice::from_ref(&target), true)
        .await?;
    let cursor = serde_json::to_value(&target)?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM scrape_pages WHERE source=$1 AND cursor=$2")
        .bind(target.source.as_str())
        .bind(cursor)
        .fetch_one(database.pool())
        .await
        .context("finding replay checkpoint")?;
    let job = Job { id, target };
    ensure!(
        database
            .start_attempt(id, config.limits.max_attempts)
            .await?,
        "replay checkpoint could not start an attempt"
    );
    let body = match read_replay(input).await {
        Ok(body) => body,
        Err(error) => return record_failure(database, &job, None, false, error).await,
    };
    let capture = match crate::transport::redact_capture(Capture {
        url: job.target.url.clone(),
        status: 200,
        content_type: String::from("application/octet-stream"),
        body,
    }) {
        Ok(capture) => capture,
        Err(error) => return record_failure(database, &job, None, false, error).await,
    };
    persist_capture(database, &job, &capture).await
}

async fn read_replay(input: &Path) -> Result<Vec<u8>> {
    let file = tokio::fs::File::open(input)
        .await
        .with_context(|| format!("opening replay input {}", input.display()))?;
    let limit = u64::try_from(MAX_BODY_BYTES)
        .context("converting replay size limit")?
        .saturating_add(1);
    let mut body = Vec::new();
    file.take(limit)
        .read_to_end(&mut body)
        .await
        .context("reading replay input")?;
    ensure!(
        body.len() <= MAX_BODY_BYTES,
        "replay input exceeds the response size limit"
    );
    Ok(body)
}

async fn export_records(database: &Database, config: &Config, output: &Path) -> Result<()> {
    let (records, issues) = database
        .export_snapshot(&config.states, &config.graduation_years)
        .await?;
    crate::export::write_spreadsheets_with_issues(
        output,
        &records,
        &issues,
        &crate::export::ExportOptions {
            states: config.states.clone(),
            graduation_years: config.graduation_years.clone(),
            rows_per_file: config.export.rows_per_file,
            per_athlete_sheets: config.export.per_athlete_sheets,
        },
    )?;
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
