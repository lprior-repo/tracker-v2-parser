#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, School, Source, Target},
};

fn contact(source: Source, path: &str, school: &str) -> Target {
    Target::get(
        source,
        PageKind::Contacts,
        format!("https://school.example.edu/{path}"),
        PageContext {
            state: Some("WI".into()),
            school_id: Some(school.into()),
            school_name: Some("Example High School".into()),
            season: Some(2026),
            ..PageContext::default()
        },
    )
}

#[tokio::test]
async fn given_same_site_access_stop_when_completed_or_exhausted_contact_is_replayed_then_no_claim_or_attempt_changes()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            records::persist(
                &database,
                &records::target(Source::MileSplit, "school-observation"),
                &ParsedPage {
                    schools: vec![School {
                        id: "school-1".into(),
                        name: "Example High School".into(),
                        state: Some("WI".into()),
                        ..School::default()
                    }],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let complete = contact(Source::MileSplit, "staff-complete", "school-1");
            let exhausted = contact(Source::MileSplit, "staff-exhausted", "school-1");
            let stop = contact(Source::AthleticNet, "challenge", "other-source-school");
            database.enqueue(&[complete.clone(), exhausted.clone(), stop], false).await?;
            database.persist(None, &complete, &Capture {
                url: complete.url.clone(), status: 200, content_type: "text/html".into(), body: b"<main>Previously parsed public page</main>".to_vec(),
            }, &ParsedPage::default()).await?;
            sqlx::query("UPDATE scrape_pages SET attempts=1 WHERE url=$1")
                .bind(&complete.url).execute(&test.pool).await?;
            sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error='prior failure' WHERE url=$1")
                .bind(&exhausted.url).execute(&test.pool).await?;
            let blocked = database.next_job(Some(Source::AthleticNet), 3, &[]).await?.context("blocked authority checkpoint")?;
            assert!(database.start_attempt(blocked.id, 3).await?);
            database.fail_job(&blocked, None, "official-site access challenge", true).await?;
            let before: Vec<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id")
                .fetch_all(&test.pool).await?;
            let input = support::fixture("contacts/named-no-email.html");
            for page in [&complete, &exhausted] {
                let output = test.cli(&[
                    "scrape", "--source", "milesplit", "--kind", "contacts", "--url", &page.url,
                    "--input", input.to_str().context("UTF8 fixture path")?, "--school-id", "school-1",
                    "--school-name", "Example High School", "--state", "WI", "--season", "2026",
                ]).await?;
                assert!(!output.status.success());
            }
            let after: Vec<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id")
                .fetch_all(&test.pool).await?;
            assert_eq!(after, before);
            let claims: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts")
                .fetch_one(&test.pool).await?;
            assert_eq!(claims, 0);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_completed_contact_when_corrected_capture_is_replayed_then_new_public_mailbox_is_parsed_without_another_attempt()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            records::persist(
                &database,
                &records::target(Source::MileSplit, "school-observation"),
                &ParsedPage {
                    schools: vec![School {
                        id: "school-1".into(), name: "Example High School".into(), state: Some("WI".into()),
                        ..School::default()
                    }],
                    ..ParsedPage::default()
                },
            ).await?;
            let page = contact(Source::MileSplit, "staff", "school-1");
            let input = test.directory().join("public-staff.html");
            std::fs::write(&input, include_str!("../fixtures/contacts/named-no-email.html"))?;
            let args = [
                "scrape", "--source", "milesplit", "--kind", "contacts", "--url", &page.url,
                "--input", input.to_str().context("UTF8 fixture path")?, "--school-id", "school-1",
                "--school-name", "Example High School", "--state", "WI", "--season", "2026",
            ];
            support::assert_success(&test.cli(&args).await?);
            std::fs::write(&input, "<main><h1>Example High School</h1><article class='staff-card'><span class='staff-name'>Ada Unpublished</span><span class='staff-title'>Head Outdoor Track Coach</span><a href='mailto:ada.runner@school.example.edu'>Published coaching email</a></article></main>")?;
            support::assert_success(&test.cli(&args).await?);
            let claims: Vec<(String, Option<String>)> = sqlx::query_as(
                "SELECT name,email FROM contacts WHERE kind='coach' ORDER BY email NULLS FIRST",
            ).fetch_all(&test.pool).await?;
            assert_eq!(claims, vec![("Ada Unpublished".into(), None), ("Ada Unpublished".into(), Some("ada.runner@school.example.edu".into()))]);
            let state: (String, i32) = sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE url=$1")
                .bind(&page.url).fetch_one(&test.pool).await?;
            assert_eq!(state, ("succeeded".into(), 1));
            database.close().await;
            Ok(())
        })
    })
    .await
}
