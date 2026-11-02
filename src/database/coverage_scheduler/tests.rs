use super::{Cache, Database, Request, select};
use crate::durable::pacing::test_support::with_database;
use anyhow::{Context, Result};

async fn oracle(database: &Database, request: &Request<'_>) -> Result<Option<i64>> {
    let row =
        sqlx::query_as::<_, (i64, serde_json::Value)>(include_str!("../profile_coverage.sql"))
            .bind(None::<String>)
            .bind(request.max_attempts)
            .bind(request.states)
            .bind(request.years)
            .bind(request.exhausted)
            .bind(crate::scope::LOWER_48.as_slice())
            .bind(request.contacts)
            .bind(request.generation)
            .bind(request.generation.map(|_| request.now_ms))
            .bind(request.owned)
            .bind(request.allow_deferred)
            .fetch_optional(database.pool())
            .await?;
    row.map(|(id, cursor)| {
        serde_json::from_value::<crate::model::Target>(cursor)?;
        Ok(id)
    })
    .transpose()
}

#[tokio::test]
async fn compiled_candidates_match_sql_oracle_across_scopes_modes_ownership_and_time() -> Result<()>
{
    with_database(|pool| Box::pin(async move {
        let database=Database { pool:pool.clone(),coverage:Cache::default() };
        sqlx::raw_sql(include_str!("../../../tests/profile_coverage_selection_parity/seed.sql")).execute(pool).await?;
        let active: Vec<i64>=sqlx::query_scalar("SELECT DISTINCT page_id FROM durable_page_operations WHERE state='active' ORDER BY page_id").fetch_all(pool).await?;
        let mut comparisons=0;
        for states in [vec![],vec!["MI".into()],vec!["WI".into()],vec!["MI".into(),"WI".into()]] {
            for years in [vec![2027],vec![2028],vec![2027,2028],vec![]] {
                for contacts in [true,false] {
                    for generation in [None,Some("parity-generation")] {
                        for allow_deferred in [true,false] {
                            for owned in [&[][..], &active[..1],&active[..3]] {
                                for exhausted in [false,true] {
                                    let request=Request {states:&states,years:&years,generation,max_attempts:3,contacts,now_ms:1000,owned,allow_deferred,exhausted};
                                    let expected=oracle(&database,&request).await;
                                    let actual=select(&database,request).await.map(|job|job.map(|job|job.id));
                                    match (actual,expected) {
                                        (Ok(actual),Ok(expected))=>assert_eq!(actual,expected,"states {states:?} years {years:?} contacts {contacts} generation {generation:?} deferred {allow_deferred} owned {owned:?} exhausted {exhausted}"),
                                        (Err(actual),Err(expected))=>assert_eq!(actual.root_cause().to_string(),expected.root_cause().to_string()),
                                        (actual,expected)=>panic!("selector result {actual:?} differs from oracle {expected:?}"),
                                    }
                                    comparisons+=1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(comparisons,768);
        database.coverage.drain().await?;
        Ok(())
    })).await
}

fn request<'a>(states: &'a [String]) -> Request<'a> {
    Request {
        states,
        years: &[2027],
        generation: None,
        max_attempts: 3,
        contacts: true,
        now_ms: 1000,
        owned: &[],
        allow_deferred: true,
        exhausted: false,
    }
}

#[tokio::test]
async fn late_commit_and_rollback_refresh_a_warm_snapshot_without_losing_lower_transaction_ids()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database=Database {pool:pool.clone(),coverage:Cache::default()};
        sqlx::raw_sql(include_str!("../../../tests/profile_coverage_selection_parity/seed.sql")).execute(pool).await?;
        let states=["MI".to_owned()];
        let first=select(&database,request(&states)).await?.context("first candidate")?.id;
        let mut early=pool.begin().await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked' WHERE id=$1").bind(first).execute(&mut *early).await?;
        assert_eq!(select(&database,request(&states)).await?.context("uncommitted row remains eligible")?.id,first);
        let mut later=pool.begin().await?;
        sqlx::query("UPDATE scrape_pages SET attempts=attempts+1 WHERE id=(SELECT max(id) FROM scrape_pages)").execute(&mut *later).await?;
        later.commit().await?;
        assert_eq!(select(&database,request(&states)).await?.map(|job|job.id),oracle(&database,&request(&states)).await?);
        early.commit().await?;
        let next=select(&database,request(&states)).await?.map(|job|job.id);
        assert_ne!(next,Some(first));
        assert_eq!(next,oracle(&database,&request(&states)).await?);
        let mut rollback=pool.begin().await?;
        sqlx::query("UPDATE athletes SET graduation_year=1900").execute(&mut *rollback).await?;
        rollback.rollback().await?;
        assert_eq!(select(&database,request(&states)).await?.map(|job|job.id),next);
        database.coverage.drain().await?;
        Ok(())
    })).await
}

async fn same_result(database: &Database, states: &[String]) -> Result<()> {
    let expected = oracle(database, &request(states)).await;
    let actual = select(database, request(states))
        .await
        .map(|job| job.map(|job| job.id));
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => assert_eq!(actual, expected),
        (Err(actual), Err(expected)) => assert_eq!(
            actual.root_cause().to_string(),
            expected.root_cause().to_string()
        ),
        (actual, expected) => {
            panic!("warm selector result {actual:?} differs from oracle {expected:?}")
        }
    }
    Ok(())
}

