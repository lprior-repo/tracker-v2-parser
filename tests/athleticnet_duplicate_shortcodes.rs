mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, Sport, Target},
    sources::athleticnet,
};

fn idless(mark: &str) -> Value {
    json!({"AthleteID":382,"FirstName":"Casey","LastName":"Runner",
        "SchoolID":3204,"TeamID":3204,"SchoolName":"Synthetic High",
        "EventID":1,"EventTypeID":0,"SeasonID":2026,"MeetID":1000,
        "ResultDate":"2026-05-01T00:00:00","Result":mark,"ShortCode":"shared"})
}

fn bio(track: &[Value], cross_country: &[Value]) -> Value {
    json!({
        "athlete":{"IDAthlete":382,"SchoolID":3204,"FirstName":"Casey",
            "LastName":"Runner","Gender":"M","State":"MI"},
        "allTeams":{"3204":{"IDSchool":3204,"SchoolName":"Synthetic High","State":"MI"}},
        "allSeasons":[{"SchoolID":3204,"IDSeason":2026,"Display":"2026 Outdoor"}],
        "grades":{"3204_2026":10},
        "meets":{"1000":{"IDMeet":1000,"MeetName":"Synthetic Meet","EndDate":"2026-05-01T00:00:00"}},
        "eventsTF":[{"IDEvent":1,"IDEventType":0,"Event":"100 Meters","PersonalEvent":true}],
        "resultsTF":track,"resultsXC":cross_country
    })
}

fn block(rows: &[Value]) -> Value {
    json!({"Event":"100 Meters","Gender":"M","isPersonal":true,"results":rows})
}

fn meet(blocks: &[Value]) -> Value {
    json!({"teams":[],"flatEvents":blocks,"relayLegs":[]})
}

