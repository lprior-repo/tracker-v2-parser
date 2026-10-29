use std::time::Duration;

use anyhow::{Context, Result};

use super::{PersistentPacing, request};
use crate::durable::pacing::test_support::{FixtureKind, WireFixture, with_database};

#[tokio::test]
async fn waiter_rechecks_403_recorded_after_preflight_before_any_second_wire() -> Result<()> {
    blocked_waiter(403).await
}

#[tokio::test]
async fn waiter_rechecks_body_classified_contact_stop_before_any_second_wire() -> Result<()> {
    blocked_waiter(200).await
}

async fn blocked_waiter(status: u16) -> Result<()> {
    with_database(|pool| Box::pin(async move {
        let fixture=WireFixture::start(FixtureKind::DelayedPage {status,body:"fixture",delay:Duration::from_millis(350)}).await?;
        let client=reqwest::Client::builder().no_proxy().build()?;
        let first=PersistentPacing::new(pool.clone()).with_start_spacing().with_wire_context(&"1".repeat(64),&"2".repeat(64))?;
        let second=PersistentPacing::new(pool.clone()).with_start_spacing().with_wire_context(&"3".repeat(64),&"4".repeat(64))?;
        let first_url=fixture.url.clone();
        let second_url=fixture.url.clone();
        let (entered,received)=tokio::sync::oneshot::channel();
        let first=first.send(request()?,Duration::from_millis(500),async {
            entered.send(()).expect("first gate admission");
            let response=client.get(first_url).send().await?;
            if status==200 {
                sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',repeat('c',64),'https://school.example.org/',jsonb_build_object('kind','contacts','access_stop_url','https://wi.milesplit.com'),'blocked',1,'public response contained a challenge')").execute(pool).await?;
            }
            Ok(response)
        });
        tokio::pin!(first);
        tokio::time::timeout(Duration::from_secs(5),async {
            tokio::select! {
                seen=received=>seen.context("first admission observer"),
                result=&mut first=>anyhow::bail!("first call ended before observation: {result:?}"),
            }
        }).await??;
        let second=second.send(request()?,Duration::from_millis(500),async {
            Ok(client.get(second_url).send().await?)
        });
        tokio::pin!(second);
        let waiting: Result<Result<()>, _>=tokio::time::timeout(Duration::from_millis(100),async {
            tokio::select! {
                result=&mut first=>anyhow::bail!("delayed first response ended early: {result:?}"),
                result=&mut second=>anyhow::bail!("second request must await the first admission: {result:?}"),
            }
        }).await;
        assert!(waiting.is_err());
        let (first_result,second_result)=tokio::join!(&mut first,&mut second);
        assert_eq!(first_result?.status().as_u16(),status);
        let denied=second_result.expect_err("saved stop must reject the admitted waiter");
        assert!(denied.is::<crate::transport::AccessStop>());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?,1);
        assert_eq!(fixture.finish().await?.len(),1);
        Ok(())
    })).await
}

#[tokio::test]
async fn saved_noncontact_media_checkpoint_does_not_become_an_originwide_contact_stop() -> Result<()>
{
    with_database(|pool| Box::pin(async move {
        sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',repeat('c',64),'https://wi.milesplit.com/athletes/1/photos',jsonb_build_object('kind','athlete'),'blocked',1,'reviewed media excluded')").execute(pool).await?;
        let fixture=WireFixture::start(FixtureKind::Page {status:200,body:"public profile"}).await?;
        let client=reqwest::Client::builder().no_proxy().build()?;
        let gate=PersistentPacing::new(pool.clone()).with_start_spacing().with_wire_context(&"1".repeat(64),&"2".repeat(64))?;
        let response=gate.send(request()?,Duration::from_millis(500),async {Ok(client.get(fixture.url.clone()).send().await?)}).await?;
        assert_eq!(response.status().as_u16(),200);
        assert_eq!(fixture.finish().await?.len(),1);
        Ok(())
    })).await
}
