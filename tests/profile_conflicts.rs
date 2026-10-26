#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::Result;
use records::{athlete, performance, persist, target};
use serde_json::json;
use track_scrapper_v2::{
    database::Database,
    model::{Athlete, ParsedPage, Source},
};

#[tokio::test]
async fn reserved_link_with_later_conflicting_profile_is_separated() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let first = Athlete {
            same_as: vec!["https://www.milesplit.com/athletes/21".to_owned()],
            ..athlete("11")
        };
        persist(&database, &target(Source::AthleticNet, "11"), &ParsedPage {
            athletes: vec![first], performances: vec![performance("11.00")], ..ParsedPage::default()
        }).await?;
        let before: i64 = sqlx::query_scalar("SELECT count(DISTINCT athlete_id) FROM athlete_sources")
            .fetch_one(&test.pool).await?;
        assert_eq!(before, 1);
        let second = Athlete { graduation_year: Some(2028), ..athlete("21") };
        let mut result = performance("12.00");
        result.athlete_id = "21".to_owned();
        persist(&database, &target(Source::MileSplit, "21"), &ParsedPage {
            athletes: vec![second], performances: vec![result], ..ParsedPage::default()
        }).await?;
        let rows: Vec<(String, i64, i32, bool)> = sqlx::query_as(
            "SELECT p.source,a.id,a.graduation_year,a.needs_review FROM athlete_sources p JOIN athletes a ON a.id=p.athlete_id ORDER BY p.source"
        ).fetch_all(&test.pool).await?;
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].1, rows[1].1);
        assert_eq!((rows[0].2, rows[0].3), (2027, true));
        assert_eq!((rows[1].2, rows[1].3), (2028, true));
        let misplaced: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id WHERE r.athlete_id<>p.athlete_id"
        ).fetch_one(&test.pool).await?;
        assert_eq!(misplaced, 0);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn later_conflict_moves_existing_results_and_does_not_remerge_on_replay() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let first = Athlete {
            same_as: vec!["https://www.milesplit.com/athletes/21".to_owned()],
            ..athlete("11")
        };
        persist(&database, &target(Source::AthleticNet, "11"), &ParsedPage {
            athletes: vec![first], ..ParsedPage::default()
        }).await?;
        let mut result = performance("12.00");
        result.athlete_id = "21".to_owned();
        let compatible = athlete("21");
        persist(&database, &target(Source::MileSplit, "21"), &ParsedPage {
            athletes: vec![compatible], performances: vec![result], ..ParsedPage::default()
        }).await?;
        let conflicting = Athlete {
            graduation_year: Some(2028),
            same_as: vec!["https://www.athletic.net/athlete/11".to_owned()],
            ..athlete("21")
        };
        let page = ParsedPage { athletes: vec![conflicting], ..ParsedPage::default() };
        persist(&database, &target(Source::MileSplit, "21"), &page).await?;
        persist(&database, &target(Source::MileSplit, "21"), &page).await?;
        let sparse = Athlete {
            id: "21".to_owned(),
            name: "Alex Morgan".to_owned(),
            same_as: vec!["https://www.athletic.net/athlete/11".to_owned()],
            ..Athlete::default()
        };
        persist(&database, &target(Source::MileSplit, "21"), &ParsedPage {
            athletes: vec![sparse], ..ParsedPage::default()
        }).await?;
        let identities: i64 = sqlx::query_scalar("SELECT count(DISTINCT athlete_id) FROM athlete_sources")
            .fetch_one(&test.pool).await?;
        assert_eq!(identities, 2);
        let moved: bool = sqlx::query_scalar(
            "SELECT r.athlete_id=p.athlete_id FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id WHERE p.source='milesplit'"
        ).fetch_one(&test.pool).await?;
        assert!(moved);
        let reviewed: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes WHERE needs_review")
            .fetch_one(&test.pool).await?;
        assert_eq!(reviewed, 2);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn sparse_profile_replay_preserves_published_details_and_links() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let first = Athlete {
                gender: Some("female".to_owned()),
                grade: Some("10".to_owned()),
                profile_url: Some("https://www.athletic.net/athlete/11".to_owned()),
                same_as: vec!["https://www.milesplit.com/athletes/21".to_owned()],
                ..athlete("11")
            };
            let target = target(Source::AthleticNet, "11");
            persist(
                &database,
                &target,
                &ParsedPage {
                    athletes: vec![first],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let sparse = Athlete {
                id: "11".to_owned(),
                name: "Alex Morgan".to_owned(),
                ..Athlete::default()
            };
            persist(
                &database,
                &target,
                &ParsedPage {
                    athletes: vec![sparse],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let details: serde_json::Value = sqlx::query_scalar(
                "SELECT details FROM athlete_sources WHERE source='athleticnet'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(details["gender"], json!("female"));
            assert_eq!(details["grade"], json!("10"));
            assert_eq!(details["graduation_year"], json!(2027));
            assert_eq!(
                details["profile_url"],
                json!("https://www.athletic.net/athlete/11")
            );
            assert_eq!(
                details["same_as"],
                json!(["https://www.milesplit.com/athletes/21"])
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
