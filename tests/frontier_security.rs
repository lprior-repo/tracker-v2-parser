mod support;

use anyhow::Result;
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, School, Source, Target},
};

fn target(url: &str) -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Athlete,
        url.to_owned(),
        PageContext::default(),
    )
}

#[tokio::test]
async fn invalid_seed_and_refresh_batches_leave_frontier_unchanged() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let existing = target("https://www.athletic.net/athlete/1");
        let new = target("https://www.athletic.net/athlete/2");
        let invalid = target("https://www.athletic.net/athlete/3?access_token=secret-value");

        assert!(database.enqueue(&[new.clone(), invalid.clone()], false).await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages")
            .fetch_one(&test.pool).await?;
        assert_eq!(count, 0);

        database.enqueue(std::slice::from_ref(&existing), false).await?;
        sqlx::query("UPDATE scrape_pages SET status='succeeded', attempts=2")
            .execute(&test.pool).await?;
        assert!(database.enqueue(&[existing, new, invalid], true).await.is_err());
        let rows: Vec<(String, String, i32)> = sqlx::query_as(
            "SELECT url, status, attempts FROM scrape_pages ORDER BY url",
        ).fetch_all(&test.pool).await?;
        assert_eq!(rows, vec![("https://www.athletic.net/athlete/1".to_owned(), "succeeded".to_owned(), 2)]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn invalid_discovery_rolls_back_domain_and_checkpoint_writes() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let current = target("https://www.athletic.net/athlete/1");
        let capture = Capture {
            url: current.url.clone(),
            status: 200,
            content_type: "application/json".to_owned(),
            body: Vec::new(),
        };
        let page = ParsedPage {
            schools: vec![School { id: "school-1".to_owned(), name: "Example School".to_owned(), ..School::default() }],
            next: vec![
                target("https://www.athletic.net/athlete/2"),
                target("https://www.athletic.net/athlete/3?%74oken=secret-value"),
            ],
            ..ParsedPage::default()
        };
        let error = database.persist(None, &current, &capture, &page).await.expect_err("secret-bearing discovery must be rejected");
        assert!(error.to_string().contains("must not carry authentication secrets"));
        let pages: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages").fetch_one(&test.pool).await?;
        let schools: i64 = sqlx::query_scalar("SELECT count(*) FROM schools").fetch_one(&test.pool).await?;
        assert_eq!((pages, schools), (0, 0));

        let invalid_current = target("https://www.athletic.net/athlete/1?password=secret-value");
        assert!(database.persist(None, &invalid_current, &capture, &ParsedPage::default()).await.is_err());
        let pages: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages").fetch_one(&test.pool).await?;
        assert_eq!(pages, 0);
        database.close().await;
        Ok(())
    })).await
}
