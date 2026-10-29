use std::time::Duration;

use anyhow::{Context, Result};
use futures::future::join_all;

use super::{PersistentPacing, request};
use crate::{
    durable::pacing::test_support::{FixtureKind, WireFixture, with_database},
    model::Source,
};

#[tokio::test]
async fn opt_in_start_spacing_preserves_restart_mode_and_reaches_two_rps_with_slow_headers()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let client=reqwest::Client::builder().no_proxy().build()?;
        let mut spans=Vec::new();
        for mode in [false,true] {
            let fixture=WireFixture::start(FixtureKind::DelayedPage {status:200,body:"ok",delay:Duration::from_millis(300)}).await?;
            let results=join_all((0..4).map(|index| {
                let client=&client;
                let url=fixture.url.clone();
                async move {
                    let gate=PersistentPacing::new(pool.clone());
                    let gate=if mode {gate.with_start_spacing()} else {gate};
                    let operation=format!("{:064x}",index+if mode {100} else {1});
                    let gate=gate.with_wire_context(&operation,&"b".repeat(64))?.restart_wire_sequence()?;
                    gate.send(request()?,Duration::from_millis(500),async {Ok(client.get(url).send().await?)}).await
                }
            })).await;
            for response in results {assert_eq!(response?.status().as_u16(),200);}
            let arrivals=fixture.finish().await?;
            assert_eq!(arrivals.len(),4);
            for pair in arrivals.windows(2) {assert!(pair[1].1.duration_since(pair[0].1)>=Duration::from_millis(490));}
            let span=arrivals.last().context("last arrival")?.1.duration_since(arrivals.first().context("first arrival")?.1);
            if mode {assert!(span<Duration::from_millis(2100),"start spacing was still clamped to completed-header cooldown: {span:?}");}
            else {assert!(span>=Duration::from_millis(2400),"legacy completion cooldown changed: {span:?}");}
            spans.push(span);
        }
        eprintln!("durable cadence proof: four HTTP requests,500msinterval,300msheaderdelay; default_span={:?},start_span={:?}",spans[0],spans[1]);
        Ok(())
    })).await
}

#[tokio::test]
async fn active_lease_polling_releases_three_rps_slots_after_slow_headers() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let client = reqwest::Client::builder().no_proxy().build()?;
            let fixture = WireFixture::start(FixtureKind::DelayedPage {
                status: 200,
                body: "ok",
                delay: Duration::from_millis(300),
            })
            .await?;
            let responses = join_all((0..4).map(|index| {
                let client = &client;
                let url = fixture.url.clone();
                async move {
                    PersistentPacing::new(pool.clone())
                        .with_start_spacing()
                        .with_wire_context(&format!("{index:064x}"), &"b".repeat(64))?
                        .restart_wire_sequence()?
                        .send(request()?, Duration::from_micros(333_333), async {
                            Ok(client.get(url).send().await?)
                        })
                        .await
                }
            }))
            .await;
            let arrivals = fixture.finish().await?;
            for response in responses {
                assert_eq!(response?.status().as_u16(), 200);
            }
            assert_eq!(arrivals.len(), 4);
            for pair in arrivals.windows(2) {
                assert!(pair[1].1.duration_since(pair[0].1) >= Duration::from_millis(323));
            }
            let span = arrivals
                .last()
                .context("last arrival")?
                .1
                .duration_since(arrivals.first().context("first arrival")?.1);
            assert!(
                span <= Duration::from_millis(1200),
                "lease polling missed released three-RPS slots: {span:?}"
            );
            eprintln!("three-RPS lease polling: requests=4, interval_us=333333, header_delay_ms=300, span={span:?}, actual_rps={:.4}", 3.0 / span.as_secs_f64());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn start_spacing_cancellation_keeps_full_committed_reservation_debt() -> Result<()> {
    with_database(|pool| Box::pin(async move {
        let gate=PersistentPacing::new(pool.clone()).with_start_spacing();
        let (entered,received)=tokio::sync::oneshot::channel();
        {
            let waiting=gate.execute(Source::MileSplit,"https://wi.milesplit.com/cancel",Duration::from_millis(500),async {
                entered.send(()).expect("admitted effect");
                futures::future::pending::<Result<()>>().await
            });
            tokio::pin!(waiting);
            tokio::time::timeout(Duration::from_secs(5),async {
                tokio::select! {
                    seen=received=>seen.context("admission observer"),
                    result=&mut waiting=>anyhow::bail!("effect ended before cancellation: {result:?}"),
                }
            }).await??;
        }
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_pacing WHERE lease_token IS NOT NULL AND next_allowed_at>clock_timestamp()+INTERVAL '40 seconds'").fetch_one(pool).await?,2);
        let recreated=gate.restart_wire_sequence()?;
        let mut called=false;
        assert!(tokio::time::timeout(Duration::from_millis(75),recreated.execute(Source::MileSplit,"https://wi.milesplit.com/next",Duration::from_millis(500),async {called=true;Ok(())})).await.is_err());
        assert!(!called);
        Ok(())
    })).await
}
