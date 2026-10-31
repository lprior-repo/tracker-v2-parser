#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{ParsedPage, Source},
};

async fn profile(
    database: &Database,
    source: Source,
    id: &str,
    link: Option<&str>,
    year: i32,
) -> Result<()> {
    let mut athlete = records::athlete(id);
    athlete.profile_url = Some(records::target(source, id).url);
    athlete.graduation_year = Some(year);
    athlete.same_as = link.into_iter().map(str::to_owned).collect();
    let mut performance = records::performance("11.00");
    performance.athlete_id = id.to_owned();
    records::persist(
        database,
        &records::target(source, id),
        &ParsedPage {
            athletes: vec![athlete],
            performances: vec![performance],
            ..ParsedPage::default()
        },
    )
    .await
}

async fn identities(database: &Database) -> Result<Vec<(String, String, i64)>> {
    Ok(
        sqlx::query_as("SELECT source,source_id,athlete_id FROM athlete_sources ORDER BY source")
            .fetch_all(database.pool())
            .await?,
    )
}

#[tokio::test]
async fn given_explicit_slugged_profile_link_when_replayed_then_numeric_identity_and_results_stay_unique()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        profile(&database, Source::MileSplit, "21", None, 2027).await?;
        let canonical = identities(&database).await?.first().context("original identity")?.2;
        for _ in 0..2 {
            profile(&database, Source::AthleticNet, "11", Some("https://wi.milesplit.com/athletes/21-alex-morgan"), 2027).await?;
        }
        assert_eq!(identities(&database).await?, [
            ("athleticnet".to_owned(), "11".to_owned(), canonical),
            ("milesplit".to_owned(), "21".to_owned(), canonical),
        ]);
        let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM athletes),(SELECT count(*) FROM results),(SELECT count(*) FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id WHERE r.athlete_id<>p.athlete_id),(SELECT count(*) FROM athletes WHERE needs_review)")
            .fetch_one(database.pool()).await?;
        assert_eq!(counts, (1, 2, 0, 0));
        let reports = database.athlete_records().await?;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].performances.len(), 2);
        assert_eq!(reports[0].profiles.len(), 2);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_conflicting_graduation_year_when_slugged_link_claims_same_person_then_profiles_stay_separate()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            profile(&database, Source::MileSplit, "21", None, 2027).await?;
            profile(
                &database,
                Source::AthleticNet,
                "11",
                Some("https://wi.milesplit.com/athletes/21-alex-morgan"),
                2028,
            )
            .await?;
            let people = identities(&database).await?;
            assert_eq!(people.len(), 2);
            assert_ne!(people[0].2, people[1].2);
            let reviewed: i64 =
                sqlx::query_scalar("SELECT count(*) FROM athletes WHERE needs_review")
                    .fetch_one(database.pool())
                    .await?;
            assert_eq!(reviewed, 2);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_malformed_explicit_profile_links_when_persisted_then_no_numeric_prefix_identity_is_guessed()
-> Result<()> {
    for suffix in ["21x-alex", "-21-alex", "21-", "21%2Dalex"] {
        support::with_schema(|test| {
            Box::pin(async move {
                let database = Database::connect(&test.url).await?;
                profile(&database, Source::MileSplit, "21", None, 2027).await?;
                let link = format!("https://wi.milesplit.com/athletes/{suffix}");
                profile(&database, Source::AthleticNet, "11", Some(&link), 2027).await?;
                let people = identities(&database).await?;
                assert_eq!(people.len(), 2);
                assert_ne!(people[0].2, people[1].2);
                let reviewed: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM athletes WHERE needs_review")
                        .fetch_one(database.pool())
                        .await?;
                assert_eq!(reviewed, 1);
                database.close().await;
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}
