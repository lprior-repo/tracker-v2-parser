use anyhow::{Result, ensure};
use serde::Serialize;

use crate::{
    database::Database,
    model::{School, Source, Target},
    sources::wiaa::{self, DirectoryMatch, DirectorySchool},
};

const MAX_SCHOOLS: usize = 10_000;

#[cfg(test)]
mod tests;

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

pub(crate) async fn schools(
    database: &Database,
    providers: &[Source],
) -> Result<Vec<(Source, School)>> {
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

pub(crate) fn prepare(
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

pub(crate) async fn check_stops(database: &Database) -> Result<()> {
    let stopped: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM scrape_pages p CROSS JOIN LATERAL unnest(ARRAY[p.url,p.cursor->>'access_stop_url']) AS stop(url) WHERE p.status='blocked' AND p.cursor->>'kind'='contacts' AND lower(substring(stop.url from '(?i)^https?://([^/?#]+)')) IN ('schools.wiaawi.org','schools.wiaawi.org:443','schools.wiaawi.org:80'))"
    ).fetch_one(database.pool()).await?;
    ensure!(
        !stopped,
        "WIAA contact authority has a preserved access stop requiring human review"
    );
    Ok(())
}
