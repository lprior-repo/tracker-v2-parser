#[path = "review/acquisition_expected.rs"]
mod expected;
#[path = "review/support.rs"]
mod review_support;
mod support;

use std::path::Path;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::{database::Database, export::ReviewRow, runner::Runner};

fn sheet(path: &Path, name: &str) -> Result<Vec<Vec<String>>> {
    let mut workbook = open_workbook_auto(path)?;
    Ok(workbook
        .worksheet_range(name)?
        .rows()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect())
}

async fn state(database: &Database) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(d) ORDER BY operation_id) FROM durable_page_operations d),'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r))")
        .fetch_one(database.pool()).await?)
}

#[tokio::test]
async fn legacy_review_matches_all_eighteen_expected_cells_and_preserves_athletes_and_database()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let ids = review_support::seed(&database).await?;
            let mi = review_support::athlete("99", Some("MI"))?;
            let excluded = review_support::failure(
                &database,
                &mi,
                "failed",
                2,
                "Michigan failure excluded from WI workbook",
            )
            .await?;
            let config = review_support::config();
            let before = state(&database).await?;
            let mut snapshot = database
                .begin_export_snapshot(&config.states, &config.graduation_years)
                .await?;
            let captured = snapshot.review_records(0, 1000).await?;
            snapshot.finish().await?;
            let literal = expected::rows(&ids);
            assert_eq!(
                captured.iter().map(ReviewRow::cells).collect::<Vec<_>>(),
                literal
            );
            assert!(!captured.iter().any(|row| row.checkpoint_id == excluded));
            let enabled = test.directory().join("with-review.xlsx");
            Runner::new().export(&config, &enabled, &test.url).await?;
            assert_eq!(
                open_workbook_auto(&enabled)?.sheet_names(),
                ["Athletes", "Review"]
            );
            let actual = sheet(&enabled, "Review")?;
            assert_eq!(
                actual.first().context("header")?,
                &expected::HEADERS.map(str::to_owned).to_vec()
            );
            assert_eq!(actual.into_iter().skip(1).collect::<Vec<_>>(), literal);
            let mut default_config = config.clone();
            default_config.export.review_sheet = false;
            let disabled = test.directory().join("without-review.xlsx");
            Runner::new()
                .export(&default_config, &disabled, &test.url)
                .await?;
            assert_eq!(open_workbook_auto(&disabled)?.sheet_names(), ["Athletes"]);
            let athletes = sheet(&enabled, "Athletes")?;
            assert_eq!(athletes, sheet(&disabled, "Athletes")?);
            assert_eq!(athletes.len(), 2);
            assert!(athletes[1].iter().any(|cell| cell == "Alex Morgan"));
            assert!(athletes[1].iter().any(|cell| cell == "11.00 (2026-05-15)"));
            assert_eq!(state(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn paged_review_keeps_one_repeatable_snapshot_while_checkpoint_and_retry_facts_change()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let ids=review_support::seed(&database).await?;
        let config=review_support::config();
        let mut snapshot=database.begin_export_snapshot(&config.states,&config.graduation_years).await?;
        let mut rows=snapshot.review_records(0,2).await?;
        assert_eq!(rows.len(),2);
        sqlx::query("UPDATE scrape_pages SET last_error='Changed after snapshot',attempts=attempts+1 WHERE status IN ('failed','blocked')").execute(database.pool()).await?;
        sqlx::query("UPDATE durable_page_operations SET state='exhausted',retry_at_ms=NULL").execute(database.pool()).await?;
        for _ in 0..3 {
            let cursor=rows.last().context("review cursor")?.checkpoint_id;
            let next=snapshot.review_records(cursor,2).await?;
            rows.extend(next);
        }
        assert_eq!(rows.iter().map(ReviewRow::cells).collect::<Vec<_>>(),expected::rows(&ids));
        snapshot.finish().await?;
        let mut fresh=database.begin_export_snapshot(&config.states,&config.graduation_years).await?;
        let changed=fresh.review_records(0,1000).await?;
        assert_eq!(changed.len(),5);
        assert!(changed.iter().all(|row|row.error=="Changed after snapshot"));
        let deferred=changed.iter().find(|row|row.athlete_source_id=="13").context("retry checkpoint")?;
        assert_eq!(deferred.durable_state,"exhausted");
        assert!(deferred.retry_after.is_empty());
        fresh.finish().await?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn legacy_empty_review_is_header_only_and_default_false_export_has_only_athletes()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut config = review_support::config();
            assert!(
                !track_scrapper_v2::config::Config::default()
                    .export
                    .review_sheet
            );
            let enabled = test.directory().join("empty-review.xlsx");
            Runner::new().export(&config, &enabled, &test.url).await?;
            assert_eq!(
                open_workbook_auto(&enabled)?.sheet_names(),
                ["Athletes", "Review"]
            );
            assert_eq!(
                sheet(&enabled, "Review")?,
                vec![expected::HEADERS.map(str::to_owned).to_vec()]
            );
            config.export.review_sheet = false;
            assert!(
                serde_json::to_value(&config)?["export"]
                    .get("review_sheet")
                    .is_none()
            );
            let disabled = test.directory().join("empty-default.xlsx");
            Runner::new().export(&config, &disabled, &test.url).await?;
            assert_eq!(open_workbook_auto(&disabled)?.sheet_names(), ["Athletes"]);
            assert_eq!(sheet(&enabled, "Athletes")?, sheet(&disabled, "Athletes")?);
            database.close().await;
            Ok(())
        })
    })
    .await
}
