use std::time::Instant;

use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::{Value, json};
use track_scrapper_v2::{database::Database, durable::model::CrawlRequest};

struct Mutation {
    label: &'static str,
    id: String,
    original: Option<String>,
    replacement: String,
    statement: &'static str,
}

async fn cases(database: &Database, request: &CrawlRequest) -> Result<Vec<Mutation>> {
    let profile: (i64,String)=sqlx::query_as("SELECT profile.id,profile.profile_url FROM athlete_sources profile JOIN scrape_pages p ON p.source=profile.source AND p.cursor->'context'->>'athlete_id'=profile.source_id WHERE p.source='milesplit' AND p.cursor->>'kind'='athlete' AND p.cursor->'context'->>'state'='MI' AND p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/athletes/[1-9][0-9]*(-[^/?#%]+)?(/(stats|progression))?/?$' ORDER BY p.id LIMIT 1")
        .fetch_one(database.pool()).await?;
    let school: (i64,Option<String>)=sqlx::query_as("SELECT id,address FROM schools WHERE source='milesplit' AND state='MI' ORDER BY id LIMIT 1").fetch_one(database.pool()).await?;
    let contact: (i64, Option<String>) =
        sqlx::query_as("SELECT id,role FROM contacts ORDER BY id LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let stopped: (i64,String)=sqlx::query_as("SELECT id,status FROM scrape_pages WHERE source='milesplit' AND cursor->>'kind'='contacts' AND status='pending' ORDER BY id LIMIT 1").fetch_one(database.pool()).await?;
    let operation: (String, Option<String>) = sqlx::query_as("SELECT operation_id,retry_at_ms::text FROM durable_page_operations WHERE run_generation=$1 ORDER BY page_id LIMIT 1")
        .bind(request.page_scope()?.run_generation).fetch_one(database.pool()).await?;
    Ok(vec![
        Mutation {
            label: "retry_operation",
            id: operation.0,
            original: operation.1,
            replacement: "1".into(),
            statement: "UPDATE durable_page_operations SET retry_at_ms=$2::text::bigint WHERE operation_id=$1",
        },
        Mutation {
            label: "profile_identity",
            id: profile.0.to_string(),
            replacement: format!("{}-qualification", profile.1),
            original: Some(profile.1),
            statement: "UPDATE athlete_sources SET profile_url=$2 WHERE id=$1::text::bigint",
        },
        Mutation {
            label: "school_coverage",
            id: school.0.to_string(),
            original: school.1,
            replacement: "selector qualification address".into(),
            statement: "UPDATE schools SET address=$2 WHERE id=$1::text::bigint",
        },
        Mutation {
            label: "contact_coverage",
            id: contact.0.to_string(),
            original: contact.1,
            replacement: "selector qualification role".into(),
            statement: "UPDATE contacts SET role=$2 WHERE id=$1::text::bigint",
        },
        Mutation {
            label: "access_stop",
            id: stopped.0.to_string(),
            original: Some(stopped.1),
            replacement: "blocked".into(),
            statement: "UPDATE scrape_pages SET status=$2 WHERE id=$1::text::bigint",
        },
    ])
}

async fn mutate(database: &Database, case: &Mutation, value: Option<&str>) -> Result<f64> {
    let started = Instant::now();
    sqlx::query(case.statement)
        .bind(&case.id)
        .bind(value)
        .execute(database.pool())
        .await?;
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

async fn samples(
    database: &Database,
    request: &CrawlRequest,
    count: usize,
    now: i64,
    case: &Mutation,
) -> Result<Value> {
    let (selection, commit) = stream::iter((0..count).map(Ok::<_, anyhow::Error>))
        .try_fold(
            (Vec::new(), Vec::new()),
            |(mut selection, mut commit), index| async move {
                let value = if index.checked_rem(2) == Some(0) {
                    Some(case.replacement.as_str())
                } else {
                    case.original.as_deref()
                };
                commit.push(mutate(database, case, value).await?);
                let (selected, elapsed) = super::measured(database, request, now).await?;
                selection.push(elapsed);
                if index.checked_rem(10) == Some(0) {
                    ensure!(
                        selected == super::expected(database, request, now).await?,
                        "canonical-change selection differs from SQL oracle for {}",
                        case.label
                    );
                }
                Ok((selection, commit))
            },
        )
        .await?;
    Ok(
        json!({"selection":super::statistics(selection),"mutation_commit":super::statistics(commit)}),
    )
}

pub async fn run(
    database: &Database,
    request: &CrawlRequest,
    count: usize,
    now: i64,
) -> Result<Value> {
    stream::iter(
        cases(database, request)
            .await?
            .into_iter()
            .map(Ok::<_, anyhow::Error>),
    )
    .try_fold(serde_json::Map::new(), |mut report, case| async move {
        let result = samples(database, request, count, now, &case).await;
        mutate(database, &case, case.original.as_deref())
            .await
            .context("restoring original canonical replica value")?;
        report.insert(case.label.into(), result?);
        Ok(report)
    })
    .await
    .map(Value::Object)
}

pub fn check(report: &Value, limit: f64) -> Result<()> {
    let cases = report
        .get("canonical_changes")
        .and_then(Value::as_object)
        .context("canonical-change measurements are missing")?;
    cases.iter().try_for_each(|(name, case)| {
        ensure!(
            case.pointer("/selection/max_ms")
                .and_then(Value::as_f64)
                .is_some_and(|maximum| maximum <= limit),
            "canonical-change selection exceeded the qualification limit for {name}"
        );
        Ok(())
    })
}
