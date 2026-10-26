mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    sources,
};

#[tokio::test]
async fn incomplete_rows_and_reclassified_neighbors_preserve_prior_evidence() -> Result<()> {
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
            let fields = row.as_object_mut().context("result object")?;
            fields.remove("IDResult");
            fields.remove("shortCode");
        }
        let first = rows.first_mut().and_then(Value::as_object_mut).context("first result")?;
        let event = first.remove("EventID").context("event ID")?;
        let mark = first.insert("Result".to_owned(), Value::String(String::new())).context("mark")?;
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "application/json".to_owned(), body: Vec::new() };
        let page = sources::parse(&target, &serde_json::to_string(&body)?)?;
        database.persist(None, &target, &capture, &page).await?;
        let incomplete: (String, String) = sqlx::query_as("SELECT event, raw_mark FROM results WHERE raw->>'source_row' LIKE 'resultsTF:0:content:%'")
            .fetch_one(&test.pool).await?;
        assert_eq!(incomplete, (String::new(), String::new()));
        let first = body.get_mut("resultsTF").and_then(Value::as_array_mut).and_then(|rows| rows.first_mut()).and_then(Value::as_object_mut).context("first result")?;
        first.insert("EventID".to_owned(), event);
        first.insert("Result".to_owned(), mark);
        let page = sources::parse(&target, &serde_json::to_string(&body)?)?;
        database.persist(None, &target, &capture, &page).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(count, 54);
        let rows: Vec<(String, bool)> = sqlx::query_as("SELECT raw_mark, athlete_id IS NOT NULL FROM results WHERE raw->>'source_row' LIKE 'resultsTF:0:content:%' OR raw->>'source_row' LIKE 'resultsTF:1:content:%' ORDER BY raw_mark")
            .fetch_all(&test.pool).await?;
        assert_eq!(rows, vec![(String::new(), false), ("11.48a".to_owned(), true), ("23.33a".to_owned(), true)]);
        database.close().await;
        Ok(())
    })).await
}
