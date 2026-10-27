mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    sources,
};

#[tokio::test]
async fn replaying_a_relay_removes_prior_individual_attribution() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = Target::get(
            Source::MileSplit,
            PageKind::Results,
            "https://oh.milesplit.com/meets/770621/results/1321880/raw".to_owned(),
            PageContext { sport: Some(Sport::CrossCountry), ..PageContext::default() },
        );
        let body = include_str!("../fixtures/milesplit/oh_meet_770621_rs1321880_raw.html")
            .replacen("Jeydyn Fields", "<a href='/athletes/123456'>Jeydyn Fields</a>", 1);
        let relay_body = body.replacen("Boys Middle School 3000 Meter", "Boys Middle School 4x400 Meter Relay", 1);
        let relay_page = sources::parse(&target, &relay_body)?;
        let relay = relay_page.unassigned_results.first().context("relay result")?;
        let result_id = relay.result_id.as_deref().context("relay result ID")?;
        let mut legacy = sources::parse(&target, &body)?;
        let performance = legacy.performances.first_mut().context("linked performance")?;
        performance.event = relay.event.clone();
        performance.result_id = relay.result_id.clone();
        performance.is_relay = true;
        legacy.unassigned_results.clear();
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "text/html".to_owned(), body: Vec::new() };
        database.persist(None, &target, &capture, &legacy).await?;
        let before: bool = sqlx::query_scalar("SELECT athlete_source_id IS NOT NULL FROM results WHERE source_result_id=$1")
            .bind(result_id).fetch_one(&test.pool).await?;
        assert!(before);
        database.persist(None, &target, &capture, &relay_page).await?;
        database.persist(None, &target, &capture, &relay_page).await?;
        let after: (bool, bool, bool, String) = sqlx::query_as("SELECT athlete_source_id IS NULL, athlete_id IS NULL, is_relay, raw_mark FROM results WHERE source_result_id=$1")
            .bind(result_id).fetch_one(&test.pool).await?;
        assert_eq!(after, (true, true, true, "12:40.6".to_owned()));
        database.close().await;
        Ok(())
    })).await
}
