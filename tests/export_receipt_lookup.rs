#[path = "export_receipt_lookup/support.rs"]
mod lookup;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::database::{Database, EXPORT_BATCH_ROWS};

#[tokio::test]
async fn given_a_late_synthetic_result_cursor_when_exporting_receipt_scoped_issues_then_exact_rows_require_bounded_receipt_work()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            lookup::seed_workload(database.pool()).await?;
            let plan = lookup::late_plan(database.pool()).await?;
            let (work, indexed) = lookup::receipt_work(&plan)?;
            assert!(
                indexed,
                "late Coverage page did not probe the capture URL hash index"
            );
            assert!(
                work <= 202_000.0,
                "late Coverage page examined {work} receipt tuples"
            );

            let expected = (lookup::LATE_CURSOR.saturating_add(1)..=lookup::RESULTS)
                .filter(|id| id % 5 <= 1)
                .map(|id| {
                    let url = lookup::capture_url(id);
                    let bytes = i64::try_from(
                        ["milesplit", &url, lookup::DETAIL]
                            .iter()
                            .map(|text| text.len())
                            .sum::<usize>(),
                    )?;
                    Ok((
                        String::from("milesplit"),
                        url,
                        String::from(lookup::DETAIL),
                        2,
                        id,
                        0,
                        bytes,
                        true,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(expected.len(), 200);
            assert_eq!(lookup::late_issues(database.pool()).await?, expected);

            let mut snapshot = database
                .begin_export_snapshot(&["WI".into()], &[2028])
                .await?;
            for pages in [50_001..=52_500, 52_501..=55_000] {
                let issues = snapshot
                    .next_issues()
                    .await?
                    .context("scoped checkpoint batch")?;
                let expected = pages
                    .filter(|id| id % 5 <= 1)
                    .map(|id| format!("https://wi.milesplit.com/athletes/{id}/stats"));
                assert_eq!(issues.len(), EXPORT_BATCH_ROWS);
                assert!(issues.iter().map(|issue| issue.url.as_str()).eq(expected));
                assert!(issues.iter().all(|issue| issue.source == "milesplit"
                    && issue.detail == "page failed: Synthetic scope fallback failure"));
            }
            let issues = snapshot
                .next_issues()
                .await?
                .context("retained result batch")?;
            let expected = (1..)
                .filter(|id| id % 5 <= 1)
                .take(EXPORT_BATCH_ROWS)
                .map(lookup::capture_url);
            assert_eq!(issues.len(), EXPORT_BATCH_ROWS);
            assert!(issues.iter().map(|issue| issue.url.as_str()).eq(expected));
            assert!(
                issues
                    .iter()
                    .all(|issue| issue.source == "milesplit" && issue.detail == lookup::DETAIL)
            );
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_long_synthetic_capture_urls_when_lookup_migration_installs_then_receipts_remain_exact_and_duplicates_do_not_duplicate_issues()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            lookup::migrate_prior(&test.pool).await?;
            let exact = lookup::long_url(&test.pool).await?;
            assert!((7_800..7_950).contains(&exact.len()));
            let wrong_source = format!("{exact}&source=other");
            let unretained = format!("{exact}&source=missing");
            let later = format!("{exact}&source=later");
            lookup::retain_capture(&test.pool, "exact-one", "milesplit", "WI", &exact).await?;
            lookup::retain_capture(&test.pool, "exact-two", "milesplit", "WI", &exact).await?;
            lookup::retain_capture(
                &test.pool,
                "other-source",
                "athleticnet",
                "WI",
                &wrong_source,
            )
            .await?;
            lookup::retain_capture(
                &test.pool,
                "nonmatching-suffix",
                "milesplit",
                "WI",
                &format!("{unretained}-suffix"),
            )
            .await?;
            let before = lookup::retained_receipts(&test.pool).await?;
            for (label, url) in [
                ("exact", &exact),
                ("other-source", &wrong_source),
                ("missing", &unretained),
            ] {
                lookup::unassigned_result(&test.pool, label, url).await?;
            }

            let database = Database::connect(&test.url).await?;
            assert_eq!(lookup::retained_receipts(database.pool()).await?, before);
            lookup::retain_capture(database.pool(), "later", "milesplit", "MI", &later).await?;
            lookup::unassigned_result(database.pool(), "later", &later).await?;
            lookup::assert_public_issues(&database, &["WI".into()], &[&exact]).await?;
            lookup::assert_public_issues(&database, &["MI".into()], &[&later]).await?;
            lookup::assert_public_issues(&database, &["OH".into()], &[]).await?;
            lookup::assert_public_issues(
                &database,
                &[],
                &[&exact, &wrong_source, &unretained, &later],
            )
            .await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}
