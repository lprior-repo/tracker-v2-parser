use super::test_support::*;
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, FetchDecision, PageOutcome, ParseDecision, ReservationDecision},
    },
    model::{Capture, PageKind},
    transport::{Failure, Mode, Options, Transport},
};
use anyhow::{Context, Result};

#[tokio::test]
async fn configured_exclusion_preserves_fsynced_real_stop_before_reservation_or_fetch_replay()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config(3)?;
            let now = chrono::Utc::now().timestamp_millis();
            let root = tempfile::tempdir()?;
            for (resume_reservation, retained_response) in [(true, false), (false, false), (true, true)] {
                let mut target = target(u32::from(resume_reservation));
                target.kind = PageKind::Meet;
                target.url = format!("https://wi.milesplit.com/meets/123/photos/{resume_reservation}-{retained_response}");
                let request = page::bind_target(&database, target, &scope(&config, now)?).await?;
                let ReservationDecision::Reserved { reservation } = page::reserve(&database, &request, now).await? else {
                    anyhow::bail!("original reservation missing")
                };
                let failure = Failure {
                    stopped_origin: Some("https://wi.milesplit.com".into()),
                    capture: Some(Capture { url: request.target.url.clone(), status: 403, content_type: "text/html".into(), body: b"<main>Access denied: CAPTCHA</main>".to_vec() }),
                    blocked: true,
                    retryable: false,
                    detail: "HTTP 403 access denied".into(),
                };
                if retained_response {
                    super::super::execution::retain(&database, root.path(), &reservation, failure.capture.context("stop capture")?, now).await?;
                } else {
                    super::super::stopped::save(root.path(), &reservation, &failure, now).await?;
                }
                let before = snapshot(&database).await?;
                if resume_reservation {
                    if !retained_response {
                        let selected = page::select_scheduled(&database, &config, &scope(&config, now)?, now, &[], page::Scheduling::Resume).await?.context("unfinished exclusion selection missing")?;
                        assert_eq!(selected.binding.operation_id, request.binding.operation_id);
                        assert_eq!(snapshot(&database).await?, before);
                    }
                    let ReservationDecision::Reserved { reservation: resumed } = page::reserve_configured(&database, &config, &request, now).await? else {
                        anyhow::bail!("exclusion suppressed unfinished retained-stop reservation")
                    };
                    assert_eq!(resumed.attempt_id, reservation.attempt_id);
                    assert_eq!(snapshot(&database).await?, before);
                }
                let fixture = WireFixture::start(FixtureKind::Page { status: 200, body: "must remain unused" }).await?;
                let mut transport = Transport::new(Options { mode: Mode::Http, driver_url: None, profile: root.path().join("unused"), headless: true, rps: 3.0 })?
                    .with_persistent_pacing(pool.clone())
                    .with_http_fixture("https://wi.milesplit.com", fixture.url.clone())?;
                let FetchDecision::Settled { outcome } = page::fetch_configured(&database, root.path(), &reservation, &mut transport, now, &config).await? else {
                    anyhow::bail!("retained stop did not settle")
                };
                assert!(matches!(outcome, PageOutcome::Blocked { .. }));
                let blocked = snapshot(&database).await?;
                let ReservationDecision::Settled { outcome: replay } = page::reserve_configured(&database, &config, &request, now).await? else {
                    anyhow::bail!("blocked reservation replay missing")
                };
                assert_eq!(replay, outcome);
                assert!(matches!(page::fetch_configured(&database, root.path(), &reservation, &mut transport, now, &config).await?, FetchDecision::Settled { outcome: saved } if saved == outcome));
                assert_eq!(snapshot(&database).await?, blocked);
                assert!(crate::runner::durable::bootstrap(&database, &config).await.is_err());
                assert_eq!(sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?, 1);
                transport.close().await?;
                assert!(fixture.finish().await?.is_empty());
            }
            assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?, 0);
            assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_receipts").fetch_one(pool).await?, 0);
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn configured_exclusion_resumes_valid_retained_capture_and_parse_without_wire() -> Result<()>
{
    with_database(|pool| Box::pin(async move {
        let database = Database::durable_test_database(pool.clone());
        let config = config(3)?;
        let now = chrono::Utc::now().timestamp_millis();
        let root = tempfile::tempdir()?;
        let mut target = target(77);
        target.kind = PageKind::Contacts;
        target.url = "https://school.example/staff".into();
        target.context.school_id = Some("77".into());
        target.context.school_name = Some("Retained school".into());
        let request = page::bind_target(&database, target, &scope(&config, now)?).await?;
        let ReservationDecision::Reserved { reservation } = page::reserve(&database, &request, now).await? else {
            anyhow::bail!("original retained reservation missing")
        };
        let reference = super::super::execution::retain(&database, root.path(), &reservation, Capture { url: request.target.url.clone(), status: 200, content_type: "text/html".into(), body: b"<main></main>".to_vec() }, now).await?;
        let ParseDecision::Parsed { parsed } = page::parse(&database, root.path(), &reservation, &reference, now).await? else {
            anyhow::bail!("retained parse missing")
        };
        let before = snapshot(&database).await?;
        assert!(matches!(page::reserve_configured(&database, &config, &request, now).await?, ReservationDecision::Reserved { reservation: saved } if saved.attempt_id == reservation.attempt_id));
        assert_eq!(snapshot(&database).await?, before);
        let fixture = WireFixture::start(FixtureKind::Page { status: 403, body: "must remain unused" }).await?;
        let mut transport = Transport::new(Options { mode: Mode::Http, driver_url: None, profile: root.path().join("unused"), headless: true, rps: 3.0 })?.with_persistent_pacing(pool.clone());
        assert!(matches!(page::fetch_configured(&database, root.path(), &reservation, &mut transport, now, &config).await?, FetchDecision::Captured { capture } if capture == reference));
        assert!(matches!(page::parse(&database, root.path(), &reservation, &reference, now).await?, ParseDecision::Parsed { parsed: saved } if saved == parsed));
        let outcome = page::persist(&database, root.path(), &reservation, &reference, &parsed).await?;
        assert!(matches!(outcome, PageOutcome::Completed { .. }));
        let completed = snapshot(&database).await?;
        assert!(matches!(page::fetch_configured(&database, root.path(), &reservation, &mut transport, now, &config).await?, FetchDecision::Settled { outcome: saved } if saved == outcome));
        assert!(matches!(page::reserve_configured(&database, &config, &request, now).await?, ReservationDecision::Settled { outcome: saved } if saved == outcome));
        assert_eq!(snapshot(&database).await?, completed);
        assert_eq!(sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?, 1);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?, 0);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_receipts").fetch_one(pool).await?, 1);
        transport.close().await?;
        assert!(fixture.finish().await?.is_empty());
        Ok(())
    })).await
}
