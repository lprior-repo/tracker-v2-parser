use super::{
    lineage_tests::{bound, configured, history},
    test_support::*,
    wire_tests::connection,
};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, FetchDecision, PageOutcome, ReservationDecision},
    },
};
use anyhow::{Context, Result};

#[tokio::test]
async fn given_two_inherited_failures_when_last_real_http_attempt_fails_then_replay_sends_no_fourth_request()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database = Database::durable_test_database(pool.clone());
        let config = configured()?;
        let now = chrono::Utc::now().timestamp_millis();
        history(&database, &config, now, 2).await?;
        let scope = bound(&config, now, "new-iowa-results", &["old-mixed"])?;
        let request = page::select(&database, &config, &scope, now + 60_000).await?.context("one inherited retry remains")?;
        let ReservationDecision::Reserved { reservation } = page::reserve_configured(&database, &config, &request, now + 60_000).await? else { anyhow::bail!("last reservation") };
        let root = tempfile::tempdir()?;
        let fixture = WireFixture::start(FixtureKind::Page { status:503, body:"temporary failure" }).await?;
        let mut transport = connection(root.path(), &fixture, &database)?.with_http_fixture("https://ia.milesplit.com", fixture.url.clone())?;
        let decision = super::fetch_configured(&database, root.path(), &reservation, &mut transport, now + 60_000, &config).await?;
        assert!(matches!(decision, FetchDecision::Settled { outcome: PageOutcome::Exhausted { detail } } if detail.contains("3/3")));
        let before = snapshot(&database).await?;
        assert!(matches!(page::reserve_configured(&database, &config, &request, now + 600_000).await?, ReservationDecision::Settled { outcome: PageOutcome::Exhausted { .. } }));
        assert!(matches!(super::fetch_configured(&database, root.path(), &reservation, &mut transport, now + 600_000, &config).await?, FetchDecision::Settled { outcome: PageOutcome::Exhausted { .. } }));
        assert_eq!(snapshot(&database).await?, before);
        transport.close().await?;
        assert_eq!(fixture.finish().await?.len(),1);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_attempts").fetch_one(pool).await?,3);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?,1);
        Ok(())
    })).await
}
