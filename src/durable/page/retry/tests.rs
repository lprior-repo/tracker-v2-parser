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
async fn configured_retry_cycles_honor_one_two_three_attempts_exact_delays_and_idempotent_replay()
-> Result<()> {
    with_database(|pool|Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        for max in 1..=3 {
            let config=config(max)?;
            let mut now=chrono::Utc::now().timestamp_millis();
            let scope=scope(&config,now)?;
            let request=page::bind_target(&database,target(max),&scope).await?;
            let policy=RetryPolicy::configured(&config,&request)?;
            for total in 1..=max*2+1 {
                let ReservationDecision::Reserved {reservation}=page::reserve_configured(&database,&config,&request,now).await? else {anyhow::bail!("reservation missing")};
                let expected_cycle=i32::try_from((total-1)/max+1)?;
                let expected_ordinal=i32::try_from((total-1)%max+1)?;
                assert_eq!((reservation.cycle,reservation.ordinal,reservation.total_attempts),(expected_cycle,expected_ordinal,i32::try_from(total)?));
                let before=snapshot(&database).await?;
                let ReservationDecision::Reserved {reservation:replayed}=page::reserve_configured(&database,&config,&request,now).await? else {anyhow::bail!("replay missing")};
                assert_eq!(replayed.attempt_id,reservation.attempt_id);
                assert_eq!(snapshot(&database).await?,before);
                let outcome=database.durable_fail_with_policy(&reservation,failure(now),&policy).await?;
                let PageOutcome::Deferred {retry_at_ms,cycle}=outcome else {anyhow::bail!("deferral missing")};
                let delay=if expected_ordinal==i32::try_from(max)? {scope.cooldown_ms} else {i64::from(expected_ordinal)*1000};
                assert_eq!(retry_at_ms,now+delay);
                assert_eq!(cycle,expected_cycle);
                let before=snapshot(&database).await?;
                assert_eq!(database.durable_fail_with_policy(&reservation,failure(now+999_999),&policy).await?,outcome);
                assert!(matches!(page::reserve_configured(&database,&config,&request,retry_at_ms-1).await?,ReservationDecision::Settled {outcome:PageOutcome::Deferred {retry_at_ms:saved,..}} if saved==retry_at_ms));
                assert_eq!(snapshot(&database).await?,before);
                now=retry_at_ms;
            }
            assert_eq!(sqlx::query_scalar::<_,i32>("SELECT attempts FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?,i32::try_from(max*2+1)?);
        }
        Ok(())
    })).await
}

#[tokio::test]
async fn configured_retry_deadline_exhaustion_is_settled_for_every_cycle_limit() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            for max in 1..=3 {
                let config = config(max)?;
                let now = chrono::Utc::now().timestamp_millis();
                let mut scope = scope(&config, now)?;
                scope.deadline_ms = now + 1000;
                let request = page::bind_target(&database, target(max), &scope).await?;
                let policy = RetryPolicy::configured(&config, &request)?;
                let ReservationDecision::Reserved { reservation } =
                    page::reserve_configured(&database, &config, &request, now).await?
                else {
                    anyhow::bail!("reservation missing")
                };
                let outcome = database
                    .durable_fail_with_policy(&reservation, failure(now), &policy)
                    .await?;
                assert_eq!(
                    outcome,
                    PageOutcome::Exhausted {
                        detail: "DNS lookup failed".into()
                    }
                );
                let before = snapshot(&database).await?;
                assert_eq!(
                    database
                        .durable_fail_with_policy(&reservation, failure(now + 5000), &policy)
                        .await?,
                    outcome
                );
                assert!(matches!(
                    page::reserve_configured(&database, &config, &request, now + 5000).await?,
                    ReservationDecision::Settled {
                        outcome: PageOutcome::Exhausted { .. }
                    }
                ));
                assert_eq!(snapshot(&database).await?, before);
            }
            Ok(())
        })
    })
    .await
}
