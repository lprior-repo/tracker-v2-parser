use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, ensure};
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::{
    config::{Config, TransportMode},
    database::{Database, Job, JobClass},
    model::{Capture, Source, Target},
    sources,
    transport::{MAX_BODY_BYTES, Mode, Options, Transport, validate_target},
};

#[derive(Default)]
pub struct Runner;

struct Schedule {
    athleticnet: u8,
    milesplit: u8,
    next_source: Source,
}

impl Schedule {
    const fn new() -> Self {
        Self {
            athleticnet: 0,
            milesplit: 0,
            next_source: Source::AthleticNet,
        }
    }

    const fn preferred(&self, source: Source) -> JobClass {
        let slot = match source {
            Source::AthleticNet => self.athleticnet,
            Source::MileSplit => self.milesplit,
        };
        match slot {
            3 => JobClass::Roster,
            6 => JobClass::Contacts,
            7 => JobClass::Bulk,
            _ => JobClass::Profile,
        }
    }

    const fn reserved(&mut self, source: Source) {
        let count = match source {
            Source::AthleticNet => &mut self.athleticnet,
            Source::MileSplit => &mut self.milesplit,
        };
        *count = if *count == 7 {
            0
        } else {
            count.saturating_add(1)
        };
        self.next_source = match source {
            Source::AthleticNet => Source::MileSplit,
            Source::MileSplit => Source::AthleticNet,
        };
    }
}

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
        if config.acquisition.includes_contacts() {
            database
                .backfill_contacts(&config.states, &config.enabled_sources(), config.season)
                .await?;
        }
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
        let mut schedule = Schedule::new();
        for _ in 0..config.limits.max_pages {
            let job = next_enabled_job(database, config, &schedule).await?;
            let Some(job) = job else { break };
            if let Some(reason) = config.acquisition.exclusion_reason(&job.target) {
                database.exclude_unattempted(job.id, reason).await?;
                continue;
            }
            ensure!(
                database
                    .start_attempt(job.id, config.limits.max_attempts)
                    .await?,
                "checkpoint {} has exhausted its attempts",
                job.id
            );
            schedule.reserved(job.target.source);
            let transport = match job.target.source {
                Source::AthleticNet => &mut *athleticnet,
                Source::MileSplit => &mut *milesplit,
            };
            acquire_job(database, &job, transport).await?;
        }
        ensure!(
            next_enabled_job(database, config, &schedule)
                .await?
                .is_none(),
            "page budget exhausted; unfinished checkpoints remain"
        );
        check_exhausted(database, config).await
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
        let database = Database::connect_for_export(database_url).await?;
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

