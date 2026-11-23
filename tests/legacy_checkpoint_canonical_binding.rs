mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use track_scrapper_v2::{
    config::Config,
    database::Database,
    model::{PageContext, PageKind, Source, Sport, Target},
    runner::Runner,
};

#[tokio::test]
async fn given_pending_implicit_get_when_public_runner_replays_explicit_get_then_the_same_checkpoint_succeeds()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut target = Target::get(
                Source::AthleticNet,
                PageKind::Athlete,
                "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0".into(),
                PageContext {
                    state: Some("WI".into()),
                    athlete_id: Some("28872883".into()),
                    season: Some(2026),
                    sport: Some(Sport::Outdoor),
                    ..PageContext::default()
                },
            );
            assert_eq!(database.enqueue(std::slice::from_ref(&target), false).await?, 1);
            let before = checkpoint(test, &target.url).await?;
            assert_eq!((&*before.1, before.2), ("pending", 0));
            let url = target.url.clone();
            target.method = Some("GET".into());
            Runner::new()
                .replay(
                    &Config::default(),
                    &test.url,
                    &support::fixture("athleticnet/bio-28872883.json"),
                    target,
                    &test.directory().join("evidence"),
                )
                .await
                .context("public Runner replay must resolve the retained canonical GET checkpoint")?;
            assert_eq!(checkpoint(test, &url).await?, (before.0, "succeeded".into(), 1, before.3));
            let results: (i64, i64) = sqlx::query_as(
                "SELECT count(*),count(*) FILTER (WHERE athlete_id IS NOT NULL) FROM results",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(results, (53, 41));
            Ok(())
        })
    })
    .await
}

async fn checkpoint(test: &support::Scenario, url: &str) -> Result<(i64, String, i32, Value)> {
    let rows: Vec<_> = sqlx::query_as(
        "SELECT id,status,attempts,cursor FROM scrape_pages WHERE source='athleticnet' AND url=$1",
    )
    .bind(url)
    .fetch_all(&test.pool)
    .await?;
    assert_eq!(rows.len(), 1, "exactly one checkpoint for the fixture");
    rows.into_iter()
        .next()
        .context("missing fixture checkpoint")
}
