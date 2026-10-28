mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target},
    sources,
};

const PROFILE_URL: &str = "https://oh.milesplit.com/athletes/15005189-brice-fuller";
const RAW: &str = include_str!("../fixtures/milesplit/oh_meet_770621_rs1321880_raw.html");

fn career_target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        PROFILE_URL.to_owned(),
        PageContext::default(),
    )
}

fn career_row(mark: &str, meet: &str) -> String {
    format!(
        r#"<div class="record" data-season="outdoor" data-event="800m" data-level="hs"><div class="seed">{mark}</div><div class="location"><a href="/meets/{meet}-fixture">Fixture</a></div><div class="date">2025-05-01</div></div>"#
    )
}

fn career_body(rows: &[String]) -> String {
    format!(
        r#"<html><head><title>Brice Fuller - MileSplit</title><link rel="canonical" href="{PROFILE_URL}"><script type="application/ld+json">{{"@context":"https://schema.org","@type":"Person","name":"Brice Fuller","image":"https://assets.sp.milesplit.com/athlete_photos/15005189?v=1","affiliation":"","gender":"Male","url":"{PROFILE_URL}"}}</script></head><body><h1 id="athleteName">Brice Fuller</h1><div class="athlete-info"><span class="current-school"><a href="/teams/9573-fairless">Fairless</a></span><span class="city-state">Brewster, OH</span><span class="grad-year">Class of 2028</span></div><div id="progression"></div><div id="stats">{}</div></body></html>"#,
        rows.join("")
    )
}

async fn persist(
    database: &Database,
    target: &Target,
    body: &str,
    page: &ParsedPage,
) -> Result<()> {
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".to_owned(),
        body: body.as_bytes().to_vec(),
    };
    database.persist(None, target, &capture, page).await?;
    Ok(())
}

async fn persist_career(database: &Database, target: &Target, rows: &[String]) -> Result<()> {
    let body = career_body(rows);
    let page = sources::parse(target, &body)?;
    persist(database, target, &body, &page).await
}

async fn career_observations(pool: &sqlx::PgPool) -> Result<Vec<(String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT raw_mark,raw#>>'{source_values,revision_status}' FROM results ORDER BY raw_mark",
    )
    .fetch_all(pool)
    .await?)
}

