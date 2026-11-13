use anyhow::Result;
use serde_json::json;

use super::*;
use crate::durable::pacing::test_support::with_database;

async fn seed(pool: &PgPool) -> Result<i64> {
    let url = "https://co.milesplit.com/teams/15064-school/admin";
    let detail = "browser interception failed: source access stopped at https://co.milesplit.com (HTTP None); human review required";
    let id: i64=sqlx::query_scalar("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',repeat('a',64),$1,$2,'blocked',1,$3) RETURNING id")
        .bind(url).bind(json!({"url":url,"source":"milesplit","kind":"contacts","context":{},"access_stop_url":"https://co.milesplit.com"})).bind(detail).fetch_one(pool).await?;
    let outcome = json!({"state":"blocked","detail":detail});
    sqlx::query("INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,cycle,cycle_attempts,original_attempts,outcome) VALUES(repeat('b',64),$1,'same-owner',$2,'blocked',1,1,0,$3)")
        .bind(id).bind(json!({})).bind(&outcome).execute(pool).await?;
    sqlx::query("INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts,state,outcome) VALUES(repeat('c',64),repeat('b',64),1,1,1,'failed',$1)")
        .bind(outcome).execute(pool).await?;
    sqlx::query("INSERT INTO durable_wire_requests(wire_id,operation_id,attempt_id,request_sha256,occurrence,source,origin,method,state,status,finished_at) VALUES(repeat('d',64),repeat('b',64),repeat('c',64),repeat('e',64),1,'milesplit','https://co.milesplit.com','GET','headers',302,clock_timestamp())")
        .execute(pool).await?;
    Ok(id)
}

#[tokio::test]
async fn given_proven_admin_redirect_when_public_stats_are_admitted_then_stops_and_attempts_remain_unchanged()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            seed(pool).await?;
            let before: serde_json::Value =
                sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(p)) FROM scrape_pages p")
                    .fetch_one(pool)
                    .await?;
            assert!(
                allows_public(pool, "https://co.milesplit.com/athletes/123-runner/stats").await?
            );
            assert!(
                !allows_public(pool, "https://co.milesplit.com/teams/15064-school/admin").await?
            );
            use crate::durable::pacing::{
                PersistentPacing, WireRequest,
                test_support::{FixtureKind, WireFixture},
            };
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 200,
                body: "public fixture",
            })
            .await?;
            let client = reqwest::Client::builder().no_proxy().build()?;
            let gate = PersistentPacing::new(pool.clone())
                .with_overlapped_starts()
                .with_wire_context(&"4".repeat(64), &"5".repeat(64))?;
            let request = WireRequest::new(
                crate::model::Source::MileSplit,
                "https://co.milesplit.com/athletes/123-runner/stats",
                "GET",
                b"",
            )?;
            assert_eq!(
                gate.send(request, std::time::Duration::from_micros(333_334), async {
                    Ok(client.get(fixture.url.clone()).send().await?)
                })
                .await?
                .status()
                .as_u16(),
                200
            );
            assert_eq!(fixture.finish().await?.len(), 1);
            let after: serde_json::Value =
                sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(p)) FROM scrape_pages p")
                    .fetch_one(pool)
                    .await?;
            assert_eq!(before, after);
            sqlx::query("UPDATE durable_wire_requests SET status=403")
                .execute(pool)
                .await?;
            assert!(
                !allows_public(pool, "https://co.milesplit.com/athletes/123-runner/stats").await?
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_mixed_or_ambiguous_contact_stop_when_public_route_is_checked_then_origin_remains_stopped()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        seed(pool).await?;
        sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',repeat('f',64),'https://co.milesplit.com/contact',jsonb_build_object('kind','contacts','access_stop_url','https://co.milesplit.com'),'blocked',1,'public response contained a challenge')").execute(pool).await?;
        assert!(!allows_public(pool,"https://co.milesplit.com/athletes/123-runner/stats").await?);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_exact_derived_local_stop_when_checked_then_it_stays_blocked_but_does_not_stop_unrelated_public_work()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        seed(pool).await?;
        let url="https://co.milesplit.com/athletes/123-runner/stats";
        let detail="source access stopped at https://co.milesplit.com (HTTP None); human review required";
        let id:i64=sqlx::query_scalar("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',repeat('1',64),$1,jsonb_build_object('kind','athlete','source','milesplit','url',$1,'context','{}'::jsonb,'access_stop_url','https://co.milesplit.com'),'blocked',1,$2) RETURNING id")
            .bind(url).bind(detail).fetch_one(pool).await?;
        let outcome=json!({"state":"blocked","detail":detail});
        sqlx::query("INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,cycle,cycle_attempts,original_attempts,outcome) VALUES(repeat('2',64),$1,'derived-owner','{}','blocked',1,1,0,$2)").bind(id).bind(&outcome).execute(pool).await?;
        sqlx::query("INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts,state,outcome) VALUES(repeat('3',64),repeat('2',64),1,1,1,'failed',$1)").bind(outcome).execute(pool).await?;
        let config=crate::config::Config::parse("states=['CO']\n[acquisition]\nmode='public_results'\ncollect_contacts=true\n[sources.athleticnet]\nenabled=false")?;
        crate::runner::durable::check_stops(&crate::database::Database::durable_test_database(pool.clone()),&config).await?;
        assert!(permits_derived(pool,id,url).await?);
        assert_eq!(sqlx::query_scalar::<_,String>("SELECT status FROM scrape_pages WHERE id=$1").bind(id).fetch_one(pool).await?,"blocked");
        sqlx::query("UPDATE scrape_pages SET last_error='source returned HTTP403' WHERE id=$1").bind(id).execute(pool).await?;
        assert!(!permits_derived(pool,id,url).await?);
        Ok(())
    })).await
}
