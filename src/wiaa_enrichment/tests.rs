use anyhow::{Context, Result};

use crate::{
    database::Database,
    durable::pacing::test_support::with_database,
    model::{PageContext, PageKind, Source, Target},
};

#[tokio::test]
async fn given_preserved_wiaa_access_stop_when_durable_preflight_runs_then_no_wire_or_checkpoint_changes()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let target = Target::get(
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
            );
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
                    .fetch_all(pool)
                    .await?;
            let error = super::check_stops(&database)
                .await
                .expect_err("preserved authority stop");
            assert!(error.to_string().contains("preserved access stop"));
            let after: Vec<serde_json::Value> =
                sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id")
                    .fetch_all(pool)
                    .await?;
            assert_eq!(after, before);
            assert_eq!(after.len(), 1);
            assert_eq!(after[0]["status"], "blocked");
            assert_eq!(after[0]["attempts"], 1);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                    .fetch_one(pool)
                    .await?,
                0
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_operations")
                    .fetch_one(pool)
                    .await?,
                0
            );
            Ok(())
        })
    })
    .await
}
