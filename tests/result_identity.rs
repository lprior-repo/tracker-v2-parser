mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    sources,
};

#[tokio::test]
async fn published_row_codes_survive_source_array_reordering() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = Target::get(
            Source::AthleticNet,
            PageKind::Athlete,
            "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0".to_owned(),
            PageContext { athlete_id: Some("28872883".to_owned()), sport: Some(Sport::Outdoor), ..PageContext::default() },
        );
        let bytes = std::fs::read(support::fixture("athleticnet/bio-28872883.json"))?;
        let mut body: Value = serde_json::from_slice(&bytes)?;
        let rows = body.get_mut("resultsTF").and_then(Value::as_array_mut).context("resultsTF")?;
        for row in rows.iter_mut().take(2) {
            row.as_object_mut().context("result object")?.remove("IDResult");
        }
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "application/json".to_owned(), body: Vec::new() };
        let page = sources::parse(&target, &serde_json::to_string(&body)?)?;
        database.persist(None, &target, &capture, &page).await?;
        let rows = body.get_mut("resultsTF").and_then(Value::as_array_mut).context("resultsTF")?;
        rows.swap(0, 1);
        rows.get_mut(1).and_then(Value::as_object_mut).context("moved result")?.insert("Result".to_owned(), Value::String("23.22a".to_owned()));
        let page = sources::parse(&target, &serde_json::to_string(&body)?)?;
        database.persist(None, &target, &capture, &page).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(count, 53);
        let mark: String = sqlx::query_scalar("SELECT raw_mark FROM results WHERE raw->>'source_row' = 'shortCode:BMiAAK8U3FOM3Apcg'")
            .fetch_one(&test.pool).await?;
        assert_eq!(mark, "23.22a");
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn meet_codes_survive_reordering_and_keep_legacy_coordinate_rows() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = Target::get(
            Source::AthleticNet,
            PageKind::Results,
            "https://www.athletic.net/api/v1/Meet/GetAllResultsData?meetId=634313&sport=tf&rawResults=false&showTips=false".to_owned(),
            PageContext { subject_id: Some("634313".to_owned()), sport: Some(Sport::Outdoor), ..PageContext::default() },
        );
        let bytes = std::fs::read(support::fixture("athleticnet/results-634313.json"))?;
        let mut body: Value = serde_json::from_slice(&bytes)?;
        let rows = body["flatEvents"][0]["results"].as_array_mut().context("results")?;
        for row in rows.iter_mut().take(2) {
            row.as_object_mut().context("result object")?.remove("IDResult");
        }
        let code = rows[0].as_object_mut().context("first result")?.remove("ShortCode").context("ShortCode")?;
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "application/json".to_owned(), body: Vec::new() };
        let page = sources::parse(&target, &serde_json::to_string(&body)?)?;
        database.persist(None, &target, &capture, &page).await?;
        let legacy_key = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                Source::AthleticNet,
                "source_row",
                target.url.as_str(),
                "flatEvents:0:results:0",
            ))?)
        );
        sqlx::query("UPDATE results SET source_result_key = $1, raw = jsonb_set(raw, '{source_row}', to_jsonb('flatEvents:0:results:0'::text)) WHERE raw->>'source_row' LIKE 'flatEvents:0:results:0:content:%'")
            .bind(legacy_key)
            .execute(&test.pool)
            .await?;
        let rows = body["flatEvents"][0]["results"].as_array_mut().context("results")?;
        rows[0].as_object_mut().context("first result")?.insert("ShortCode".to_owned(), code);
        rows.swap(0, 1);
        rows[0].as_object_mut().context("moved result")?.insert("Result".to_owned(), Value::String("10.42a".to_owned()));
        let page = sources::parse(&target, &serde_json::to_string(&body)?)?;
        database.persist(None, &target, &capture, &page).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(count, 759);
        let mark: String = sqlx::query_scalar("SELECT raw_mark FROM results WHERE raw->>'source_row' = 'shortCode:gBiw1KaiNiVKKErUA'")
            .fetch_one(&test.pool).await?;
        assert_eq!(mark, "10.42a");
        let legacy: i64 = sqlx::query_scalar("SELECT count(*) FROM results WHERE raw->>'source_row' = 'flatEvents:0:results:0'")
            .fetch_one(&test.pool).await?;
        assert_eq!(legacy, 1);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn idless_rows_without_codes_survive_reordering_without_overwriting() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = Target::get(
            Source::AthleticNet,
            PageKind::Athlete,
            "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0".to_owned(),
            PageContext { athlete_id: Some("28872883".to_owned()), sport: Some(Sport::Outdoor), ..PageContext::default() },
        );
        let bytes = std::fs::read(support::fixture("athleticnet/bio-28872883.json"))?;
        let mut body: Value = serde_json::from_slice(&bytes)?;
        let rows = body.get_mut("resultsTF").and_then(Value::as_array_mut).context("resultsTF")?;
        for row in rows.iter_mut().take(2) {
            let object = row.as_object_mut().context("result object")?;
            object.remove("IDResult");
            object.remove("shortCode");
            object.insert("Result".to_owned(), Value::String("11.48a".to_owned()));
        }
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "application/json".to_owned(), body: Vec::new() };
        let page = sources::parse(&target, &serde_json::to_string(&body)?)?;
        database.persist(None, &target, &capture, &page).await?;
        let rows = body.get_mut("resultsTF").and_then(Value::as_array_mut).context("resultsTF")?;
        rows.swap(0, 1);
        let page = sources::parse(&target, &serde_json::to_string(&body)?)?;
        database.persist(None, &target, &capture, &page).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(count, 55);
        let events: Vec<String> = sqlx::query_scalar("SELECT DISTINCT event FROM results WHERE raw_mark = '11.48a' ORDER BY event")
            .fetch_all(&test.pool).await?;
        assert!(events.contains(&"100 Meters".to_owned()));
        assert!(events.contains(&"200 Meters".to_owned()));
        database.close().await;
        Ok(())
    })).await
}
