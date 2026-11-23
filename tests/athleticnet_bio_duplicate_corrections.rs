mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, Sport, Target},
    sources::athleticnet,
};

fn individual(mark: &str) -> Value {
    json!({"IDResult":42,"AthleteID":382,"SchoolID":3204,"EventID":1,
        "EventTypeID":0,"SeasonID":2026,"MeetID":1000,
        "ResultDate":"2026-05-01T00:00:00","Result":mark,"ShortCode":"shared"})
}

fn capture(track: &[Value], cross_country: &[Value], legs: &[Value]) -> Value {
    json!({
        "athlete":{"IDAthlete":382,"SchoolID":3204,"FirstName":"Casey",
            "LastName":"Runner","Gender":"M","State":"MI"},
        "allTeams":{"3204":{"IDSchool":3204,"SchoolName":"Synthetic High","State":"MI"}},
        "allSeasons":[{"SchoolID":3204,"IDSeason":2026,"Display":"2026 Outdoor"}],
        "grades":{"3204_2026":10},
        "meets":{"1000":{"IDMeet":1000,"MeetName":"Synthetic Meet","EndDate":"2026-05-01T00:00:00"}},
        "eventsTF":[
            {"IDEvent":1,"IDEventType":0,"Event":"100 Meters","PersonalEvent":true},
            {"IDEvent":2,"IDEventType":0,"Event":"4x100 Relay","PersonalEvent":false}
        ],
        "resultsTF":track,"resultsXC":cross_country,"relayTeamMembers":legs
    })
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

async fn trusted(database: &Database) -> Result<(i64, Option<i64>, String, bool)> {
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

fn coordinate(key: &str, ordinal: usize, row: &Value) -> String {
    let digest = Sha256::digest(row.to_string().as_bytes());
    format!("{key}:{ordinal}:content:{digest:x}")
}

#[tokio::test]
async fn given_owned_bio_result_when_tf_native_id_repeats_then_neither_order_corrects_it()
-> Result<()> {
    for reverse in [false, true] {
        support::with_schema(move |test| {
            Box::pin(async move {
                let database = Database::connect(&test.url).await?;
                let target = athleticnet::athlete_target("382", Some("MI"), Sport::Outdoor)?;
                replay(
                    &database,
                    &target,
                    &capture(&[individual("11.20a")], &[], &[]),
                )
                .await?;
                let before = trusted(&database).await?;
                let mut rows = [individual("11.05a"), individual("11.92a")];
                rows[1]["IDResult"] = json!("42");
                if reverse {
                    rows.reverse();
                }
                let duplicate = capture(&rows, &[], &[]);
                replay(&database, &target, &duplicate).await?;
                assert_eq!(trusted(&database).await?, before);
                let retained = quarantined(&database).await?;
                assert_eq!(retained.len(), 2);
                for (ordinal, (stored, published)) in retained.iter().zip(&rows).enumerate() {
                    let mark = published["Result"].as_str().context("published mark")?;
                    assert_eq!(stored.1, mark);
                    assert_eq!(stored.3["raw"], *published);
                    assert_eq!(
                        stored.3["source_row"],
                        coordinate("resultsTF", ordinal, published)
                    );
                }
                replay(&database, &target, &duplicate).await?;
                assert_eq!(quarantined(&database).await?, retained);
                replay(
                    &database,
                    &target,
                    &capture(&[individual("11.01a")], &[], &[]),
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
async fn given_owned_bio_result_when_tf_and_xc_share_its_native_id_then_both_occurrences_are_quarantined()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = athleticnet::athlete_target("382", Some("MI"), Sport::Outdoor)?;
            replay(
                &database,
                &target,
                &capture(&[individual("11.20a")], &[], &[]),
            )
            .await?;
            let before = trusted(&database).await?;
            let track = individual("11.05a");
            let mut cross_country = individual("19:20.1");
            cross_country["IDResult"] = json!("42");
            cross_country["Distance"] = json!(5000);
            let duplicate = capture(
                std::slice::from_ref(&track),
                std::slice::from_ref(&cross_country),
                &[],
            );
            replay(&database, &target, &duplicate).await?;
            assert_eq!(trusted(&database).await?, before);
            let retained = quarantined(&database).await?;
            assert_eq!(retained.len(), 2);
            for (stored, (key, published, sport)) in retained.iter().zip([
                ("resultsTF", &track, "outdoor"),
                ("resultsXC", &cross_country, "cross_country"),
            ]) {
                assert_eq!(stored.3["source_row"], coordinate(key, 0, published));
                assert_eq!(stored.3["raw"], *published);
                assert_eq!(stored.3["sport"], sport);
            }
            replay(&database, &target, &duplicate).await?;
            assert_eq!(quarantined(&database).await?, retained);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_owned_bio_result_when_a_relay_reuses_its_id_then_raw_legs_survive_without_owner_demotion()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = athleticnet::athlete_target("382", Some("MI"), Sport::Outdoor)?;
            let row = individual("11.20a");
            replay(
                &database,
                &target,
                &capture(std::slice::from_ref(&row), &[], &[]),
            )
            .await?;
            let before = trusted(&database).await?;
            let mut relay = individual("43.12a");
            relay["EventID"] = json!(2);
            relay["AthleteID"] = json!(999);
            let legs = [
                json!({"TeamID":999,"AthleteID":382,"Name":"Casey Runner"}),
                json!({"TeamID":999,"Name":"Masked Runner •"}),
            ];
            let duplicate = capture(&[row.clone(), relay.clone()], &[], &legs);
            replay(&database, &target, &duplicate).await?;
            assert_eq!(trusted(&database).await?, before);
            let retained = quarantined(&database).await?;
            assert_eq!(retained.len(), 2);
            assert_ne!(retained[0].0, retained[1].0);
            assert_eq!(
                retained[0].3["source_row"],
                coordinate("resultsTF", 0, &row)
            );
            assert_eq!(retained[0].3["raw"], row);
            assert!(retained[1].2);
            assert_eq!(retained[1].1, "43.12a");
            assert_eq!(
                retained[1].3["source_row"],
                coordinate("resultsTF", 1, &relay)
            );
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
async fn given_masked_bio_identity_when_identical_native_ids_repeat_then_each_raw_occurrence_survives()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = athleticnet::athlete_target("382", Some("MI"), Sport::Outdoor)?;
            let row = individual("11.20a");
            let mut duplicate = capture(&[row.clone(), row.clone(), row.clone()], &[], &[]);
            duplicate["athlete"]["FirstName"] = json!("Hidden *");
            replay(&database, &target, &duplicate).await?;
            let retained = quarantined(&database).await?;
            assert_eq!(retained.len(), 3);
            for (ordinal, result) in retained.iter().enumerate() {
                assert_eq!(result.1, "11.20a");
                assert_eq!(result.3["raw"], row);
                assert_eq!(
                    result.3["source_row"],
                    coordinate("resultsTF", ordinal, &row)
                );
            }
            let people: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes")
                .fetch_one(database.pool())
                .await?;
            assert_eq!(people, 0);
            let total: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(database.pool())
                .await?;
            assert_eq!(total, 3);
            let schools: Vec<String> = sqlx::query_scalar(
                "SELECT s.source_id FROM results r JOIN schools s ON s.id=r.represented_school_id \
                 ORDER BY r.id",
            )
            .fetch_all(database.pool())
            .await?;
            assert_eq!(schools, ["3204", "3204", "3204"]);
            replay(&database, &target, &duplicate).await?;
            assert_eq!(quarantined(&database).await?, retained);
            database.close().await;
            Ok(())
        })
    })
    .await
}
