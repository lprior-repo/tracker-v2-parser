use super::*;
use crate::{durable::page::PageReservation, model::Capture};

const BODY: &str = include_str!("../../../../fixtures/milesplit/athlete-oh-15005189.html");

#[tokio::test]
async fn over_budget_reserved_attempt_never_starts_another_http_request() -> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let root=tempfile::tempdir()?;
        let fixture=WireFixture::start(FixtureKind::Page {status:200,body:BODY}).await?;
        let now=chrono::Utc::now().timestamp_millis();
        let reservation=old_reservation(&database,now).await?;
        let mut connection=transport(root.path(),&fixture,&database)?;
        let result=page::fetch(&database,root.path(),&reservation,&mut connection,now).await;
        connection.close().await?;
        let wires=fixture.finish().await?;
        assert_eq!(wires.len(),0);
        assert!(matches!(result?,FetchDecision::Settled {outcome:PageOutcome::Exhausted {detail}} if detail.contains("45/3") && detail.contains("original DNS failure")));
        assert_eq!(database.durable_failure_detail(&reservation.request).await?.as_deref(),Some("original DNS failure"));
        assert_eq!(database.durable_acquisition_attempts(&reservation.request).await?,45);
        Ok(())
    })).await
}

#[tokio::test]
async fn over_budget_retained_capture_recovers_after_ack_loss_without_new_wire() -> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let root=tempfile::tempdir()?;
        let fixture=WireFixture::start(FixtureKind::Page {status:200,body:BODY}).await?;
        let now=chrono::Utc::now().timestamp_millis();
        let reservation=old_reservation(&database,now).await?;
        let capture=page::execution::retain(&database,root.path(),&reservation,Capture {url:reservation.request.target.url.clone(),status:200,content_type:"text/html".into(),body:BODY.as_bytes().to_vec()},now).await?;
        sqlx::query("UPDATE durable_page_attempts SET capture=NULL,state='reserved' WHERE attempt_id=$1").bind(&reservation.attempt_id).execute(pool).await?;
        let mut connection=transport(root.path(),&fixture,&database)?;
        let fetched=page::fetch(&database,root.path(),&reservation,&mut connection,now).await;
        connection.close().await?;
        assert_eq!(fixture.finish().await?.len(),0);
        assert!(matches!(fetched?,FetchDecision::Captured {capture:ref actual} if actual==&capture));
        let ParseDecision::Parsed {parsed}=page::parse(&database,root.path(),&reservation,&capture,now).await? else {anyhow::bail!("parse retained capture")};
        let completed=page::persist(&database,root.path(),&reservation,&capture,&parsed).await?;
        assert!(matches!(completed,PageOutcome::Completed {..}));
        assert_eq!(page::persist(&database,root.path(),&reservation,&capture,&parsed).await?,completed);
        assert_eq!(database.durable_acquisition_attempts(&reservation.request).await?,45);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_receipts").fetch_one(pool).await?,1);
        Ok(())
    })).await
}

async fn old_reservation(database: &Database, now: i64) -> Result<PageReservation> {
    let request = page::bind_target(database, target(), &scope()?).await?;
    let ReservationDecision::Reserved { reservation } =
        page::reserve(database, &request, now).await?
    else {
        anyhow::bail!("initial reservation")
    };
    let old = PageReservation {
        request: request.clone(),
        attempt_id: page::attempt_identity(&request, 15, 3)?,
        cycle: 15,
        ordinal: 3,
        total_attempts: 45,
    };
    sqlx::query("UPDATE durable_page_attempts SET attempt_id=$2,cycle=15,ordinal=3,total_attempts=45 WHERE attempt_id=$1").bind(&reservation.attempt_id).bind(&old.attempt_id).execute(database.pool()).await?;
    sqlx::query("INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts,state,outcome) SELECT repeat(md5($1||':'||n::text),2),$1,((n-1)/3)+1,((n-1)%3)+1,n,'failed',jsonb_build_object('state','deferred','retry_at_ms',$2::bigint,'cycle',((n-1)/3)+1) FROM generate_series(1,44) n").bind(&request.binding.operation_id).bind(now).execute(database.pool()).await?;
    sqlx::query(
        "UPDATE durable_page_operations SET cycle=15,cycle_attempts=3 WHERE operation_id=$1",
    )
    .bind(&request.binding.operation_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE scrape_pages SET attempts=45,last_error='original DNS failure' WHERE id=$1",
    )
    .bind(request.binding.page_id)
    .execute(database.pool())
    .await?;
    Ok(old)
}
