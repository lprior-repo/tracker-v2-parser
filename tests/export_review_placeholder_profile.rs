#[path = "review/support.rs"]
mod review_support;
mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use serde_json::Value;
use track_scrapper_v2::{
    database::{Database, EXPORT_BATCH_BYTES, ExportLimitExceeded, ExportSnapshotInvalidated},
    export::{ExportLayout, ExportOptions, ReviewRow, ReviewStatus},
    model::{Capture, Sport},
    sources,
};

async fn seed(database: &Database, id: &str, mode: &str, mark: &str) -> Result<i64> {
    let target = sources::milesplit::athlete_target(id, Some("WI"), Sport::Outdoor)?;
    let remaining = if mode == "all" {
        ""
    } else {
        "<td></td><td></td><td></td>"
    };
    let html = format!(
        "<html><head><title>Placeholder Fixture - MileSplit</title><link rel='canonical' href='{}'></head><body>\
         <h1 id='athleteName'>Placeholder Athlete {id}</h1>\
         <div class='athlete-info'><span class='current-school'><a href='/teams/9{id}-fixture'>Published School</a></span>\
         <span class='city-state'>Madison, WI</span><span class='grad-year'>Class of 2028</span></div>\
         <table id='prTable'><tr data-pr-mode='{mode}' data-pr-level='hs'>\
         <td class='pr-td-event'>100m</td><td class='pr-td-mark'>{mark}</td>{remaining}</tr></table>\
         <section id='progression' class='progression-cta'>Join MileSplit PRO</section></body></html>",
        target.url
    );
    let parsed = sources::parse(&target, &html)?;
    assert_eq!(
        parsed.performances.len(),
        usize::from(mode != "all" && mark == "11.25")
    );
    let values = parsed
        .athletes
        .first()
        .context("parsed athlete")?
        .source_values
        .as_ref()
        .context("retained profile evidence")?;
    let rows = values["pages"][&target.url]["pr_rows"]
        .as_array()
        .context("retained PR rows")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["masked"], false);
    assert!(
        rows[0]["html"]
            .as_str()
            .context("retained PR HTML")?
            .contains("pr-td-mark")
    );
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".into(),
        body: html.into_bytes(),
    };
    database.persist(None, &target, &capture, &parsed).await
}

async fn source_state(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object(\
         'profiles',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM athlete_sources p),\
         'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r),\
         'pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),\
         'requests',(SELECT count(*) FROM durable_wire_requests))",
    )
    .fetch_one(database.pool())
    .await?)
}

async fn legacy_mask(database: &Database, id: &str, mask: Option<Value>) -> Result<()> {
    let target = sources::milesplit::athlete_target(id, Some("WI"), Sport::Outdoor)?;
    let changed = sqlx::query(
        "UPDATE athlete_sources SET details=CASE WHEN $3::jsonb IS NULL \
         THEN details #- ARRAY['source_values','pages',$2,'pr_rows','0','masked'] \
         ELSE jsonb_set(details,ARRAY['source_values','pages',$2,'pr_rows','0','masked'],$3) END \
         WHERE source='milesplit' AND source_id=$1",
    )
    .bind(id)
    .bind(&target.url)
    .bind(mask)
    .execute(database.pool())
    .await?;
    assert_eq!(changed.rows_affected(), 1);
    Ok(())
}

