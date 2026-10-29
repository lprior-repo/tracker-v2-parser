#[path = "review/support.rs"]
mod review_support;
mod support;

use std::path::Path;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::{
    database::Database,
    durable::operations::{
        OperationSpec, Workspace, freeze_export, frozen_plan, publish_export, render_export,
        verify_export,
    },
    export::{ExportLayout, ExportOptions, ReviewRow, prepare_chunk, verify_chunks_with_review},
    runner::Runner,
};

fn rows(path: &Path, sheet: &str) -> Result<Vec<Vec<String>>> {
    let mut workbook = open_workbook_auto(path)?;
    Ok(workbook
        .worksheet_range(sheet)?
        .rows()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect())
}

fn write_tampered(
    path: &Path,
    athlete_rows: &[Vec<String>],
    review_rows: &[Vec<String>],
) -> Result<()> {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    [("Athletes", athlete_rows), ("Review", review_rows)]
        .into_iter()
        .try_for_each(|(name, rows)| -> Result<()> {
            let sheet = workbook.add_worksheet().set_name(name)?;
            rows.iter()
                .enumerate()
                .try_for_each(|(r, cells)| -> Result<()> {
                    cells
                        .iter()
                        .enumerate()
                        .try_for_each(|(c, value)| -> Result<()> {
                            sheet.write_string(u32::try_from(r)?, u16::try_from(c)?, value)?;
                            Ok(())
                        })
                })
        })?;
    workbook.save(path)?;
    Ok(())
}

