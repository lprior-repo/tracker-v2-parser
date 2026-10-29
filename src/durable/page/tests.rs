use std::path::Path;

mod retry_clock;
mod sanitization;
mod stop;

use super::{
    self as page, FetchDecision, PageOutcome, PageScope, ParseDecision, ReservationDecision,
};
use crate::{
    database::Database,
    durable::pacing::test_support::{FixtureKind, WireFixture, with_database},
    model::{PageContext, PageKind, Source, Target},
    transport::{Mode, Options, Transport},
};
use anyhow::{Context, Result};

fn target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "https://oh.milesplit.com/athletes/15005189-brice-fuller".into(),
        PageContext {
            state: Some("OH".into()),
            athlete_id: Some("15005189".into()),
            ..Default::default()
        },
    )
}

fn scope() -> Result<PageScope> {
    Ok(PageScope {
        run_generation: "native-wire-test".into(),
        config_sha256: "a".repeat(64),
        deadline_ms: chrono::Utc::now()
            .timestamp_millis()
            .checked_add(3_600_000)
            .context("test deadline")?,
        cooldown_ms: 15_000,
    })
}

fn transport(root: &Path, fixture: &WireFixture, database: &Database) -> Result<Transport> {
    Ok(Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: root.join("unused"),
        headless: true,
        rps: 3.0,
    })?
    .with_persistent_pacing(database.pool().clone())
    .with_http_fixture("https://oh.milesplit.com", fixture.url.clone())?)
}

#[tokio::test]
async fn retained_capture_manifest_recovers_before_any_second_get_after_database_ack_loss()
-> Result<()> {
    with_database(|pool|Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let root=tempfile::tempdir()?;
        let fixture=WireFixture::start(FixtureKind::Page {status:200,body:include_str!("../../../fixtures/milesplit/athlete-oh-15005189.html")}).await?;
        let request=page::bind_target(&database,target(),&scope()?).await?;
        let now=chrono::Utc::now().timestamp_millis();
        let ReservationDecision::Reserved {reservation}=page::reserve(&database,&request,now).await? else {anyhow::bail!("reservation")};
        let mut first=transport(root.path(),&fixture,&database)?;
        let FetchDecision::Captured {capture}=page::fetch(&database,root.path(),&reservation,&mut first,now).await? else {anyhow::bail!("capture")};
        first.close().await?;
        sqlx::query("UPDATE durable_page_attempts SET capture=NULL,state='reserved' WHERE attempt_id=$1").bind(&reservation.attempt_id).execute(pool).await?;
        let mut restarted=transport(root.path(),&fixture,&database)?;
        let FetchDecision::Captured {capture:replayed}=page::fetch(&database,root.path(),&reservation,&mut restarted,now).await? else {anyhow::bail!("retained replay")};
        assert_eq!(capture,replayed);
        restarted.close().await?;
        let ParseDecision::Parsed {parsed}=page::parse(&database,root.path(),&reservation,&capture,now).await? else {anyhow::bail!("parse")};
        let outcome=page::persist(&database,root.path(),&reservation,&capture,&parsed).await?;
        assert!(matches!(outcome,PageOutcome::Completed {..}));
        assert_eq!(page::persist(&database,root.path(),&reservation,&capture,&parsed).await?,outcome);
        assert_eq!(sqlx::query_scalar::<_,i32>("SELECT attempts FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?,1);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_receipts").fetch_one(pool).await?,1);
        assert_eq!(fixture.finish().await?.len(),1);
        Ok(())
    })).await
}

#[tokio::test]
async fn http_access_denial_is_retained_and_never_retried_in_the_same_or_new_operation()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let root = tempfile::tempdir()?;
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 403,
                body: "<h1>Access denied</h1>",
            })
            .await?;
            let mut scope = scope()?;
            let request = page::bind_target(&database, target(), &scope).await?;
            let now = chrono::Utc::now().timestamp_millis();
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &request, now).await?
            else {
                anyhow::bail!("reservation")
            };
            let mut first = transport(root.path(), &fixture, &database)?;
            assert!(matches!(
                page::fetch(&database, root.path(), &reservation, &mut first, now).await?,
                FetchDecision::Settled {
                    outcome: PageOutcome::Blocked { .. }
                }
            ));
            first.close().await?;
            let (capture, _, _) = database.durable_stages(&reservation).await?;
            assert_eq!(capture.context("denial evidence retained")?.status, 403);
            let mut restarted = transport(root.path(), &fixture, &database)?;
            assert!(matches!(
                page::fetch(&database, root.path(), &reservation, &mut restarted, now).await?,
                FetchDecision::Settled {
                    outcome: PageOutcome::Blocked { .. }
                }
            ));
            restarted.close().await?;
            scope.run_generation = "new-denial-operation".into();
            let next = page::bind_target(&database, target(), &scope).await?;
            assert!(page::reserve(&database, &next, now).await.is_err());
            assert_eq!(
                sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages")
                    .fetch_one(pool)
                    .await?,
                1
            );
            assert_eq!(fixture.finish().await?.len(), 1);
            Ok(())
        })
    })
    .await
}