#[tokio::test]
async fn warm_selection_tracks_profile_school_page_and_bulk_changes_without_resetting_source_rows()
-> Result<()> {
    with_database(|pool|Box::pin(async move {
        let database=Database {pool:pool.clone(),coverage:Cache::default()};
        sqlx::raw_sql(include_str!("../../../tests/profile_coverage_selection_parity/seed.sql")).execute(pool).await?;
        let states=["MI".to_owned()];
        same_result(&database,&states).await?;
        for mutation in [
            "UPDATE scrape_pages SET cursor=cursor#-'{context,school_id}' WHERE cursor->'context'->>'athlete_id'='10001'",
            "UPDATE athlete_sources SET school_id=(SELECT id FROM schools WHERE source_id='1002') WHERE source_id='10001'",
            "UPDATE athlete_sources SET graduation_year=2028 WHERE source_id='10001'",
            "UPDATE athletes SET graduation_year=2028 WHERE id=4",
            "DELETE FROM athlete_sources WHERE source_id='10003'",
            "UPDATE schools SET source_id=source_id||'9' WHERE source_id='1001'",
            "UPDATE scrape_pages SET cursor=jsonb_set(cursor,'{context,acquisition_state}','\"WI\"') WHERE id=(SELECT min(id) FROM scrape_pages WHERE status='pending')",
            "UPDATE scrape_pages SET status='blocked',cursor=jsonb_set(cursor,'{access_stop_url}',to_jsonb(url)) WHERE cursor->>'kind'='contacts'",
            "UPDATE scrape_pages SET attempts=attempts+1 WHERE status='pending'",
            "TRUNCATE scrape_pages CASCADE",
        ] {
            sqlx::raw_sql(mutation).execute(pool).await?;
            same_result(&database,&states).await?;
        }
        assert!(select(&database,request(&states)).await?.is_none());
        database.coverage.drain().await?;
        Ok(())
    })).await
}

#[tokio::test]
async fn independent_school_writes_do_not_wait_on_a_shared_selector_invalidation_row() -> Result<()>
{
    with_database(|pool|Box::pin(async move {
        sqlx::raw_sql(include_str!("../../../tests/profile_coverage_selection_parity/seed.sql")).execute(pool).await?;
        let ids: Vec<i64>=sqlx::query_scalar("SELECT id FROM schools ORDER BY id LIMIT 2").fetch_all(pool).await?;
        let mut first=pool.begin().await?;
        sqlx::query("UPDATE schools SET address='first uncommitted address' WHERE id=$1").bind(ids[0]).execute(&mut *first).await?;
        let mut second=pool.begin().await?;
        tokio::time::timeout(std::time::Duration::from_secs(1),sqlx::query("UPDATE schools SET address='independent address' WHERE id=$1").bind(ids[1]).execute(&mut *second)).await.context("independent school update must complete before the first transaction releases its change record")??;
        let distinct: i64=sqlx::query_scalar("SELECT count(*) FROM selector_changes WHERE kind='school_contact' AND identity=$1").bind(ids[1].to_string()).fetch_one(&mut *second).await?;
        assert_eq!(distinct,1);
        second.rollback().await?;
        first.rollback().await?;
        Ok(())
    })).await
}

#[tokio::test]
async fn restored_tracker_values_from_a_different_xid_epoch_do_not_force_repeated_full_rebuilds()
-> Result<()> {
    with_database(|pool|Box::pin(async move {
        let database=Database {pool:pool.clone(),coverage:Cache::default()};
        sqlx::raw_sql(include_str!("../../../tests/profile_coverage_selection_parity/seed.sql")).execute(pool).await?;
        let states=["MI".to_owned()];
        let first=select(&database,request(&states)).await?.context("first candidate")?.id;
        let before=database.coverage.snapshot.lock().unwrap().as_ref().unwrap().prepared.clone();
        sqlx::query("UPDATE selector_changes SET changed_xid=(pg_snapshot_xmax(pg_current_snapshot())::text::numeric+1000000000)::text::xid8 WHERE kind='page' AND identity=$1").bind(first.to_string()).execute(pool).await?;
        assert_eq!(select(&database,request(&states)).await?.context("unchanged candidate")?.id,first);
        let after=database.coverage.snapshot.lock().unwrap().as_ref().unwrap().prepared.clone();
        assert!(std::sync::Arc::ptr_eq(&before,&after));
        sqlx::query("UPDATE scrape_pages SET status='blocked' WHERE id=$1").bind(first).execute(pool).await?;
        assert_ne!(select(&database,request(&states)).await?.map(|job|job.id),Some(first));
        same_result(&database,&states).await?;
        database.coverage.drain().await?;
        Ok(())
    })).await
}