fn meet_target() -> Result<Target> {
    athleticnet::parse(
        &athleticnet::meet_target("634313", Sport::Outdoor)?,
        include_str!("../fixtures/athleticnet/meet-634313.json"),
    )?
    .next
    .into_iter()
    .next()
    .context("meet results target")
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

async fn trusted_code(database: &Database) -> Result<(i64, Option<i64>, String)> {
    Ok(sqlx::query_as(
        "SELECT id,athlete_source_id,raw_mark FROM results \
         WHERE source_result_id IS NULL AND raw->>'source_row'='shortCode:shared'",
    )
    .fetch_one(database.pool())
    .await?)
}

async fn positional(database: &Database) -> Result<Vec<(String, String, String, Value)>> {
    Ok(sqlx::query_as(
        "SELECT r.source_result_key,p.source_id,r.raw_mark,r.raw FROM results r \
         JOIN athlete_sources p ON p.id=r.athlete_source_id \
         WHERE r.source_result_id IS NULL AND r.raw->>'source_row'<>'shortCode:shared' \
         ORDER BY r.raw->>'source_row'",
    )
    .fetch_all(database.pool())
    .await?)
}

fn coordinate(prefix: &str, row: &Value) -> String {
    let digest = Sha256::digest(row.to_string().as_bytes());
    format!("{prefix}:content:{digest:x}")
}

#[tokio::test]
async fn given_owned_bio_shortcode_when_idless_code_repeats_then_each_occurrence_survives_without_correction()
-> Result<()> {
    for reverse in [false, true] {
        support::with_schema(move |test| {
            Box::pin(async move {
                let database = Database::connect(&test.url).await?;
                let target = athleticnet::athlete_target("382", Some("MI"), Sport::Outdoor)?;
                replay(&database, &target, &bio(&[idless("11.20a")], &[])).await?;
                let before = trusted_code(&database).await?;
                let mut rows = [idless("11.05a"), idless("11.92a")];
                rows[1]
                    .as_object_mut()
                    .context("result object")?
                    .remove("ShortCode");
                rows[1]["shortCode"] = json!("shared");
                if reverse {
                    rows.reverse();
                }
                let duplicate = bio(&rows, &[]);
                replay(&database, &target, &duplicate).await?;
                assert_eq!(trusted_code(&database).await?, before);
                let retained = positional(&database).await?;
                assert_eq!(retained.len(), 2);
                for (ordinal, (stored, row)) in retained.iter().zip(&rows).enumerate() {
                    assert_eq!(stored.1, "382");
                    assert_eq!(stored.2, row["Result"].as_str().context("mark")?);
                    assert_eq!(stored.3["source_values"]["row"], *row);
                    assert_eq!(
                        stored.3["source_row"],
                        coordinate(&format!("resultsTF:{ordinal}"), row)
                    );
                }
                replay(&database, &target, &duplicate).await?;
                assert_eq!(positional(&database).await?, retained);
                replay(&database, &target, &bio(&[idless("11.01a")], &[])).await?;
                assert_eq!(
                    trusted_code(&database).await?,
                    (before.0, before.1, "11.01a".into())
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
async fn given_owned_bio_shortcode_when_tf_and_xc_reuse_it_without_ids_then_both_histories_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = athleticnet::athlete_target("382", Some("MI"), Sport::Outdoor)?;
            replay(&database, &target, &bio(&[idless("11.20a")], &[])).await?;
            let before = trusted_code(&database).await?;
            let track = idless("11.05a");
            let mut cross_country = idless("19:20.1");
            cross_country["Distance"] = json!(5000);
            let duplicate = bio(
                std::slice::from_ref(&track),
                std::slice::from_ref(&cross_country),
            );
            replay(&database, &target, &duplicate).await?;
            assert_eq!(trusted_code(&database).await?, before);
            let retained = positional(&database).await?;
            assert_eq!(retained.len(), 2);
            for (stored, (prefix, row)) in retained
                .iter()
                .zip([("resultsTF:0", &track), ("resultsXC:0", &cross_country)])
            {
                assert_eq!(stored.3["source_row"], coordinate(prefix, row));
                assert_eq!(stored.3["source_values"]["row"], *row);
            }
            replay(&database, &target, &duplicate).await?;
            assert_eq!(positional(&database).await?, retained);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_owned_meet_shortcode_when_idless_athletes_share_it_across_blocks_then_page_commits_both_owners()
-> Result<()> {
    for reverse in [false, true] {
        support::with_schema(move |test| {
            Box::pin(async move {
                let database = Database::connect(&test.url).await?;
                let target = meet_target()?;
                replay(&database, &target, &meet(&[block(&[idless("11.20a")])])).await?;
                let before = trusted_code(&database).await?;
                let mut rows = [idless("11.05a"), idless("10.80a")];
                rows[1]["AthleteID"] = json!(383);
                rows[1]["FirstName"] = json!("Jordan");
                rows[1]["LastName"] = json!("Sprinter");
                if reverse {
                    rows.reverse();
                }
                let duplicate = meet(&[
                    block(std::slice::from_ref(&rows[0])),
                    block(std::slice::from_ref(&rows[1])),
                ]);
                replay(&database, &target, &duplicate).await?;
                assert_eq!(trusted_code(&database).await?, before);
                let retained = positional(&database).await?;
                assert_eq!(retained.len(), 2);
                for (ordinal, (stored, row)) in retained.iter().zip(&rows).enumerate() {
                    assert_eq!(stored.1, row["AthleteID"].to_string());
                    assert_eq!(stored.2, row["Result"].as_str().context("mark")?);
                    assert_eq!(stored.3["source_values"]["row"], *row);
                    assert_eq!(
                        stored.3["source_row"],
                        coordinate(&format!("flatEvents:{ordinal}:results:0"), row)
                    );
                }
                replay(&database, &target, &duplicate).await?;
                assert_eq!(positional(&database).await?, retained);
                replay(&database, &target, &meet(&[block(&[idless("11.01a")])])).await?;
                assert_eq!(
                    trusted_code(&database).await?,
                    (before.0, before.1, "11.01a".into())
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
async fn given_distinct_native_ids_with_shared_shortcode_when_corrected_and_reordered_then_native_rows_update()
-> Result<()> {
    for is_meet in [false, true] {
        support::with_schema(move |test| {
            Box::pin(async move {
                let database = Database::connect(&test.url).await?;
                let target = if is_meet {
                    meet_target()?
                } else {
                    athleticnet::athlete_target("382", Some("MI"), Sport::Outdoor)?
                };
                let body = |rows: &[Value]| if is_meet { meet(&[block(rows)]) } else { bio(rows, &[]) };
                let mut rows = [idless("11.20a"), idless("11.92a")];
                rows[0]["IDResult"] = json!(101);
                rows[1]["IDResult"] = json!(102);
                replay(&database, &target, &body(&rows)).await?;
                let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM results ORDER BY source_result_id")
                    .fetch_all(database.pool()).await?;
                assert_eq!(ids.len(), 2);
                rows[0]["Result"] = json!("11.01a");
                rows.reverse();
                replay(&database, &target, &body(&rows)).await?;
                let actual: Vec<(i64, String, String, String)> = sqlx::query_as(
                    "SELECT id,source_result_id,raw_mark,raw->>'source_row' FROM results ORDER BY source_result_id",
                ).fetch_all(database.pool()).await?;
                assert_eq!(actual, vec![
                    (ids[0], "101".into(), "11.01a".into(), "shortCode:shared".into()),
                    (ids[1], "102".into(), "11.92a".into(), "shortCode:shared".into()),
                ]);
                database.close().await;
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}
async fn check_mixed_shortcode_replay(is_meet: bool) -> Result<()> {
    for reverse in [false, true] {
        support::with_schema(move |test| {
            Box::pin(async move {
                let database = Database::connect(&test.url).await?;
                let target = if is_meet {
                    meet_target()?
                } else {
                    athleticnet::athlete_target("382", Some("MI"), Sport::Outdoor)?
                };
                let body = |rows: &[Value]| {
                    if is_meet {
                        meet(&[block(rows)])
                    } else {
                        bio(rows, &[])
                    }
                };
                replay(&database, &target, &body(&[idless("11.20a")])).await?;
                let before = trusted_code(&database).await?;
                let mut rows = [idless("11.05a"), idless("11.92a")];
                rows[0]["IDResult"] = json!(101);
                if is_meet {
                    rows[1]["AthleteID"] = json!(383);
                    rows[1]["FirstName"] = json!("Jordan");
                    rows[1]["LastName"] = json!("Sprinter");
                }
                if reverse {
                    rows.reverse();
                }
                let duplicate = body(&rows);
                replay(&database, &target, &duplicate).await?;
                assert_eq!(trusted_code(&database).await?, before);
                let native: (String, String) = sqlx::query_as(
                    "SELECT raw_mark,raw->>'source_row' FROM results WHERE source_result_id='101'",
                )
                .fetch_one(database.pool())
                .await?;
                assert_eq!(native, ("11.05a".into(), "shortCode:shared".into()));
                let retained = positional(&database).await?;
                assert_eq!(retained.len(), 1);
                let (ordinal, row) = rows
                    .iter()
                    .enumerate()
                    .find(|(_, row)| row.get("IDResult").is_none())
                    .context("ID-less occurrence")?;
                let prefix = if is_meet {
                    format!("flatEvents:0:results:{ordinal}")
                } else {
                    format!("resultsTF:{ordinal}")
                };
                assert_eq!(retained[0].1, if is_meet { "383" } else { "382" });
                assert_eq!(retained[0].2, "11.92a");
                assert_eq!(retained[0].3["source_values"]["row"], *row);
                assert_eq!(retained[0].3["source_row"], coordinate(&prefix, row));
                replay(&database, &target, &duplicate).await?;
                assert_eq!(trusted_code(&database).await?, before);
                assert_eq!(positional(&database).await?, retained);
                let total: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                    .fetch_one(database.pool())
                    .await?;
                assert_eq!(total, 3);
                database.close().await;
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_native_and_idless_bio_rows_sharing_shortcode_when_replayed_then_trusted_fallback_survives()
-> Result<()> {
    check_mixed_shortcode_replay(false).await
}

#[tokio::test]
async fn given_native_and_idless_meet_rows_sharing_shortcode_when_replayed_then_both_owners_commit()
-> Result<()> {
    check_mixed_shortcode_replay(true).await
}