async fn verify_workbook(
    test: &support::Scenario,
    database: &Database,
    review: &[ReviewRow],
) -> Result<()> {
    let path = test.directory().join("placeholder-review.xlsx");
    let options = ExportOptions {
        states: vec!["WI".into()],
        graduation_years: vec![2028],
        rows_per_file: 1000,
        per_athlete_sheets: false,
    };
    let written = database
        .write_export_with_review(&path, &options, ExportLayout::SingleSheet, true)
        .await?;
    assert_eq!(written, [path.clone()]);
    let mut workbook = open_workbook_auto(path)?;
    assert_eq!(workbook.sheet_names(), ["Athletes", "Review"]);
    let actual = workbook
        .worksheet_range("Review")?
        .rows()
        .skip(1)
        .map(|row| row.iter().map(ToString::to_string).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        review.iter().map(ReviewRow::cells).collect::<Vec<_>>()
    );
    let athletes = workbook.worksheet_range("Athletes")?;
    assert_eq!(athletes.height(), 11);
    let headers = athletes.rows().next().context("Athletes headers")?;
    let marks = headers
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value.to_string().contains(" PR [").then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(marks.len(), 2);
    for row in athletes.rows().skip(1) {
        let name = row.first().context("athlete name")?.to_string();
        let values = marks
            .iter()
            .map(|index| row.get(*index).map_or(String::new(), ToString::to_string))
            .collect::<Vec<_>>();
        if name.ends_with("202899") || name.ends_with("202898") {
            assert_eq!(
                values
                    .iter()
                    .filter(|value| value.as_str() == "11.25")
                    .count(),
                1
            );
        } else {
            assert!(
                values.iter().all(String::is_empty),
                "placeholder became a published mark: {name} {values:?}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn given_unmasked_placeholders_with_restricted_progression_when_exporting_then_missing_public_marks_remain_reviewable()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut expected = Vec::new();
            for (index, (mode, mark)) in [
                ("all", ""), ("all", "--"), ("all", "—"), ("all", "***"),
                ("outdoor", ""), ("outdoor", "--"), ("outdoor", "—"), ("outdoor", "***"),
            ].into_iter().enumerate()
            {
                let id = (202820 + index).to_string();
                expected.push(seed(&database, &id, mode, mark).await?);
            }
            seed(&database, "202898", "all", "11.25").await?;
            seed(&database, "202899", "outdoor", "11.25").await?;
            let before = source_state(&database).await?;
            assert_eq!(before["requests"], 0);
            let mut snapshot = database.begin_export_snapshot(&["WI".into()], &[2028]).await?;
            let review = snapshot.review_records(0, 1000).await?;
            assert_eq!(review.iter().map(|row| row.checkpoint_id).collect::<Vec<_>>(), expected);
            assert!(review.iter().all(|row| row.error == "No public PR/result rows in captured profile evidence; source progression is restricted. This does not establish career absence."));
            assert!(snapshot.review_records(review.last().context("last review row")?.checkpoint_id, 1000).await?.is_empty());
            snapshot.finish().await?;
            verify_workbook(test, &database, &review).await?;
            assert_eq!(source_state(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_public_pr_batches_around_legacy_placeholders_when_reviewing_one_row_then_every_missing_mark_and_failure_is_returned()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for id in ["202900", "202901", "202902"] {
                seed(&database, id, "all", "11.25").await?;
            }
            let missing_mask = seed(&database, "202903", "all", "--").await?;
            legacy_mask(&database, "202903", None).await?;
            for id in ["202904", "202905"] {
                seed(&database, id, "all", "11.25").await?;
            }
            let null_mask = seed(&database, "202906", "all", "***").await?;
            legacy_mask(&database, "202906", Some(Value::Null)).await?;
            seed(&database, "202907", "all", "11.25").await?;
            let failed_target = review_support::athlete("202908", Some("WI"))?;
            let failed = review_support::failure(
                &database,
                &failed_target,
                "failed",
                1,
                "Fixture acquisition failure",
            )
            .await?;
            for id in ["202909", "202910"] {
                seed(&database, id, "all", "11.25").await?;
            }
            let before = source_state(&database).await?;
            assert_eq!(before["requests"], 0);
            let mut snapshot = database
                .begin_export_snapshot(&["WI".into()], &[2028])
                .await?;
            let mut cursor = 0;
            let mut paged = Vec::new();
            for expected in [missing_mask, null_mask, failed] {
                let rows = snapshot.review_records(cursor, 1).await?;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].checkpoint_id, expected);
                cursor = expected;
                paged.extend(rows);
            }
            assert!(snapshot.review_records(cursor, 1).await?.is_empty());
            assert_eq!(snapshot.review_records(0, 1000).await?, paged);
            assert_eq!(snapshot.review_records(0, 2).await?, paged[..2]);
            assert_eq!(snapshot.review_records(null_mask, 2).await?, paged[2..]);
            assert_eq!(
                paged
                    .iter()
                    .map(|row| row.athlete_source_id.as_str())
                    .collect::<Vec<_>>(),
                ["202903", "202906", "202908"]
            );
            assert_eq!(
                paged.iter().map(|row| row.status).collect::<Vec<_>>(),
                [
                    ReviewStatus::Succeeded,
                    ReviewStatus::Succeeded,
                    ReviewStatus::Failed
                ]
            );
            assert_eq!(paged[2].error, "Fixture acquisition failure");
            snapshot.finish().await?;
            assert_eq!(source_state(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_oversized_retained_profile_when_review_inspects_public_marks_then_original_record_guard_and_invalidation_remain_active()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(&database, "202920", "all", "11.25").await?;
            sqlx::query("UPDATE athlete_sources SET details=jsonb_set(details,'{source_values,retained_padding}',to_jsonb(repeat('x',$1))) WHERE source_id='202920'")
                .bind(i32::try_from(EXPORT_BATCH_BYTES)?)
                .execute(database.pool()).await?;
            let before = source_state(&database).await?;
            let mut snapshot = database.begin_export_snapshot(&["WI".into()], &[2028]).await?;
            let error = snapshot.review_records(0, 1).await.err().context("oversized original profile must fail before projected PR parsing")?;
            let limit = error.downcast_ref::<ExportLimitExceeded>().context("typed athlete byte guard")?;
            assert_eq!(limit.kind, "athlete serialized bytes");
            assert_eq!(limit.limit, i64::try_from(EXPORT_BATCH_BYTES)?);
            assert!(limit.actual > limit.limit);
            let error = snapshot.review_records(0, 1).await.err().context("failed Review snapshot must not resume")?;
            assert!(error.downcast_ref::<ExportSnapshotInvalidated>().is_some());
            let error = snapshot.next_records(1).await.err().context("failed Review snapshot must not change categories")?;
            assert!(error.downcast_ref::<ExportSnapshotInvalidated>().is_some());
            drop(snapshot);
            assert_eq!(source_state(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}
