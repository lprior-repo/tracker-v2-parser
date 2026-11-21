#[path = "review/support.rs"]
mod review_support;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{database::Database, export::ReviewRow};

async fn rows(database: &Database, state: &str) -> Result<Vec<ReviewRow>> {
    let mut snapshot = database
        .begin_export_snapshot(&[state.to_owned()], &[2028])
        .await?;
    let rows = snapshot.review_records(0, 1000).await?;
    let cursor = rows.last().map_or(0, |row| row.checkpoint_id);
    assert!(snapshot.review_records(cursor, 1000).await?.is_empty());
    snapshot.finish().await?;
    Ok(rows)
}

#[tokio::test]
async fn given_scoped_failures_when_selected_state_varies_in_case_or_whitespace_then_review_cells_match()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let first = review_support::failure(
                &database,
                &review_support::athlete("202801", Some("WI"))?,
                "failed",
                1,
                "Wisconsin request failed",
            )
            .await?;
            let second = review_support::failure(
                &database,
                &review_support::athlete("202802", Some("WI"))?,
                "blocked",
                0,
                "Wisconsin access stop",
            )
            .await?;
            review_support::failure(
                &database,
                &review_support::athlete("202803", Some("MI"))?,
                "failed",
                1,
                "Michigan request excluded",
            )
            .await?;
            let canonical = rows(&database, "WI").await?;
            assert_eq!(
                canonical
                    .iter()
                    .map(|row| row.checkpoint_id)
                    .collect::<Vec<_>>(),
                [first, second]
            );
            assert_eq!(
                canonical.first().context("first Wisconsin row")?.state,
                "WI"
            );
            let expected = canonical.iter().map(ReviewRow::cells).collect::<Vec<_>>();
            for state in ["wi", "\u{2003}\twI\r\u{00a0}"] {
                assert_eq!(
                    rows(&database, state)
                        .await?
                        .iter()
                        .map(ReviewRow::cells)
                        .collect::<Vec<_>>(),
                    expected,
                    "equivalent selected state {state:?} changed Review rows"
                );
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_unicode_padded_acquisition_state_when_reviewing_then_matching_checkpoint_is_retained()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut target = review_support::athlete("202804", Some("MI"))?;
            target.context.acquisition_state = Some("\u{2003}\tWi\r\u{00a0}".into());
            let id = review_support::failure(
                &database,
                &target,
                "failed",
                1,
                "Retained Wisconsin acquisition scope",
            )
            .await?;
            let selected = rows(&database, "WI").await?;
            assert_eq!(selected.len(), 1);
            let row = selected.first().context("padded acquisition row")?;
            assert_eq!(row.checkpoint_id, id);
            assert_eq!(row.state.trim(), "WI");
            assert_eq!(row.error, "Retained Wisconsin acquisition scope");
            assert!(rows(&database, "MI").await?.is_empty());
            database.close().await;
            Ok(())
        })
    })
    .await
}