#[tokio::test]
async fn review_freezes_scoped_failures_and_retry_state_and_matches_legacy_export_cells()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let ids = review_support::seed(&database).await?;
            let config = review_support::config();
            let mut snapshot = database
                .begin_export_snapshot(&config.states, &config.graduation_years)
                .await?;
            let expected = snapshot.review_records(0, 1000).await?;
            snapshot.finish().await?;
            assert_eq!(
                expected
                    .iter()
                    .map(|row| row.checkpoint_id)
                    .collect::<Vec<_>>(),
                ids
            );
            assert_eq!(
                (
                    &expected[0].school,
                    &expected[0].school_source_id,
                    &expected[0].athlete,
                    &expected[0].athlete_source_id
                ),
                (
                    &"Published School".into(),
                    &"1000".into(),
                    &"Alex Morgan".into(),
                    &"11".into()
                )
            );
            assert_eq!(expected[0].error, "DNS resolution failed");
            assert_eq!(expected[1].attempts, 0);
            assert_eq!(
                expected[1].next_action,
                "Human handling required; preserve the access stop."
            );
            assert_eq!(expected[2].durable_state, "deferred");
            assert_eq!(expected[2].retry_after, "2026-10-29T13:00:00.000000Z");
            assert!(
                expected[2]
                    .next_action
                    .starts_with("Wait for automatic retry")
            );
            assert!(
                expected[2]
                    .ai_guidance
                    .starts_with("Wait for the automatic retry")
            );
            let legacy = test.directory().join("legacy.xlsx");
            Runner::new().export(&config, &legacy, &test.url).await?;
            let workspace = Workspace::open(
                &test.directory().join("artifacts"),
                "review-export",
                &OperationSpec::Export {
                    config: config.clone(),
                },
            )?;
            let frozen = freeze_export(&workspace, &config, &test.url).await?;
            assert_eq!(frozen_plan(&workspace, &frozen)?.review_rows, Some(5));
            sqlx::query(
                "UPDATE scrape_pages SET last_error='Changed after freeze',status='succeeded'",
            )
            .execute(database.pool())
            .await?;
            sqlx::query("UPDATE durable_page_operations SET state='exhausted',retry_at_ms=NULL")
                .execute(database.pool())
                .await?;
            let resumed = freeze_export(&workspace, &config, &test.url).await?;
            assert_eq!(resumed.manifest, frozen.manifest);
            let workbook = render_export(&workspace, &frozen)?;
            let verified = verify_export(&workspace, &frozen, &workbook)?;
            assert_eq!((verified.rows, verified.review_rows), (1, Some(5)));
            assert_eq!(
                rows(&workbook.path, "Athletes")?,
                rows(&legacy, "Athletes")?
            );
            assert_eq!(rows(&workbook.path, "Review")?, rows(&legacy, "Review")?);
            let actual = rows(&workbook.path, "Review")?;
            assert_eq!(
                actual[1..],
                expected.iter().map(ReviewRow::cells).collect::<Vec<_>>()
            );
            assert_eq!(actual[0][11..13], ["Durable state", "Retry after UTC"]);
            assert_eq!(
                open_workbook_auto(&workbook.path)?.sheet_names(),
                ["Athletes", "Review"]
            );
            let mut changed = actual.clone();
            changed[1][13] = "Tampered review error".into();
            let tampered = test.directory().join("tampered-review.xlsx");
            write_tampered(&tampered, &rows(&workbook.path, "Athletes")?, &changed)?;
            let records = database
                .athlete_records_for(&config.states, &config.graduation_years)
                .await?;
            let chunk = prepare_chunk(
                &records,
                &ExportOptions {
                    states: config.states.clone(),
                    graduation_years: config.graduation_years.clone(),
                    rows_per_file: 1000,
                    per_athlete_sheets: false,
                },
            )?;
            let error = verify_chunks_with_review(&tampered, &[chunk], Some(&expected))
                .err()
                .context("Review cell tampering must fail")?;
            assert!(error.to_string().contains("Review cell differs"));
            let publication = publish_export(&workspace, &verified)?;
            assert_eq!(
                publish_export(&workspace, &verified)?.workbook,
                publication.workbook
            );
            assert_eq!(render_export(&workspace, &frozen)?, workbook);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn enabled_review_is_header_only_when_empty_and_default_export_stays_one_sheet() -> Result<()>
{
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut config = review_support::config();
            for (enabled, name) in [(true, "enabled"), (false, "disabled")] {
                config.export.review_sheet = enabled;
                let serialized = serde_json::to_value(&config)?;
                assert_eq!(serialized["export"].get("review_sheet").is_some(), enabled);
                let workspace = Workspace::open(
                    &test.directory().join("artifacts"),
                    name,
                    &OperationSpec::Export {
                        config: config.clone(),
                    },
                )?;
                let frozen = freeze_export(&workspace, &config, &test.url).await?;
                let workbook = render_export(&workspace, &frozen)?;
                assert_eq!(
                    verify_export(&workspace, &frozen, &workbook)?.review_rows,
                    enabled.then_some(0)
                );
                let expected = if enabled {
                    vec!["Athletes", "Review"]
                } else {
                    vec!["Athletes"]
                };
                assert_eq!(open_workbook_auto(&workbook.path)?.sheet_names(), expected);
                if enabled {
                    assert_eq!(rows(&workbook.path, "Review")?.len(), 1);
                }
            }
            config.export.review_sheet = true;
            config.export.layout = ExportLayout::Detailed;
            assert!(config.validate().is_err());
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn review_removes_credentials_and_raw_payloads_and_rejects_oversized_rows_before_publication()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let target=review_support::athlete("99",Some("WI"))?;
        let id=review_support::failure(&database,&target,"failed",1,"password=private-value").await?;
        sqlx::query("UPDATE scrape_pages SET url='https://private-user:private-pass@wi.milesplit.com/athletes/99?token=private-token&view=results' WHERE id=$1")
            .bind(id).execute(database.pool()).await?;
        let config=review_support::config();
        let mut snapshot=database.begin_export_snapshot(&config.states,&config.graduation_years).await?;
        let review=snapshot.review_records(0,1000).await?;
        snapshot.finish().await?;
        assert_eq!(review.len(),1);
        assert_eq!(review[0].url,"https://wi.milesplit.com/athletes/99?view=results");
        assert!(review[0].error.contains("Sensitive request details omitted"));
        assert!(!serde_json::to_string(&review)?.contains("private-"));
        sqlx::query("UPDATE scrape_pages SET last_error='<html>raw response</html>' WHERE id=$1").bind(id).execute(database.pool()).await?;
        let mut snapshot=database.begin_export_snapshot(&config.states,&config.graduation_years).await?;
        assert!(snapshot.review_records(0,1000).await?[0].error.contains("Response payload omitted"));
        snapshot.finish().await?;
        sqlx::query("UPDATE scrape_pages SET last_error=repeat('x',70000) WHERE id=$1").bind(id).execute(database.pool()).await?;
        let workspace=Workspace::open(&test.directory().join("artifacts"),"oversized-review",&OperationSpec::Export{config:config.clone()})?;
        let error=freeze_export(&workspace,&config,&test.url).await.err().context("oversized review must fail")?;
        assert!(format!("{error:#}").contains("64 KiB"));
        assert!(!workspace.path("frozen-export.json")?.exists());
        assert!(!workspace.path("publication-receipt.json")?.exists());
        database.close().await;
        Ok(())
    })).await
}
