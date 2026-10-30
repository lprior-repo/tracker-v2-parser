mod guards;
mod support;

use super::IndexRefreshPermit;
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, FetchDecision, PageOutcome, ParseDecision, ReservationDecision},
    },
};
use anyhow::Result;
use support::*;

#[test]
fn wiaa_index_refresh_never_accepts_payload_token_preflight_or_browser_origin() {
    let mut payload = target(false);
    payload.body = Some(serde_json::json!({}));
    assert!(!super::eligible(&payload));
    let mut token = target(false);
    token.token_url = Some(token.url.clone());
    assert!(!super::eligible(&token));
    let mut browser = target(false);
    browser.browser_origin = Some("https://schools.wiaawi.org".into());
    assert!(!super::eligible(&browser));
}

#[tokio::test]
async fn succeeded_wiaa_indexes_refresh_in_a_new_future_cohort_operation_and_replay_without_new_wire()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let root=tempfile::tempdir()?;
        let old=config("2027")?;
        let future=config("2028,2029")?;
        let now=chrono::Utc::now().timestamp_millis();
        let old_scope=scope(root.path(),&old,"prior-wiaa",now)?;
        let future_scope=scope(root.path(),&future,"future-wiaa",now)?;
        for letter in [false,true] {
            let target=target(letter);
            assert!(matches!(page::offline_replay(&database,root.path(),&old_scope,target.clone(),capture(&target,body(letter))).await?,PageOutcome::Completed {..}));
            let previous=page::bind_target(&database,target.clone(),&old_scope).await?;
            let old_record=operation(&database,&previous.binding.operation_id).await?;
            let request=page::bind_target(&database,target,&future_scope).await?;
            assert!(IndexRefreshPermit::bind(&future,&request)?.is_some());
            let before=snapshot(&database).await?;
            assert!(page::reserve(&database,&request,now).await.is_err());
            assert_eq!(snapshot(&database).await?,before);
            let ReservationDecision::Reserved {reservation}=page::reserve_configured(&database,&future,&request,now).await? else {anyhow::bail!("fresh index reservation missing")};
            assert_eq!((reservation.total_attempts,reservation.cycle,reservation.ordinal),(2,1,1));
            let before_old_replay=snapshot(&database).await?;
            assert!(page::reserve(&database,&previous,now).await.is_err());
            assert!(matches!(page::reserve_configured(&database,&old,&previous,now).await?,ReservationDecision::Settled {outcome:PageOutcome::Completed {..}}));
            assert_eq!(snapshot(&database).await?,before_old_replay);
            let fixture=WireFixture::start(FixtureKind::Page {status:200,body:body(letter)}).await?;
            let mut transport=connection(root.path(),&fixture,&database,&request.target.url)?;
            let FetchDecision::Captured {capture}=page::fetch_configured(&database,root.path(),&reservation,&mut transport,now,&future).await? else {anyhow::bail!("fresh index capture missing")};
            let parsed=match page::parse(&database,root.path(),&reservation,&capture,now).await? {ParseDecision::Parsed {parsed}=>parsed, other=>anyhow::bail!("fresh index parse missing: {other:?}")};
            let outcome=page::persist(&database,root.path(),&reservation,&capture,&parsed).await?;
            assert!(matches!(outcome,PageOutcome::Completed {..}));
            let before=snapshot(&database).await?;
            assert!(matches!(page::reserve_configured(&database,&future,&request,now).await?,ReservationDecision::Settled {outcome:PageOutcome::Completed {..}}));
            assert!(matches!(page::fetch_configured(&database,root.path(),&reservation,&mut transport,now,&future).await?,FetchDecision::Settled {outcome:PageOutcome::Completed {..}}));
            assert_eq!(page::persist(&database,root.path(),&reservation,&capture,&parsed).await?,outcome);
            assert_eq!(snapshot(&database).await?,before);
            assert_eq!(operation(&database,&previous.binding.operation_id).await?,old_record);
            assert_eq!(sqlx::query_scalar::<_,i32>("SELECT original_attempts FROM durable_page_operations WHERE operation_id=$1").bind(&request.binding.operation_id).fetch_one(pool).await?,1);
            assert_eq!(sqlx::query_scalar::<_,i32>("SELECT attempts FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?,2);
            assert_eq!(sqlx::query_as::<_,(String,i32)>("SELECT method,status FROM durable_wire_requests WHERE attempt_id=$1").bind(&reservation.attempt_id).fetch_one(pool).await?,(if letter {"POST"} else {"GET"}.into(),200));
            transport.close().await?;
            let requests=fixture.finish().await?;
            assert_eq!(requests.len(),1);
            assert_eq!(requests[0].0,if letter {"/Directory/School/DirectoryLetter?LetterBtn=A"} else {"/Directory/School/List"});
        }
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_receipts").fetch_one(pool).await?,4);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_attempts").fetch_one(pool).await?,4);
        Ok(())
    })).await
}
