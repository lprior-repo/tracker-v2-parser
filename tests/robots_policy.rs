mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{PageContext, PageKind, Source, Sport, Target},
    sources,
    transport::{Mode, Options, Transport},
};

fn guarded_transport() -> Result<Transport> {
    Transport::new(Options {
        mode: Mode::Browser,
        driver_url: Some("fixture-must-not-launch".into()),
        profile: std::path::PathBuf::new(),
        headless: true,
        rps: 1.0,
    })
}

fn generated_api() -> Result<Target> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Meet,
        "https://oh.milesplit.com/meets/770621-beaver-eastern-invite-2026/results".into(),
        PageContext {
            sport: Some(Sport::CrossCountry),
            ..PageContext::default()
        },
    );
    let page = sources::parse(
        &target,
        include_str!("../fixtures/milesplit/meet-oh-770621-results.html"),
    )?;
    page.next
        .into_iter()
        .find(|next| next.url.contains("/api/v1/meets/"))
        .context("actual parser-generated API target")
}

#[tokio::test]
async fn given_published_meet_when_generated_api_is_acquired_then_policy_stops_before_browser_start()
-> Result<()> {
    let target = generated_api()?;
    let mut transport = guarded_transport()?;
    let failure = transport.fetch(&target).await.expect_err("robots denial");
    transport.close().await?;
    assert!(failure.blocked);
    assert_eq!(
        failure.stopped_origin.as_deref(),
        Some("https://oh.milesplit.com")
    );
    assert!(failure.capture.is_none());
    assert!(failure.detail.contains("published robots restrictions"));
    Ok(())
}

#[tokio::test]
async fn given_robots_denial_when_persisted_and_reenqueued_then_authority_and_stop_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = generated_api()?;
            database.enqueue(&[target.clone()], false).await?;
            let job = database
                .next_job(None, 3, &[])
                .await?
                .context("queued API")?;
            assert!(database.start_attempt(job.id, 3).await?);
            let mut transport = guarded_transport()?;
            let failure = transport.fetch(&target).await.expect_err("policy stop");
            transport.close().await?;
            database.fail_acquisition(&job, &failure).await?;
            database.enqueue(&[target], true).await?;
            let row: (String, i32, String) = sqlx::query_as(
                "SELECT status,attempts,cursor->>'access_stop_url' FROM scrape_pages WHERE id=$1",
            )
            .bind(job.id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(
                row,
                ("blocked".into(), 1, "https://oh.milesplit.com".into())
            );
            assert!(database.next_job(None, 3, &[]).await?.is_none());
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_encoded_or_ambiguous_disallowed_routes_when_acquired_then_no_policy_bypass_exists()
-> Result<()> {
    for path in [
        "/api/v1/test",
        "/%61pi/v1/test",
        "/%2Fapi/v1/test",
        "/%2561pi/v1/test",
        "/API/v1/test",
        "/rankings",
        "/virtual-meets",
        "/contact",
        "/%GG",
        "/safe%2f..%2fapi/v1/test",
        "/%5capi/v1/test",
        "/%00api/v1/test",
    ] {
        let mut target = generated_api()?;
        target.url = format!("https://www.milesplit.com{path}");
        let mut transport = guarded_transport()?;
        let failure = transport
            .fetch(&target)
            .await
            .expect_err("policy denial before launch");
        transport.close().await?;
        assert!(failure.blocked, "{path}: {}", failure.detail);
        assert_eq!(
            failure.stopped_origin.as_deref(),
            Some("https://www.milesplit.com")
        );
        assert!(failure.capture.is_none());
    }
    Ok(())
}

#[tokio::test]
async fn given_disallowed_token_preflight_when_public_profile_is_requested_then_bootstrap_stops()
-> Result<()> {
    let mut target = generated_api()?;
    target.token_url = Some(target.url.clone());
    target.url = "https://oh.milesplit.com/athletes/123".into();
    let mut transport = guarded_transport()?;
    let failure = transport
        .fetch(&target)
        .await
        .expect_err("preflight policy stop");
    transport.close().await?;
    assert!(failure.blocked);
    assert_eq!(
        failure.stopped_origin.as_deref(),
        Some("https://oh.milesplit.com")
    );
    assert!(failure.detail.contains("published robots restrictions"));
    assert!(failure.capture.is_none());
    Ok(())
}
