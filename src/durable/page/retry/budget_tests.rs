use super::{RetryPolicy, test_support::*};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::with_database,
        page::{self, PageOutcome, ReservationDecision},
    },
};
use anyhow::Result;

#[tokio::test]
async fn old_forty_five_attempt_deferred_page_settles_before_cooldown_without_a_new_attempt()
-> Result<()> {
    settle_old_attempts("deferred").await
}

#[tokio::test]
async fn old_forty_five_attempt_active_page_without_reservation_settles_without_a_new_attempt()
-> Result<()> {
    settle_old_attempts("active").await
}

async fn settle_old_attempts(state: &'static str) -> Result<()> {
    with_database(move |pool| Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let config=config(3)?;
        let now=chrono::Utc::now().timestamp_millis();
        let request=page::bind_target(&database,target(3),&scope(&config,now)?).await?;
        let policy=RetryPolicy::configured(&config,&request)?;
        let ReservationDecision::Reserved {reservation}=page::reserve_configured(&database,&config,&request,now).await? else {anyhow::bail!("reservation")};
        database.durable_fail_with_policy(&reservation,failure(now),&policy).await?;
        sqlx::query("INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts,state,outcome) SELECT repeat(md5($1||':'||n::text),2),$1,((n-1)/3)+1,((n-1)%3)+1,n,'failed',jsonb_build_object('state','deferred','retry_at_ms',$2::bigint,'cycle',((n-1)/3)+1) FROM generate_series(2,45) n")
            .bind(&request.binding.operation_id).bind(now+60_000).execute(pool).await?;
        sqlx::query("UPDATE scrape_pages SET attempts=45 WHERE id=$1").bind(request.binding.page_id).execute(pool).await?;
        sqlx::query("UPDATE durable_page_operations SET cycle=15,cycle_attempts=3,retry_at_ms=$2,outcome=jsonb_build_object('state','deferred','retry_at_ms',$2::bigint,'cycle',15) WHERE operation_id=$1")
            .bind(&request.binding.operation_id).bind(now+60_000).execute(pool).await?;
        if state == "active" {
            sqlx::query("UPDATE durable_page_operations SET state='active',outcome=NULL,retry_at_ms=NULL WHERE operation_id=$1")
                .bind(&request.binding.operation_id).execute(pool).await?;
        }
        let old_attempts:serde_json::Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a").fetch_one(pool).await?;
        assert!(matches!(page::reserve_configured(&database,&config,&request,now).await?,ReservationDecision::Settled {outcome:PageOutcome::Exhausted {detail}} if detail.contains("45") && detail.contains("3") && detail.contains("DNS lookup failed")));
        let before=snapshot(&database).await?;
        assert!(matches!(page::reserve_configured(&database,&config,&request,now+120_000).await?,ReservationDecision::Settled {outcome:PageOutcome::Exhausted {..}}));
        assert_eq!(snapshot(&database).await?,before);
        assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a").fetch_one(pool).await?,old_attempts);
        assert_eq!(sqlx::query_scalar::<_,i32>("SELECT attempts FROM scrape_pages").fetch_one(pool).await?,45);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?,0);
        Ok(())
    })).await
}

#[tokio::test]
async fn phase_change_shares_the_three_attempt_budget_without_resetting_history() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config(3)?;
            let mut now = chrono::Utc::now().timestamp_millis();
            let mut scope = scope(&config, now)?;
            scope.run_generation = "same-campaign-schools".into();
            let first = page::bind_target(&database, target(3), &scope).await?;
            for _ in 0..2 {
                let ReservationDecision::Reserved { reservation } =
                    page::reserve_configured(&database, &config, &first, now).await?
                else {
                    anyhow::bail!("school reservation")
                };
                let PageOutcome::Deferred { retry_at_ms, .. } = database
                    .durable_fail_with_policy(
                        &reservation,
                        failure(now),
                        &RetryPolicy::configured(&config, &first)?,
                    )
                    .await?
                else {
                    anyhow::bail!("school retry")
                };
                now = retry_at_ms;
            }
            scope.run_generation = "same-campaign-results".into();
            let next = page::bind_target(&database, target(3), &scope).await?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &config, &next, now).await?
            else {
                anyhow::bail!("third reservation")
            };
            assert_eq!(reservation.total_attempts, 3);
            let outcome = database
                .durable_fail_with_policy(
                    &reservation,
                    failure(now),
                    &RetryPolicy::configured(&config, &next)?,
                )
                .await?;
            assert!(matches!(outcome,PageOutcome::Exhausted {detail} if detail.contains("3/3")));
            for request in [&first, &next] {
                assert!(matches!(
                    page::reserve_configured(&database, &config, request, now + 60_000).await?,
                    ReservationDecision::Settled {
                        outcome: PageOutcome::Exhausted { .. }
                    }
                ));
            }
            assert_eq!(
                sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages")
                    .fetch_one(pool)
                    .await?,
                3
            );
            scope.run_generation = "same-campaign-v2-results".into();
            let fresh = page::bind_target(&database, target(3), &scope).await?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &config, &fresh, now).await?
            else {
                anyhow::bail!("explicit fresh generation")
            };
            assert_eq!(reservation.total_attempts, 4);
            Ok(())
        })
    })
    .await
}
