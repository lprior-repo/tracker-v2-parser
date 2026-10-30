use super::support::*;
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, IndexRefreshPermit, PageOutcome, ReservationDecision},
    },
    model::PageKind,
};
use anyhow::{Context, Result};

#[tokio::test]
async fn wiaa_index_refresh_denies_unrelated_succeeded_pages_and_unreviewed_failed_pages()
-> Result<()> {
    with_database(|pool|Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let root=tempfile::tempdir()?;
        let config=config("2028,2029")?;
        let now=chrono::Utc::now().timestamp_millis();
        let scope=scope(root.path(),&config,"negative-index",now)?;
        let mut targets=Vec::new();
        for url in ["https://school.example/staff","https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=1","https://schools.wiaawi.org/Directory/School/List?extra=1","http://schools.wiaawi.org/Directory/School/List"] {
            let mut value=target(false);value.url=url.into();targets.push(value);
        }
        let mut no_marker=target(false);no_marker.context.subject_id=None;targets.push(no_marker);
        for state in [None,Some("MI".into())] {
            let mut wrong_state=target(false);wrong_state.context.state=state;targets.push(wrong_state);
        }
        let mut wrong_method=target(false);wrong_method.method=Some("POST".into());targets.push(wrong_method);
        let mut wrong_letter=target(true);wrong_letter.method=Some("GET".into());targets.push(wrong_letter);
        let mut noncanonical_letter=target(true);noncanonical_letter.url.push('&');targets.push(noncanonical_letter);
        let mut wrong_kind=target(false);wrong_kind.kind=PageKind::Team;wrong_kind.url="https://wi.milesplit.com/teams/123-school".into();targets.push(wrong_kind);
        for target in targets {
            let request=page::bind_target(&database,target,&scope).await?;
            sqlx::query("UPDATE scrape_pages SET status='succeeded',attempts=3 WHERE id=$1").bind(request.binding.page_id).execute(pool).await?;
            assert!(IndexRefreshPermit::bind(&config,&request)?.is_none());
            let before=snapshot(&database).await?;
            assert!(page::reserve_configured(&database,&config,&request,now).await.is_err());
            assert_eq!(snapshot(&database).await?,before);
        }
        let request=page::bind_target(&database,target(true),&scope).await?;
        sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error='invalid required identity' WHERE id=$1").bind(request.binding.page_id).execute(pool).await?;
        assert!(IndexRefreshPermit::bind(&config,&request)?.is_some());
        assert!(matches!(page::reserve_configured(&database,&config,&request,now).await?,ReservationDecision::Settled {outcome:PageOutcome::Rejected {..}}));
        assert_eq!(sqlx::query_as::<_,(String,i32,String)>("SELECT status,attempts,last_error FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?,("failed".into(),3,"invalid required identity".into()));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_attempts").fetch_one(pool).await?,0);
        let fixture=WireFixture::start(FixtureKind::Page {status:200,body:"unused"}).await?;
        assert_eq!(fixture.finish().await?.len(),0);
        Ok(())
    })).await
}

#[tokio::test]
async fn wiaa_index_refresh_requires_bound_wisconsin_contact_scope_and_preserves_stop_owner_guards()
-> Result<()> {
    with_database(|pool|Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let root=tempfile::tempdir()?;
        let configured=config("2028,2029")?;
        let now=chrono::Utc::now().timestamp_millis();
        let old_scope=scope(root.path(),&configured,"old-index",now)?;
        let target=target(false);
        assert!(matches!(page::offline_replay(&database,root.path(),&old_scope,target.clone(),capture(&target,body(false))).await?,PageOutcome::Completed {..}));
        for (index,input) in ["states=['MI']\n[acquisition]\nmode='public_results'\ncollect_contacts=true", "states=['WI']\n[acquisition]\nmode='public_results'\ncollect_contacts=false", "states=['WI']\n[acquisition]\nmode='public_results'\ncollect_contacts=true\n[sources.milesplit]\nenabled=false"].into_iter().enumerate() {
            let config=crate::config::Config::parse(input)?;
            let scope=scope(root.path(),&config,&format!("denied-config-{index}"),now)?;
            let request=page::bind_target(&database,target.clone(),&scope).await?;
            assert!(IndexRefreshPermit::bind(&config,&request)?.is_none());
            let before=snapshot(&database).await?;
            assert!(page::reserve_configured(&database,&config,&request,now).await.is_err());
            assert_eq!(snapshot(&database).await?,before);
        }
        let scope=scope(root.path(),&configured,"fresh-index",now)?;
        let request=page::bind_target(&database,target.clone(),&scope).await?;
        let mut wrong=configured.clone();wrong.graduation_years=vec![2027];
        let before=snapshot(&database).await?;
        assert!(IndexRefreshPermit::bind(&wrong,&request).is_err());
        assert!(page::reserve_configured(&database,&wrong,&request,now).await.is_err());
        assert_eq!(snapshot(&database).await?,before);
        sqlx::query("UPDATE scrape_pages SET status='blocked' WHERE id=$1").bind(request.binding.page_id).execute(pool).await?;
        let before=snapshot(&database).await?;
        assert!(page::reserve_configured(&database,&configured,&request,now).await.err().context("blocked error")?.to_string().contains("checkpoint is blocked"));
        assert_eq!(snapshot(&database).await?,before);
        sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE id=$1").bind(request.binding.page_id).execute(pool).await?;
        assert!(matches!(page::reserve_configured(&database,&configured,&request,now).await?,ReservationDecision::Reserved {..}));
        let scope=super::support::scope(root.path(),&configured,"foreign-index",now)?;
        let other=page::bind_target(&database,target,&scope).await?;
        let before=snapshot(&database).await?;
        assert!(page::reserve_configured(&database,&configured,&other,now).await.err().context("owner error")?.to_string().contains("explicit handoff required"));
        assert_eq!(snapshot(&database).await?,before);
        Ok(())
    })).await
}