#[test]
fn given_ambiguous_career_rows_when_reordered_then_observation_ids_are_stable() -> Result<()> {
    let first = career_row("2:00.00", "900");
    let second = career_row("2:10.00", "900");
    let before = sources::parse(
        &career_target(),
        &career_body(&[first.clone(), second.clone()]),
    )?;
    let after = sources::parse(&career_target(), &career_body(&[second, first]))?;
    let identities = |page: ParsedPage| {
        page.performances
            .into_iter()
            .map(|row| (row.mark, row.result_id))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(identities(before), identities(after));
    Ok(())
}

#[tokio::test]
async fn given_unique_history_when_context_becomes_ambiguous_then_old_observation_is_quarantined()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = career_target();
            persist_career(&database, &target, &[career_row("1:50.00", "900")]).await?;
            let rows = [career_row("2:00.00", "900"), career_row("2:10.00", "900")];
            persist_career(&database, &target, &rows).await?;
            let observed = career_observations(&test.pool).await?;
            assert_eq!(observed.len(), 3);
            assert_eq!(
                observed[0],
                (
                    "1:50.00".to_owned(),
                    Some("unverified_prior_revision".to_owned())
                )
            );
            persist_career(&database, &target, &[rows[1].clone(), rows[0].clone()]).await?;
            assert_eq!(career_observations(&test.pool).await?, observed);
            persist_career(&database, &target, &[career_row("2:05.00", "900")]).await?;
            let observed = career_observations(&test.pool).await?;
            assert!(
                observed
                    .iter()
                    .any(|(mark, status)| mark == "2:05.00" && status.is_none())
            );
            assert!(
                observed
                    .iter()
                    .filter(|(mark, _)| mark != "2:05.00")
                    .all(|(_, status)| status.as_deref() == Some("unverified_prior_revision"))
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_career_ranking_sibling_mask_when_context_changes_then_obsolete_mark_is_quarantined()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = career_target();
            let row = |mark: &str| {
                career_row(mark, "900").replace(
                    "<div class=\"location\">",
                    "<span class=\"mask\">rank</span><div class=\"location\">",
                )
            };
            persist_career(&database, &target, &[row("1:50.00")]).await?;
            let rows = [row("2:00.00"), row("2:10.00")];
            let parsed = sources::parse(&target, &career_body(&rows))?;
            assert_eq!(
                parsed
                    .performances
                    .iter()
                    .map(|row| row.mark.as_str())
                    .collect::<Vec<_>>(),
                vec!["2:00.00", "2:10.00"]
            );
            persist_career(&database, &target, &rows).await?;
            let observed = career_observations(&test.pool).await?;
            assert_eq!(
                observed,
                vec![
                    (
                        "1:50.00".to_owned(),
                        Some("unverified_prior_revision".to_owned())
                    ),
                    ("2:00.00".to_owned(), None),
                    ("2:10.00".to_owned(), None),
                ]
            );
            persist_career(&database, &target, &[rows[1].clone(), rows[0].clone()]).await?;
            assert_eq!(career_observations(&test.pool).await?, observed);
            persist_career(&database, &target, &[row("2:05.00")]).await?;
            assert_eq!(
                career_observations(&test.pool).await?,
                vec![
                    (
                        "2:00.00".to_owned(),
                        Some("unverified_prior_revision".to_owned())
                    ),
                    ("2:05.00".to_owned(), None),
                    (
                        "2:10.00".to_owned(),
                        Some("unverified_prior_revision".to_owned())
                    ),
                ]
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_history_when_other_context_or_empty_response_arrives_then_prior_is_not_deleted()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = career_target();
            persist_career(&database, &target, &[career_row("1:50.00", "900")]).await?;
            let mut continuation = target.clone();
            continuation.url.push_str("/progression");
            persist_career(&database, &continuation, &[career_row("2:10.00", "901")]).await?;
            persist_career(&database, &target, &[]).await?;
            assert_eq!(
                career_observations(&test.pool).await?,
                vec![("1:50.00".to_owned(), None), ("2:10.00".to_owned(), None)]
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

fn raw_target(file: bool) -> Result<Target> {
    let mut target = sources::milesplit::meet_target("770621", Sport::CrossCountry)?;
    target.url = "https://oh.milesplit.com/meets/770621/results".to_owned();
    if file {
        target.kind = PageKind::Results;
        target.url.push_str("/1321880/raw");
    }
    Ok(target)
}

fn old_raw_page(target: &Target, file: bool) -> Result<ParsedPage> {
    let mut page = sources::parse(target, RAW)?;
    assert_eq!(page.unassigned_results.len(), 80);
    for row in &mut page.unassigned_results {
        let id = row.result_id.as_deref().context("raw ID")?;
        let mut coordinates: Value =
            serde_json::from_str(id.strip_prefix("raw:").context("raw prefix")?)?;
        coordinates[1] = json!(if file { "1321880" } else { "inline" });
        row.result_id = Some(format!("raw:{}", serde_json::to_string(&coordinates)?));
        let evidence = row.raw.as_object_mut().context("raw evidence")?;
        evidence.remove("kind");
        evidence.remove("published_file_identity");
    }
    Ok(page)
}

#[tokio::test]
async fn given_large_meet_with_current_file_rows_when_another_file_replays_then_legacy_budget_is_unchanged()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = raw_target(true)?;
        let page = sources::parse(&target, RAW)?;
        persist(&database, &target, RAW, &page).await?;
        sqlx::query(
            "INSERT INTO results(source_result_key,source_result_id,source,event,raw_mark,sport,status,is_personal_record,is_relay,meet_id,source_url,raw) \
             SELECT encode(sha256(convert_to('large-meet-' || n::text,'UTF8')),'hex'), \
             'raw:' || json_build_array('770621','milesplit-file:770621:9999999','100m','final',n,'published')::text, \
             'milesplit','100m','12.00','xc','recorded',FALSE,FALSE,'770621', \
             'https://oh.milesplit.com/meets/770621/results/9999999/raw', \
             jsonb_build_object('raw',jsonb_build_object('kind','raw_result','published_file_identity','milesplit-file:770621:9999999')) \
             FROM generate_series(1,10001) AS n",
        ).execute(&test.pool).await?;
        persist(&database, &target, RAW, &page).await?;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE source_url=$1),count(*) FILTER (WHERE raw#>>'{raw,published_file_identity}'='milesplit-file:770621:9999999') FROM results",
        ).bind(&target.url).fetch_one(&test.pool).await?;
        assert_eq!(counts, (80, 10001));
        let changed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM results WHERE raw#>>'{raw,revision_status}' IS NOT NULL",
        ).fetch_one(&test.pool).await?;
        assert_eq!(changed, 0);
        database.close().await;
        Ok(())
    })).await
}

