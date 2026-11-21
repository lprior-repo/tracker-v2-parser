mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, Sport},
    sources,
};

async fn seed(database: &Database, id: &str, masked: bool) -> Result<i64> {
    let target = sources::milesplit::athlete_target(id, Some("WI"), Sport::Outdoor)?;
    let mark = if masked {
        "<span class='mask'>**</span>"
    } else {
        "11.25"
    };
    let html = format!(
        "<html><head><title>Review Fixture - MileSplit</title><link rel='canonical' href='{}'></head><body>\
         <h1 id='athleteName'>Review Athlete {id}</h1>\
         <div class='athlete-info'><span class='current-school'><a href='/teams/9{id}-fixture'>Published School</a></span>\
         <span class='city-state'>Madison, WI</span><span class='grad-year'>Class of 2028</span></div>\
         <table id='prTable'><tr data-pr-mode='all' data-pr-level='all'>\
         <td class='pr-td-event'>100m</td><td class='pr-td-mark'>{mark}</td></tr></table>\
         <section id='progression' class='progression-cta'>Join MileSplit PRO</section></body></html>",
        target.url
    );
    let page = sources::parse(&target, &html)?;
    assert!(page.performances.is_empty());
    let evidence = page
        .athletes
        .first()
        .context("parsed athlete")?
        .source_values
        .as_ref()
        .context("retained source evidence")?;
    assert_eq!(
        evidence["pages"][&target.url]["pr_rows"][0]["masked"],
        masked
    );
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".into(),
        body: html.into_bytes(),
    };
    database.persist(None, &target, &capture, &page).await
}

#[tokio::test]
async fn given_only_masked_prs_and_restricted_progression_when_reviewing_then_public_marks_are_not_inferred()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let masked_id = seed(&database, "202805", true).await?;
            seed(&database, "202806", false).await?;
            let mut snapshot = database.begin_export_snapshot(&["WI".into()], &[2028]).await?;
            let rows = snapshot.review_records(0, 1000).await?;
            assert_eq!(rows.iter().map(|row| row.checkpoint_id).collect::<Vec<_>>(), [masked_id]);
            let row = rows.first().context("masked-only source availability review")?;
            assert_eq!(row.athlete_source_id, "202805");
            assert_eq!(row.state, "WI");
            assert_eq!(row.error, "No public PR/result rows in captured profile evidence; source progression is restricted. This does not establish career absence.");
            assert!(snapshot.review_records(masked_id, 1000).await?.is_empty());
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_public_headlines_with_legacy_mask_metadata_when_reviewing_then_only_boolean_true_is_masked()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for (index, mask) in [
                None,
                Some(Value::Null),
                Some(json!(false)),
                Some(json!("true")),
                Some(json!([true])),
                Some(json!({"value": true})),
            ].into_iter().enumerate() {
                let id = (202810 + index).to_string();
                seed(&database, &id, false).await?;
                let target = sources::milesplit::athlete_target(&id, Some("WI"), Sport::Outdoor)?;
                sqlx::query(
                    "UPDATE athlete_sources SET details=CASE WHEN $3::jsonb IS NULL \
                     THEN details #- ARRAY['source_values','pages',$2,'pr_rows','0','masked'] \
                     ELSE jsonb_set(details,ARRAY['source_values','pages',$2,'pr_rows','0','masked'],$3) END \
                     WHERE source='milesplit' AND source_id=$1",
                )
                .bind(&id)
                .bind(&target.url)
                .bind(mask)
                .execute(database.pool())
                .await?;
            }
            let mut snapshot = database.begin_export_snapshot(&["WI".into()], &[2028]).await?;
            assert!(snapshot.review_records(0, 1000).await?.is_empty());
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}
