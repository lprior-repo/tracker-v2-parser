use super::*;
use crate::{
    durable::pacing::test_support::{FixtureKind, WireFixture, with_database},
    model::{PageContext, PageKind, Source},
    transport::{Mode, Options},
};

fn configured(pool: sqlx::PgPool, fixture: &WireFixture) -> Result<Transport> {
    let mut transport = Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: Default::default(),
        headless: true,
        rps: 2.0,
    })?
    .with_persistent_pacing(pool);
    transport.http_test_url = Some(fixture.url.clone());
    transport.clients.insert(
        "http://www.milesplit.com".into(),
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?,
    );
    Ok(transport)
}

#[tokio::test]
async fn given_redirects_and_a_recreated_transport_when_wired_then_every_request_remains_two_rps()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let fixture = WireFixture::start(FixtureKind::Redirect).await?;
            let target = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                "http://www.milesplit.com/athletes/fixture".into(),
                PageContext::default(),
            );
            let mut first = configured(pool.clone(), &fixture)?;
            let captured = first.fetch(&target).await;
            let first_closed = first.close().await;
            let mut second = configured(pool.clone(), &fixture)?;
            let restarted = second.fetch(&target).await;
            let second_closed = second.close().await;
            let arrivals = fixture.finish().await?;
            first_closed.and(second_closed)?;
            assert_eq!(captured?.status, 200);
            assert_eq!(restarted?.status, 200);
            assert_eq!(arrivals.len(), 3);
            for pair in arrivals.windows(2) {
                assert!(pair[1].1.duration_since(pair[0].1) >= Duration::from_millis(450));
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_contact_targets_when_durably_paced_then_five_and_thirty_second_caps_are_persisted()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let fixture = WireFixture::start(FixtureKind::Redirect).await?;
        let transport = configured(pool.clone(), &fixture)?;
        for (source, address, seconds) in [
            (Source::MileSplit, "https://school.example.edu/contact", 5_i64),
            (Source::AthleticNet, "https://other.example.edu/staff-directory", 30),
        ] {
            let target = Target::get(source, PageKind::Contacts, address.into(), PageContext::default());
            transport.paced_wire(&target, address, async { Ok(()) }).await?;
            let observed: i64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM (next_allowed_at-updated_at))::bigint FROM durable_wire_pacing WHERE pacing_key=$1")
                .bind(format!("provider:{}", source.as_str())).fetch_one(pool).await?;
            assert_eq!(observed, seconds);
        }
        transport.close().await?;
        assert!(fixture.finish().await?.is_empty());
        Ok(())
    })).await
}

#[tokio::test]
async fn given_recorded_http_headers_without_page_capture_when_replayed_then_only_a_new_attempt_can_send()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let fixture = WireFixture::start(FixtureKind::Redirect).await?;
            let target = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                "http://www.milesplit.com/athletes/fixture".into(),
                PageContext::default(),
            );
            let operation = "1".repeat(64);
            let attempt = "2".repeat(64);
            let mut first = configured(pool.clone(), &fixture)?;
            first.set_wire_context(&operation, &attempt)?;
            let captured = first.fetch(&target).await;
            first.close().await?;
            let mut replay = configured(pool.clone(), &fixture)?;
            replay.set_wire_context(&operation, &attempt)?;
            let repeated = replay
                .fetch(&target)
                .await
                .expect_err("completed innerHTTP cannot repeat sameattempt");
            replay.close().await?;
            assert!(!repeated.blocked);
            assert!(repeated.detail.contains("network interruption"));
            let mut next = configured(pool.clone(), &fixture)?;
            next.set_wire_context(&operation, &"3".repeat(64))?;
            let next_capture = next.fetch(&target).await;
            next.close().await?;
            assert_eq!(captured?.status, 200);
            assert_eq!(next_capture?.status, 200);
            assert_eq!(fixture.finish().await?.len(), 3);
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM durable_wire_requests")
                .fetch_one(pool)
                .await?;
            assert_eq!(count, 3);
            Ok(())
        })
    })
    .await
}
