mod acquisition;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::{
    config::Config,
    database::Database,
    model::{School, Source, Target},
    sources::wiaa::{self, DirectoryMatch, DirectorySchool},
    transport::{Mode, Options, Transport},
};

const MAX_SCHOOLS: usize = 10_000;

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub index_requests: usize,
    pub directory_schools: usize,
    pub provider_schools: usize,
    pub matched: usize,
    pub missing: usize,
    pub uncorroborated: usize,
    pub ambiguous: usize,
    pub enqueued: u64,
}

pub async fn enrich(config: &Config, database_url: &str) -> Result<Report> {
    let mut config = config.clone();
    config.validate()?;
    ensure!(
        config.states.iter().any(|state| state == "WI"),
        "WIAA enrichment requires Wisconsin scope"
    );
    ensure!(
        config.acquisition.includes_contacts(),
        "WIAA enrichment requires contact collection enabled"
    );
    let database = Database::connect(database_url).await?;
    let result = locked(&config, &database).await;
    database.close().await;
    result
}

async fn locked(config: &Config, database: &Database) -> Result<Report> {
    let mut transaction = database.pool().begin().await?;
    let acquired: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtext(current_schema()),194832741)",
    )
    .fetch_one(&mut *transaction)
    .await?;
    ensure!(acquired, "another scraper owns the acquisition session");
    let result = collect(config, database).await;
    let cleanup = transaction
        .rollback()
        .await
        .context("releasing WIAA enrichment lock");
    combine(result, cleanup)
}

async fn collect(config: &Config, database: &Database) -> Result<Report> {
    acquisition::check_stops(database).await?;
    let providers = config.enabled_sources();
    let source = providers
        .iter()
        .copied()
        .find(|source| *source == Source::MileSplit)
        .or_else(|| providers.first().copied())
        .context("WIAA enrichment requires an enabled athlete provider")?;
    let schools = schools(database, &providers).await?;
    let mut transport = Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: std::env::temp_dir().join("tracker-wiaa-unused-http-profile"),
        headless: true,
        rps: config.limits.requests_per_second,
    })?;
    let result = acquisition::collect_index(database, &mut transport, source).await;
    let entries = combine(result, transport.close().await)?;
    let (targets, mut report) = prepare(&schools, &entries, config.season)?;
    report.enqueued = database.enqueue(&targets, false).await?;
    Ok(report)
}

async fn schools(database: &Database, providers: &[Source]) -> Result<Vec<(Source, School)>> {
    let sources = providers
        .iter()
        .map(|source| source.as_str())
        .collect::<Vec<_>>();
    let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT source,jsonb_build_object('id',source_id,'name',name,'state',state,'city',city,'address',address,'postal_code',postal_code,'website',website) FROM schools WHERE source=ANY($1) AND state='WI' ORDER BY source,source_id LIMIT 10001"
    ).bind(sources).fetch_all(database.pool()).await?;
    ensure!(
        rows.len() <= MAX_SCHOOLS,
        "WIAA enrichment exceeds 10000 provider schools"
    );
    rows.into_iter()
        .map(|(source, school)| {
            Ok((
                serde_json::from_value(serde_json::Value::String(source))?,
                serde_json::from_value(school)?,
            ))
        })
        .collect()
}

fn prepare(
    schools: &[(Source, School)],
    entries: &[DirectorySchool],
    season: i32,
) -> Result<(Vec<Target>, Report)> {
    let report = Report {
        index_requests: 27,
        directory_schools: entries.len(),
        provider_schools: schools.len(),
        ..Report::default()
    };
    schools.iter().try_fold(
        (Vec::new(), report),
        |(mut targets, mut report), (source, school)| {
            match wiaa::match_school(school, entries) {
                DirectoryMatch::Matched(entry) => {
                    targets.push(wiaa::contact_target(*source, school, entry, season)?);
                    report.matched = report.matched.saturating_add(1);
                }
                DirectoryMatch::Missing => report.missing = report.missing.saturating_add(1),
                DirectoryMatch::Uncorroborated => {
                    report.uncorroborated = report.uncorroborated.saturating_add(1);
                }
                DirectoryMatch::Ambiguous => report.ambiguous = report.ambiguous.saturating_add(1),
            }
            Ok((targets, report))
        },
    )
}

fn combine<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("cleanup also failed: {cleanup:#}")))
        }
    }
}
