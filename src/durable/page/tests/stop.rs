use super::{page, scope, target, transport};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{FetchDecision, PageOutcome, ReservationDecision},
    },
};
use anyhow::{Context, Result};

#[tokio::test]
async fn known_denial_survives_wire_and_page_database_recording_failures_without_second_get()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database = Database::durable_test_database(pool.clone());
        let root = tempfile::tempdir()?;
        let fixture = WireFixture::start(FixtureKind::Page {
            status: 403,
            body: "<h1>Access denied</h1>",
        }).await?;
        let mut scope = scope()?;
        let request = page::bind_target(&database, target(), &scope).await?;
        let now = chrono::Utc::now().timestamp_millis();
        let ReservationDecision::Reserved { reservation } = page::reserve(&database, &request, now).await? else {
            anyhow::bail!("reservation")
        };
        sqlx::query("CREATE FUNCTION reject_wire_headers() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected wire-header database outage'; END $$").execute(pool).await?;
        sqlx::query("CREATE TRIGGER reject_wire_headers BEFORE UPDATE ON durable_wire_requests FOR EACH ROW EXECUTE FUNCTION reject_wire_headers()").execute(pool).await?;
        sqlx::query("CREATE FUNCTION reject_page_stop() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='blocked' THEN RAISE EXCEPTION 'injected stop database outage'; END IF; RETURN NEW; END $$").execute(pool).await?;
        sqlx::query("CREATE TRIGGER reject_page_stop BEFORE UPDATE ON durable_page_operations FOR EACH ROW EXECUTE FUNCTION reject_page_stop()").execute(pool).await?;
        let mut first = transport(root.path(), &fixture, &database)?;
        let failed = page::fetch(&database, root.path(), &reservation, &mut first, now).await;
        first.close().await?;
        assert!(failed.is_err());
        let stages = database.durable_stages(&reservation).await?;
        assert!(stages.0.is_none());
        assert!(stages.2.is_none());
        let wire: (String, Option<i32>) = sqlx::query_as("SELECT state,status FROM durable_wire_requests").fetch_one(pool).await?;
        assert_eq!(wire, ("intent".into(), None));
        let manifest = root.path().join(format!("{}.stop", reservation.attempt_id));
        let retained: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
        assert_eq!(retained["origin"], "https://oh.milesplit.com");
        assert!(retained.get("body").is_none());
        assert!(retained.get("detail").is_none());
        sqlx::query("DROP TRIGGER reject_wire_headers ON durable_wire_requests").execute(pool).await?;
        sqlx::query("DROP TRIGGER reject_page_stop ON durable_page_operations").execute(pool).await?;
        let mut restarted = transport(root.path(), &fixture, &database)?;
        let replayed = page::fetch(&database, root.path(), &reservation, &mut restarted, now).await?;
        restarted.close().await?;
        assert!(matches!(replayed, FetchDecision::Settled { outcome: PageOutcome::Blocked { .. } }));
        let saved: (String, i32, serde_json::Value) = sqlx::query_as("SELECT status,attempts,cursor FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?;
        assert_eq!(saved.0, "blocked");
        assert_eq!(saved.1, 1);
        assert_eq!(saved.2["access_stop_url"], "https://oh.milesplit.com");
        let outcome = database.durable_stages(&reservation).await?.2.context("saved access stop")?;
        assert!(matches!(outcome, PageOutcome::Blocked { .. }));
        scope.run_generation = "after-retained-stop".into();
        let next = page::bind_target(&database, target(), &scope).await?;
        assert!(page::reserve(&database, &next, now).await.is_err());
        assert_eq!(fixture.finish().await?.len(), 1);
        Ok(())
    })).await
}
