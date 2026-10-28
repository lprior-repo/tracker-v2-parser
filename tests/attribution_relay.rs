mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::{database::Database, model::Capture, sources};

fn published_individual() -> Result<Value> {
    let mut body: Value =
        serde_json::from_str(include_str!("../fixtures/athleticnet/results-634313.json"))?;
    let block = body["flatEvents"]
        .as_array()
        .context("published events")?
        .iter()
        .find(|block| {
            block["results"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["IDResult"] == 292277081))
        })
        .context("published individual event")?;
    let row = block["results"]
        .as_array()
        .context("published rows")?
        .iter()
        .find(|row| row["IDResult"] == 292277081)
        .context("published individual row")?
        .clone();
    let mut block = block.clone();
    block["results"] = json!([row]);
    body["flatEvents"] = json!([block]);
    Ok(body)
}

#[tokio::test]
async fn given_published_result_id_when_event_is_corrected_to_relay_then_replay_clears_ownership()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let metadata = sources::parse(
            &sources::athleticnet::meet_target("634313", track_scrapper_v2::model::Sport::Outdoor)?,
            include_str!("../fixtures/athleticnet/meet-634313.json"),
        )?;
        let target = metadata.next.first().context("results target")?;
        let body = published_individual()?;
        let individual_body = body.to_string();
        let individual = sources::parse(target, &individual_body)?;
        assert_eq!(individual.performances.len(), 1);
        assert_eq!(individual.performances[0].result_id.as_deref(), Some("292277081"));
        assert_eq!(individual.performances[0].mark, "10.41a");
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "application/json".to_owned(), body: individual_body.into_bytes() };
        database.persist(None, target, &capture, &individual).await?;
        let before: (bool, bool, bool, bool) = sqlx::query_as("SELECT athlete_source_id IS NOT NULL, athlete_id IS NOT NULL, is_relay, normalized_numeric IS NOT NULL FROM results WHERE source_result_id='292277081'")
            .fetch_one(&test.pool).await?;
        assert_eq!(before, (true, true, false, true));
        let mut corrected = body;
        corrected["flatEvents"][0]["Event"] = json!("4x100 Relay");
        corrected["flatEvents"][0]["EventId"] = json!(7);
        corrected["flatEvents"][0]["isPersonal"] = json!(false);
        corrected["flatEvents"][0]["results"][0]["EventID"] = json!(7);
        corrected["flatEvents"][0]["results"][0]["PersonalEvent"] = json!(false);
        let relay_body = corrected.to_string();
        let relay = sources::parse(target, &relay_body)?;
        assert!(relay.performances.is_empty());
        assert!(relay.athletes.is_empty());
        assert_eq!(relay.unassigned_results.len(), 1);
        assert_eq!(relay.unassigned_results[0].result_id.as_deref(), Some("292277081"));
        let capture = Capture { body: relay_body.into_bytes(), ..capture };
        database.persist(None, target, &capture, &relay).await?;
        database.persist(None, target, &capture, &relay).await?;
        let after: (bool, bool, bool, String, Option<i64>, Option<String>) = sqlx::query_as("SELECT athlete_source_id IS NULL, athlete_id IS NULL, is_relay, raw_mark, normalized_numeric, normalized_unit FROM results WHERE source_result_id='292277081'")
            .fetch_one(&test.pool).await?;
        assert_eq!(after, (true, true, true, "10.41a".to_owned(), None, None));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
            .fetch_one(&test.pool).await?;
        assert_eq!(count, 1);
        database.close().await;
        Ok(())
    })).await
}
