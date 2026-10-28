mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Target},
};

fn contact(url: &str, school: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        url.into(),
        PageContext {
            school_id: Some(school.into()),
            ..PageContext::default()
        },
    )
}

#[tokio::test]
async fn given_redirected_contact_denial_when_persisted_then_effective_authority_is_ineligible()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let origin = contact("https://a.example.edu/staff", "a");
            database.enqueue(&[origin], false).await?;
            let job = database
                .next_job(None, 3, &[])
                .await?
                .context("origin job")?;
            assert!(database.start_attempt(job.id, 3).await?);
            let capture = Capture {
                url: "https://b.example.edu/staff".into(),
                status: 403,
                content_type: "text/html".into(),
                body: Vec::new(),
            };
            database
                .fail_job(&job, Some(&capture), "fixture access stop", true)
                .await?;
            let denied = contact("https://b.example.edu/contact", "b");
            let unrelated = contact("https://c.example.edu/staff", "c");
            database
                .enqueue(&[denied, unrelated.clone()], false)
                .await?;
            let next = database
                .next_job(None, 3, &[])
                .await?
                .context("unrelated site")?;
            let blocked_id: i64 = sqlx::query_scalar(
                "SELECT id FROM scrape_pages WHERE url='https://b.example.edu/contact'",
            )
            .fetch_one(&test.pool)
            .await?;
            let admitted = database.start_attempt(blocked_id, 3).await?;
            let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM scrape_pages WHERE id=$1")
                .bind(blocked_id)
                .fetch_one(&test.pool)
                .await?;
            let provenance: (String, String) = sqlx::query_as(
                "SELECT url,cursor->>'access_stop_url' FROM scrape_pages WHERE id=$1",
            )
            .bind(job.id)
            .fetch_one(&test.pool)
            .await?;
            database.close().await;
            assert_eq!(
                provenance,
                (
                    "https://a.example.edu/staff".into(),
                    "https://b.example.edu/staff".into()
                )
            );
            assert_eq!(next.target.url, unrelated.url);
            assert!(!admitted);
            assert_eq!(attempts, 0);
            Ok(())
        })
    })
    .await
}
