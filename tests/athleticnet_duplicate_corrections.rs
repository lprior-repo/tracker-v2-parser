mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, Sport, Target},
    sources::athleticnet,
};

fn target() -> Result<Target> {
    athleticnet::parse(
        &athleticnet::meet_target("634313", Sport::Outdoor)?,
        include_str!("../fixtures/athleticnet/meet-634313.json"),
    )?
    .next
    .into_iter()
    .next()
    .context("meet results target")
}

fn individual(mark: &str) -> Value {
    json!({"IDResult":42,"AthleteID":382,"FirstName":"Casey","LastName":"Runner",
        "Result":mark,"TeamID":3204,"SchoolName":"Middleton","ShortCode":"shared"})
}

fn block(rows: &[Value]) -> Value {
    json!({"Event":"100 Meters","Gender":"M","isPersonal":true,"results":rows})
}

fn capture(blocks: &[Value], legs: &[Value]) -> Value {
    json!({"teams":[],"flatEvents":blocks,"relayLegs":legs})
}

async fn replay(database: &Database, target: &Target, body: &Value) -> Result<()> {
    let encoded = body.to_string();
    let page = athleticnet::parse(target, &encoded)?;
    database
        .persist(
            None,
            target,
            &Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "application/json".to_owned(),
                body: encoded.into_bytes(),
            },
            &page,
        )
        .await?;
    Ok(())
}

async fn trusted(database: &Database) -> Result<(i64, i64, String, bool)> {
    Ok(sqlx::query_as(
        "SELECT id,athlete_source_id,raw_mark,is_relay FROM results WHERE source_result_id='42'",
    )
    .fetch_one(database.pool())
    .await?)
}

async fn quarantined(database: &Database) -> Result<Vec<(String, String, bool, Value)>> {
    Ok(sqlx::query_as(
        "SELECT source_result_key,raw_mark,is_relay,raw FROM results \
         WHERE source_result_id IS NULL AND athlete_source_id IS NULL AND athlete_id IS NULL \
         ORDER BY raw->>'source_row'",
    )
    .fetch_all(database.pool())
    .await?)
}

fn coordinate(block: usize, ordinal: usize, row: &Value) -> String {
    let digest = Sha256::digest(row.to_string().as_bytes());
    format!("flatEvents:{block}:results:{ordinal}:content:{digest:x}")
}

#[tokio::test]
async fn given_owned_result_when_conflicting_marks_repeat_its_native_id_then_neither_order_corrects_it()
-> Result<()> {
    for reverse in [false, true] {
        support::with_schema(move |test| {
            Box::pin(async move {
                let database = Database::connect(&test.url).await?;
                let target = target()?;
                replay(
                    &database,
                    &target,
                    &capture(&[block(&[individual("11.20a")])], &[]),
                )
                .await?;
                let before = trusted(&database).await?;
                let mut rows = [individual("11.05a"), individual("11.92a")];
                rows[1]["IDResult"] = json!("42");
                if reverse {
                    rows.reverse();
                }
                let duplicate = capture(&[block(&rows)], &[]);
                replay(&database, &target, &duplicate).await?;
                assert_eq!(trusted(&database).await?, before);
                let retained = quarantined(&database).await?;
                assert_eq!(retained.len(), 2);
                for (ordinal, (retained, published)) in retained.iter().zip(&rows).enumerate() {
                    let mark = published["Result"].as_str().context("published mark")?;
                    assert_eq!(retained.1, mark);
                    assert_eq!(retained.3["raw"], *published);
                    assert_eq!(retained.3["source_row"], coordinate(0, ordinal, published));
                }
                replay(&database, &target, &duplicate).await?;
                assert_eq!(quarantined(&database).await?, retained);
                replay(
                    &database,
                    &target,
                    &capture(&[block(&[individual("11.01a")])], &[]),
                )
                .await?;
                assert_eq!(
                    trusted(&database).await?,
                    (before.0, before.1, "11.01a".to_owned(), false)
                );
                database.close().await;
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_owned_result_when_a_second_flat_block_reuses_its_id_then_raw_relay_legs_survive_without_demotion()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target()?;
            let row = individual("11.20a");
            replay(
                &database,
                &target,
                &capture(&[block(std::slice::from_ref(&row))], &[]),
            )
            .await?;
            let before = trusted(&database).await?;
            let relay = individual("43.12a");
            let legs = [
                json!({"ResultID":42,"AthleteID":382,"Name":"Casey Runner","ShortDesc":"11"}),
                json!({"ResultID":42,"Name":"Masked Runner •"}),
            ];
            let relay_block = json!({"Event":"4x100 Relay","Gender":"M","isPersonal":false,
                "results":[relay]});
            let duplicate = capture(&[block(std::slice::from_ref(&row)), relay_block], &legs);
            replay(&database, &target, &duplicate).await?;
            assert_eq!(trusted(&database).await?, before);
            let retained = quarantined(&database).await?;
            assert_eq!(retained.len(), 2);
            assert_ne!(retained[0].0, retained[1].0);
            assert_eq!(retained[0].3["source_row"], coordinate(0, 0, &row));
            assert_eq!(retained[0].3["raw"], row);
            assert!(retained[1].2);
            assert_eq!(retained[1].1, "43.12a");
            assert_eq!(retained[1].3["source_row"], coordinate(1, 0, &relay));
            assert_eq!(retained[1].3["raw"], json!({"row":relay,"relayLegs":legs}));
            assert_eq!(retained[1].3["members"], json!(["athleticnet:382"]));
            replay(&database, &target, &duplicate).await?;
            assert_eq!(quarantined(&database).await?, retained);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_identical_published_rows_when_native_id_and_shortcode_repeat_then_each_occurrence_is_retained()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target()?;
            let row = individual("11.20a");
            let duplicate = capture(&[block(&[row.clone(), row.clone(), row.clone()])], &[]);
            replay(&database, &target, &duplicate).await?;
            let retained = quarantined(&database).await?;
            assert_eq!(retained.len(), 3);
            for (ordinal, result) in retained.iter().enumerate() {
                assert_eq!(result.1, "11.20a");
                assert_eq!(result.3["raw"], row);
                assert_eq!(result.3["source_row"], coordinate(0, ordinal, &row));
            }
            let total: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(database.pool())
                .await?;
            assert_eq!(total, 3);
            replay(&database, &target, &duplicate).await?;
            assert_eq!(quarantined(&database).await?, retained);
            database.close().await;
            Ok(())
        })
    })
    .await
}