fn row_html(body: &str, selector: &str) -> Result<String> {
    let document = scraper::Html::parse_document(body);
    let selector = scraper::Selector::parse(selector).unwrap();
    Ok(document
        .select(&selector)
        .next()
        .context("retained row")?
        .html())
}

async fn seed_old_masked_career(
    database: &Database,
    target: &Target,
    own_mask: bool,
) -> Result<String> {
    let public = career_body(&[career_row("1:50.00", "900")]);
    let masked = if own_mask {
        public.replace("class=\"record\"", "class=\"record mask\"")
    } else {
        public.replace("id=\"stats\"", "id=\"stats\" class=\"mask\"")
    };
    let mut old = sources::parse(target, &masked)?;
    assert!(old.performances.is_empty());
    old.performances = sources::parse(target, &public)?.performances;
    let html = row_html(&masked, ".record")?;
    old.performances[0]
        .source_values
        .as_mut()
        .context("row evidence")?["row"]["html"] = json!(html);
    persist(database, target, &masked, &old).await?;
    Ok(masked)
}

#[tokio::test]
async fn given_retained_self_masked_career_when_replayed_then_quarantine_clears_only_on_public_reconfirmation()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = career_target();
            let masked = seed_old_masked_career(&database, &target, true).await?;
            persist(
                &database,
                &target,
                &masked,
                &sources::parse(&target, &masked)?,
            )
            .await?;
            assert_eq!(
                career_observations(&test.pool).await?,
                vec![(
                    "1:50.00".to_owned(),
                    Some("unverified_prior_revision".to_owned())
                )]
            );
            let reason: String = sqlx::query_scalar(
                "SELECT raw#>>'{source_values,mask_reconciliation}' FROM results",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(reason, "retained_mark_masked");
            persist_career(&database, &target, &[career_row("2:00.00", "900")]).await?;
            assert_eq!(
                career_observations(&test.pool).await?,
                vec![("2:00.00".to_owned(), None)]
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_retained_own_mask_when_fresh_masked_html_changes_then_prior_mark_stays_quarantined()
-> Result<()> {
    for changed_mark in [false, true] {
        support::with_schema(|test| {
            Box::pin(async move {
                let database = Database::connect(&test.url).await?;
                let target = career_target();
                let masked = seed_old_masked_career(&database, &target, true).await?;
                let changed = if changed_mark {
                    masked.replace("1:50.00", "1:51.00")
                } else {
                    masked.replace(
                        "class=\"record mask\"",
                        "class=\"record mask\" data-layout=\"new\"",
                    )
                };
                let page = sources::parse(&target, &changed)?;
                assert!(page.performances.is_empty());
                persist(&database, &target, &changed, &page).await?;
                assert_eq!(
                    career_observations(&test.pool).await?,
                    vec![(
                        "1:50.00".to_owned(),
                        Some("unverified_prior_revision".to_owned())
                    )]
                );
                let html: String =
                    sqlx::query_scalar("SELECT raw#>>'{source_values,row,html}' FROM results")
                        .fetch_one(&test.pool)
                        .await?;
                assert!(html.contains("1:50.00"));
                assert!(!html.contains("data-layout"));
                database.close().await;
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_ancestor_mask_without_per_result_capture_binding_when_replayed_then_uncertainty_is_explicit()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = career_target();
            let masked = seed_old_masked_career(&database, &target, false).await?;
            persist(
                &database,
                &target,
                &masked,
                &sources::parse(&target, &masked)?,
            )
            .await?;
            assert_eq!(
                career_observations(&test.pool).await?,
                vec![("1:50.00".to_owned(), None)]
            );
            let reason: String = sqlx::query_scalar(
                "SELECT raw#>>'{source_values,mask_reconciliation}' FROM results",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(reason, "ancestor_mask_capture_binding_unproven");
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_previous_identified_raw_shape_when_replayed_then_uncertainty_is_export_visible()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = raw_target(true)?;
            let body = RAW.replacen(
                "Jeydyn Fields",
                "<a href='/athletes/123456'>Jeydyn Fields</a>",
                1,
            );
            let current = sources::parse(&target, &body)?;
            assert_eq!(current.performances.len(), 1);
            let mut old = current.clone();
            let row = old.performances.first_mut().context("identified raw row")?;
            row.source_values = None;
            let mut coordinates: Value = serde_json::from_str(
                row.result_id
                    .as_deref()
                    .context("raw ID")?
                    .strip_prefix("raw:")
                    .context("raw prefix")?,
            )?;
            coordinates[1] = json!("1321880");
            row.result_id = Some(format!("raw:{}", serde_json::to_string(&coordinates)?));
            let old_id = row.result_id.clone();
            persist(&database, &target, &body, &old).await?;
            let before: Value =
                sqlx::query_scalar("SELECT raw FROM results WHERE source_result_id=$1")
                    .bind(&old_id)
                    .fetch_one(&test.pool)
                    .await?;
            assert!(before.get("source_values").is_none() && before.get("raw").is_none());
            persist(&database, &target, &body, &current).await?;
            let mut after: Value =
                sqlx::query_scalar("SELECT raw FROM results WHERE source_result_id=$1")
                    .bind(&old_id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(
                after["source_values"]["revision_status"],
                "unverified_prior_revision"
            );
            after
                .as_object_mut()
                .context("retained raw")?
                .remove("source_values");
            assert_eq!(after, before);
            let records = database.athlete_records().await?;
            let prior = records
                .iter()
                .flat_map(|record| &record.performances)
                .find(|row| row.performance.result_id == old_id)
                .context("exported prior evidence")?;
            assert_eq!(
                prior
                    .performance
                    .source_values
                    .as_ref()
                    .context("exported marker")?["revision_status"],
                "unverified_prior_revision"
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 81);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_public_result_when_later_capture_masks_ancestor_then_original_is_not_invalidated()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = career_target();
            let body = career_body(&[career_row("1:50.00", "900")]);
            persist(&database, &target, &body, &sources::parse(&target, &body)?).await?;
            let masked = body.replace("id=\"stats\"", "id=\"stats\" class=\"mask\"");
            for _ in 0..2 {
                persist(
                    &database,
                    &target,
                    &masked,
                    &sources::parse(&target, &masked)?,
                )
                .await?;
            }
            assert_eq!(
                career_observations(&test.pool).await?,
                vec![("1:50.00".to_owned(), None)]
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_self_masked_legacy_pr_when_replayed_then_quarantined_without_rejecting_locked_rankings()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = career_target();
        let table = r#"<table id="prTable"><tr data-pr-mode="outdoor" data-pr-level="hs"><td>800m</td><td>1:50.00</td><td class="mask">1</td><td></td><td></td></tr></table>"#;
        let public = career_body(&[]).replace("</body>", &format!("{table}</body>"));
        let current = sources::parse(&target, &public)?;
        assert_eq!(current.performances.len(), 1);
        let masked = public.replace("<tr data-pr-mode", "<tr class=\"mask\" data-pr-mode");
        let mut old = sources::parse(&target, &masked)?;
        assert!(old.performances.is_empty());
        old.performances = current.performances;
        let html = row_html(&masked, "#prTable tr")?;
        old.performances[0].source_values.as_mut().context("PR evidence")?["row"]["html"] = json!(html);
        persist(&database, &target, &masked, &old).await?;
        persist(&database, &target, &masked, &sources::parse(&target, &masked)?).await?;
        assert_eq!(career_observations(&test.pool).await?, vec![("1:50.00".to_owned(), Some("unverified_prior_revision".to_owned()))]);
        persist(&database, &target, &public, &sources::parse(&target, &public)?).await?;
        assert_eq!(career_observations(&test.pool).await?, vec![("1:50.00".to_owned(), None)]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_old_manual_table_url_keys_when_numbered_file_replayed_then_rows_are_adopted()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut target = sources::milesplit::meet_target("770517", Sport::Outdoor)?;
            target.kind = PageKind::Results;
            target.url = "https://ar.milesplit.com/meets/770517/results/1311117/raw".to_owned();
            let body = include_str!("../fixtures/milesplit/meet-ar-770517-results.html")
                .replace("rel=\"canonical\"", "rel=\"unused\"");
            let mut old = sources::parse(&target, &body)?;
            assert!(!old.unassigned_results.is_empty());
            for row in &mut old.unassigned_results {
                let id = row.result_id.as_deref().context("table ID")?;
                let mut coordinates: Value =
                    serde_json::from_str(id.strip_prefix("table:").context("table prefix")?)?;
                coordinates[1] = json!(target.url);
                row.result_id = Some(format!("table:{}", serde_json::to_string(&coordinates)?));
            }
            persist(&database, &target, &body, &old).await?;
            let before: Vec<i64> = sqlx::query_scalar("SELECT id FROM results ORDER BY id")
                .fetch_all(&test.pool)
                .await?;
            for _ in 0..2 {
                persist(&database, &target, &body, &sources::parse(&target, &body)?).await?;
            }
            let after: Vec<i64> = sqlx::query_scalar("SELECT id FROM results ORDER BY id")
                .fetch_all(&test.pool)
                .await?;
            assert_eq!(before, after);
            let old_keys: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM results WHERE source_result_id LIKE '%https://%'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(old_keys, 0);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_previous_version_raw_keys_when_same_fixture_replayed_then_proven_rows_are_adopted()
-> Result<()> {
    for file in [true, false] {
        support::with_schema(|test| Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let old_target = raw_target(file)?;
            persist(&database, &old_target, RAW, &old_raw_page(&old_target, file)?).await?;
            let original: Vec<i64> = sqlx::query_scalar("SELECT id FROM results ORDER BY id").fetch_all(&test.pool).await?;
            for target in [raw_target(true)?, raw_target(false)?] {
                persist(&database, &target, RAW, &sources::parse(&target, RAW)?).await?;
            }
            let current: Vec<i64> = sqlx::query_scalar("SELECT id FROM results ORDER BY id").fetch_all(&test.pool).await?;
            if file {
                assert_eq!(current, original);
            } else {
                assert_eq!(current.len(), 160);
                assert!(original.iter().all(|id| current.contains(id)));
                let quarantined: i64 = sqlx::query_scalar("SELECT count(*) FROM results WHERE id=ANY($1) AND raw#>>'{raw,revision_status}'='unverified_prior_revision'").bind(&original).fetch_one(&test.pool).await?;
                assert_eq!(quarantined, 80);
            }
            let canonical: i64 = sqlx::query_scalar("SELECT count(*) FROM results WHERE source_result_id LIKE '%milesplit-file:770621:1321880%' AND raw#>>'{raw,published_file_identity}'='milesplit-file:770621:1321880'").fetch_one(&test.pool).await?;
            assert_eq!(canonical, 80);
            database.close().await;
            Ok(())
        })).await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_identical_rows_in_distinct_numbered_files_when_only_a_replays_then_b_is_untouched()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let a = raw_target(true)?;
            persist(&database, &a, RAW, &old_raw_page(&a, true)?).await?;
            let mut b = a.clone();
            b.url = b.url.replace("1321880", "1321881");
            let body_b = RAW.replace("1321880", "1321881").replacen(
                "Jeydyn Fields",
                "<a href='/athletes/123456'>Jeydyn Fields</a>",
                1,
            );
            let mut old_b = sources::parse(&b, &body_b)?;
            assert_eq!(old_b.performances.len(), 1);
            old_b.unassigned_results.clear();
            let row = old_b
                .performances
                .first_mut()
                .context("file B identified row")?;
            row.source_values = None;
            let mut coordinates: Value = serde_json::from_str(
                row.result_id
                    .as_deref()
                    .context("file B ID")?
                    .strip_prefix("raw:")
                    .context("raw prefix")?,
            )?;
            coordinates[1] = json!("1321881");
            let a_page = sources::parse(&a, RAW)?;
            let a_id = a_page
                .unassigned_results
                .first()
                .context("file A matching row")?
                .result_id
                .as_deref()
                .context("file A ID")?;
            let mut a_coordinates: Value =
                serde_json::from_str(a_id.strip_prefix("raw:").context("raw prefix")?)?;
            a_coordinates[1] = json!("1321881");
            assert_eq!(coordinates, a_coordinates);
            row.result_id = Some(format!("raw:{}", serde_json::to_string(&coordinates)?));
            let id_b = row.result_id.clone();
            persist(&database, &b, &body_b, &old_b).await?;
            sqlx::query("UPDATE athletes SET needs_review=FALSE")
                .execute(&test.pool)
                .await?;
            let before_result: Value =
                sqlx::query_scalar("SELECT to_jsonb(r) FROM results r WHERE source_result_id=$1")
                    .bind(&id_b)
                    .fetch_one(&test.pool)
                    .await?;
            let before_athlete: Value = sqlx::query_scalar("SELECT to_jsonb(a) FROM athletes a")
                .fetch_one(&test.pool)
                .await?;
            persist(&database, &a, RAW, &sources::parse(&a, RAW)?).await?;
            let after_result: Value =
                sqlx::query_scalar("SELECT to_jsonb(r) FROM results r WHERE source_result_id=$1")
                    .bind(&id_b)
                    .fetch_one(&test.pool)
                    .await?;
            let after_athlete: Value = sqlx::query_scalar("SELECT to_jsonb(a) FROM athletes a")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(after_result, before_result);
            assert_eq!(after_athlete, before_athlete);
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 81);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_legacy_numbered_trailing_slash_url_when_replayed_then_original_rows_are_adopted()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut target = raw_target(true)?;
            target.url.push('/');
            persist(&database, &target, RAW, &old_raw_page(&target, true)?).await?;
            let before: Vec<i64> = sqlx::query_scalar("SELECT id FROM results ORDER BY id")
                .fetch_all(&test.pool)
                .await?;
            for replay in [target, raw_target(true)?] {
                persist(&database, &replay, RAW, &sources::parse(&replay, RAW)?).await?;
            }
            let after: Vec<i64> = sqlx::query_scalar("SELECT id FROM results ORDER BY id")
                .fetch_all(&test.pool)
                .await?;
            assert_eq!(after, before);
            let quarantined: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM results WHERE raw#>>'{raw,revision_status}' IS NOT NULL",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(quarantined, 0);
            assert_eq!(after.len(), 80);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_legacy_raw_row_without_retained_html_when_replayed_then_it_remains_quarantined()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = raw_target(true)?;
            let mut old = old_raw_page(&target, true)?;
            old.unassigned_results.truncate(1);
            old.unassigned_results[0]
                .raw
                .as_object_mut()
                .context("raw evidence")?
                .remove("published_html");
            persist(&database, &target, RAW, &old).await?;
            let retained: i64 = sqlx::query_scalar("SELECT id FROM results")
                .fetch_one(&test.pool)
                .await?;
            persist(&database, &target, RAW, &sources::parse(&target, RAW)?).await?;
            let prior: (String, Option<String>) = sqlx::query_as(
                "SELECT source_result_id,raw#>>'{raw,revision_status}' FROM results WHERE id=$1",
            )
            .bind(retained)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(Some(prior.0), old.unassigned_results[0].result_id);
            assert_eq!(prior.1.as_deref(), Some("unverified_prior_revision"));
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 81);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_legacy_numbered_file_when_row_changes_then_prior_observation_is_quarantined()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = raw_target(true)?;
            let mut old = old_raw_page(&target, true)?;
            old.unassigned_results.truncate(1);
            persist(&database, &target, RAW, &old).await?;
            let prior: i64 = sqlx::query_scalar("SELECT id FROM results")
                .fetch_one(&test.pool)
                .await?;
            let changed = RAW.replacen("12:40.6", "12:45.6", 1);
            persist(
                &database,
                &target,
                &changed,
                &sources::parse(&target, &changed)?,
            )
            .await?;
            let evidence: (String, Option<String>) = sqlx::query_as(
                "SELECT raw_mark,raw#>>'{raw,revision_status}' FROM results WHERE id=$1",
            )
            .bind(prior)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(
                evidence,
                (
                    "12:40.6".to_owned(),
                    Some("unverified_prior_revision".to_owned())
                )
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 81);
            database.close().await;
            Ok(())
        })
    })
    .await
}