async fn acquire_job(database: &Database, job: &Job, transport: &mut Transport) -> Result<()> {
    info!("acquiring {}", job.target.url);
    match transport.fetch(&job.target).await {
        Ok(capture) => {
            if let Some(error) = persist_capture(database, job, capture).await? {
                tracing::warn!("page {} failed: {error:#}", job.target.url);
            }
            Ok(())
        }
        Err(failure) => {
            let error = anyhow!(
                "acquisition failed for {}: {}",
                job.target.url,
                failure.detail
            );
            let detail = format!("{error:#}");
            database
                .fail_acquisition(job, &failure)
                .await
                .with_context(|| format!("recording page failure after: {detail}"))?;
            if failure.blocked && job.target.kind != crate::model::PageKind::Contacts {
                return Err(error);
            }
            tracing::warn!("{detail}");
            Ok(())
        }
    }
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

async fn next_enabled_job(
    database: &Database,
    config: &Config,
    schedule: &Schedule,
) -> Result<Option<Job>> {
    check_source_stops(database, config).await?;
    let mut sources = config.enabled_sources();
    sources.sort_by_key(|source| *source != schedule.next_source);
    for source in sources {
        if let Some(job) = database
            .next_job_with_priority(
                Some(source),
                config.limits.max_attempts,
                schedule.preferred(source),
                &config.states,
            )
            .await?
        {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

async fn check_source_stops(database: &Database, config: &Config) -> Result<()> {
    for source in config.enabled_sources() {
        let stops: Vec<(String, i32, serde_json::Value)> = sqlx::query_as(
            "SELECT url,attempts,cursor FROM scrape_pages WHERE source=$1 AND status='blocked' AND cursor->>'kind' <> 'contacts' ORDER BY id LIMIT 1001",
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
            .try_for_each(|(address, attempts, cursor)| -> Result<()> {
                ensure!(
                    config
                        .acquisition
                        .permits_preserved_stop(address, *attempts, cursor)?,
                    "{} has a blocked page requiring human review",
                    source.as_str()
                );
                Ok(())
            })?;
    }
    Ok(())
}

async fn check_exhausted(database: &Database, config: &Config) -> Result<()> {
    for source in config.enabled_sources() {
        let exhausted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM scrape_pages p WHERE source=$1 AND status IN ('pending','failed') AND attempts >= $2 \
             AND (cardinality($3::text[]) = 0 OR upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state')))=ANY($3) \
               OR (p.source='athleticnet' AND p.cursor->>'kind'='directory' \
                 AND p.cursor->'context'->>'state' IS NULL \
                 AND p.cursor->'context'->>'subject_id'=array_to_string($3,','))))",
        )
        .bind(source.as_str())
        .bind(i32::try_from(config.limits.max_attempts).context("converting maximum attempts")?)
        .bind(&config.states)
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

async fn persist_capture(
    database: &Database,
    job: &Job,
    capture: Capture,
) -> Result<Option<anyhow::Error>> {
    let (capture, retained) = match retain_profile_capture(job, capture).await {
        Ok(retained) => retained,
        Err(error) => {
            return record_failure(
                database,
                job,
                None,
                false,
                error.context("retaining profile evidence"),
            )
            .await
            .map(|()| None);
        }
    };
    let parse_target =
        if job.target.kind == crate::model::PageKind::Contacts && job.target.url != capture.url {
            let mut target = job.target.clone();
            target.url.clone_from(&capture.url);
            Cow::Owned(target)
        } else {
            Cow::Borrowed(&job.target)
        };
    let parsed = match std::str::from_utf8(&capture.body)
        .context("source response is not UTF-8")
        .and_then(|body| sources::parse(&parse_target, body))
    {
        Ok(mut parsed) => {
            if let Some(evidence) = retained {
                evidence.attach(&mut parsed, &job.target.url)?;
            }
            parsed
        }
        Err(error) => {
            let blocked = error.is::<sources::AccessDenied>()
                || error.is::<sources::contacts::AccessDenied>();
            let error = error.context("parsing source page");
            let error = match retained {
                Some(evidence) => error.context(evidence.describe()),
                None => error,
            };
            let detail = format!("{error:#}");
            database
                .fail_job(job, Some(&capture), &detail, blocked)
                .await
                .with_context(|| format!("recording page failure after: {detail}"))?;
            if blocked && job.target.kind != crate::model::PageKind::Contacts {
                return Err(error);
            }
            return Ok(Some(error));
        }
    };
    match database
        .persist(Some(job.id), &job.target, &capture, &parsed)
        .await
    {
        Ok(_) => Ok(None),
        Err(error) => record_failure(
            database,
            job,
            Some(&capture),
            false,
            error.context("persisting source page"),
        )
        .await
        .map(|()| None),
    }
}

async fn retain_profile_capture(
    job: &Job,
    capture: Capture,
) -> Result<(Capture, Option<crate::profile_evidence::RetainedCapture>)> {
    if job.target.source != Source::MileSplit || job.target.kind != crate::model::PageKind::Athlete
    {
        return Ok((capture, None));
    }
    crate::profile_evidence::retain(capture)
        .await
        .map(|(capture, evidence)| (capture, Some(evidence)))
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
    let (id, cursor): (i64, serde_json::Value) = sqlx::query_as(
        "SELECT id,cursor FROM scrape_pages WHERE source=$1 AND (cursor=$2 OR ( \
         cursor->>'kind'='contacts' AND $2::jsonb->>'kind'='contacts' \
         AND url=$2::jsonb->>'url' \
         AND cursor->'context'->>'school_id'=$2::jsonb->'context'->>'school_id')) \
         ORDER BY (cursor=$2) DESC,id LIMIT 1",
    )
    .bind(target.source.as_str())
    .bind(cursor)
    .fetch_one(database.pool())
    .await
    .context("finding replay checkpoint")?;
    let job = Job {
        id,
        target: serde_json::from_value(cursor).context("decoding replay checkpoint target")?,
    };
    if !database
        .start_attempt(id, config.limits.max_attempts)
        .await?
    {
        let replayable: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM scrape_pages p WHERE p.id=$1 \
             AND (p.status='succeeded' OR (p.status IN ('pending','failed') AND p.attempts >= $2)) \
             AND (p.cursor->>'kind' <> 'contacts' OR NOT EXISTS ( \
               SELECT 1 FROM scrape_pages blocked CROSS JOIN LATERAL unnest(ARRAY[blocked.url,blocked.cursor->>'access_stop_url']) AS stop(url) WHERE blocked.status='blocked' AND blocked.cursor->>'kind'='contacts' \
               AND regexp_replace(lower(substring(stop.url from '(?i)^https?://([^/?#]+)')), \
                 CASE WHEN lower(stop.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') = \
               regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')), \
                 CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,''))))",
        )
        .bind(id)
        .bind(i32::try_from(config.limits.max_attempts).context("converting maximum attempts")?)
        .fetch_one(database.pool())
        .await
        .context("checking replay checkpoint")?;
        ensure!(replayable, "replay checkpoint could not start an attempt");
    }
    let body = match read_replay(input).await {
        Ok(body) => body,
        Err(error) => return record_failure(database, &job, None, false, error).await,
    };
    let capture = match crate::transport::prepare_response(
        job.target.kind,
        Capture {
            url: job.target.url.clone(),
            status: 200,
            content_type: String::from("application/octet-stream"),
            body,
        },
    ) {
        Ok(capture) => capture,
        Err(failure) => {
            return record_failure(
                database,
                &job,
                failure.capture.as_ref(),
                failure.blocked,
                anyhow!(failure.detail),
            )
            .await;
        }
    };
    persist_capture(database, &job, capture)
        .await?
        .map_or_else(|| Ok(()), Err)
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
    database
        .write_export_with_layout(
            output,
            &crate::export::ExportOptions {
                states: config.states.clone(),
                graduation_years: config.graduation_years.clone(),
                rows_per_file: config.export.rows_per_file,
                per_athlete_sheets: config.export.per_athlete_sheets,
            },
            config.export.layout,
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

#[cfg(test)]
mod schedule_tests {
    use super::{JobClass, Schedule, Source};

    const CYCLE: [JobClass; 8] = [
        JobClass::Profile,
        JobClass::Profile,
        JobClass::Profile,
        JobClass::Roster,
        JobClass::Profile,
        JobClass::Profile,
        JobClass::Contacts,
        JobClass::Bulk,
    ];

    #[test]
    fn given_both_sources_when_reserving_two_cycles_then_sources_alternate_and_all_classes_progress()
     {
        let mut schedule = Schedule::new();
        for _ in 0..2 {
            for expected in CYCLE {
                for source in [Source::AthleticNet, Source::MileSplit] {
                    assert_eq!(schedule.next_source, source);
                    assert_eq!(schedule.preferred(source), expected);
                    schedule.reserved(source);
                }
            }
        }
        assert_eq!(schedule.athleticnet, 0);
        assert_eq!(schedule.milesplit, 0);
        assert_eq!(schedule.next_source, Source::AthleticNet);
    }

    #[test]
    fn given_only_one_runnable_source_when_reserving_then_other_source_keeps_its_own_cycle() {
        for source in [Source::AthleticNet, Source::MileSplit] {
            let mut schedule = Schedule::new();
            let other = match source {
                Source::AthleticNet => Source::MileSplit,
                Source::MileSplit => Source::AthleticNet,
            };
            for _ in 0..2 {
                for expected in CYCLE {
                    assert_eq!(schedule.preferred(source), expected);
                    assert_eq!(schedule.preferred(other), JobClass::Profile);
                    schedule.reserved(source);
                    assert_eq!(schedule.next_source, other);
                }
            }
            assert_eq!(schedule.preferred(source), JobClass::Profile);
            schedule.reserved(other);
            assert_eq!(schedule.next_source, source);
        }
    }
}
