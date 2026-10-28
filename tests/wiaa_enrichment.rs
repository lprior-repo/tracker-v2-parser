mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::model::{PageContext, PageKind, Source, Target};

fn target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=42".into(),
        PageContext {
            school_id: Some("provider-school-123".into()),
            school_name: Some("Example".into()),
            state: Some("WI".into()),
            season: Some(2026),
            ..PageContext::default()
        },
    )
}

#[tokio::test]
async fn given_preserved_wiaa_access_stop_when_enrichment_is_requested_then_it_stops_before_network_and_preserves_evidence()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            use track_scrapper_v2::{config::Config, database::Database};
            let database = Database::connect(&test.url).await?;
            let target = target();
            database
                .enqueue(std::slice::from_ref(&target), false)
                .await?;
            let job = database
                .next_job(Some(Source::MileSplit), 3, &[])
                .await?
                .context("WIAA checkpoint")?;
            assert!(database.start_attempt(job.id, 3).await?);
            database
                .fail_job(&job, None, "saved WIAA access challenge", true)
                .await?;
            let before: Vec<serde_json::Value> =
                sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id")
                    .fetch_all(&test.pool)
                    .await?;
            let error = track_scrapper_v2::wiaa_enrichment::enrich(&Config::default(), &test.url)
                .await
                .expect_err("preserved authority stop");
            assert!(error.to_string().contains("preserved access stop"));
            let after: Vec<serde_json::Value> =
                sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(after, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_another_scraper_owns_lock_when_enrichment_is_requested_then_it_fails_before_network()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            use track_scrapper_v2::{config::Config, database::Database};
            let database = Database::connect(&test.url).await?;
            let mut connection = database.pool().acquire().await?;
            sqlx::query("SELECT pg_advisory_lock(hashtext(current_schema()),194832741)")
                .execute(&mut *connection)
                .await?;
            let error = track_scrapper_v2::wiaa_enrichment::enrich(&Config::default(), &test.url)
                .await
                .expect_err("scraper owns shared lock");
            assert!(error.to_string().contains("another scraper owns"));
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 0);
            let released: bool = sqlx::query_scalar(
                "SELECT pg_advisory_unlock(hashtext(current_schema()),194832741)",
            )
            .fetch_one(&mut *connection)
            .await?;
            assert!(released);
            drop(connection);
            database.close().await;
            Ok(())
        })
    })
    .await
}
