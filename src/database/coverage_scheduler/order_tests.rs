use std::sync::Arc;

use super::{
    Cache, load,
    model::{Key, Snapshot, conflicts},
    refresh,
};
use crate::{durable::pacing::test_support::with_database, scope::LOWER_48};
use anyhow::{Context, Result};

fn ordered() -> Result<String> {
    let query = include_str!("../profile_coverage.sql");
    let order = query
        .split_once("    ORDER BY COALESCE(d.state='active'")
        .context("ordering clause")?
        .1
        .split_once("    LIMIT 1")
        .context("selection bound")?
        .0;
    let rank = format!("row_number() OVER (ORDER BY COALESCE(d.state='active'{order}) AS ordinal");
    Ok(query
        .replace(
            "SELECT j.id,j.attempts FROM jobs j",
            &format!("SELECT j.id,j.attempts,{rank} FROM jobs j"),
        )
        .replace("    LIMIT 1\n", "")
        + " ORDER BY j.ordinal")
}

#[tokio::test]
async fn every_compiled_candidate_matches_the_full_sql_order_including_aliases_retries_and_owned_schools()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        sqlx::raw_sql(include_str!("../../../tests/profile_coverage_selection_parity/seed.sql")).execute(pool).await?;
        let active: Vec<i64>=sqlx::query_scalar("SELECT DISTINCT page_id FROM durable_page_operations WHERE state='active' ORDER BY page_id LIMIT 3").fetch_all(pool).await?;
        let cache=Cache::default();
        let query=ordered()?;
        let mut compared=0;
        for states in [vec![],vec!["MI".into()],vec!["WI".into()]] {
            for years in [vec![2027],vec![2028],vec![2027,2028]] {
                for contacts in [false,true] {
                    for generation in [None,Some("parity-generation")] {
                        let key=Key {states:states.clone(),years:years.clone(),generation:generation.map(str::to_owned),max_attempts:3,contacts};
                        let mut transaction=pool.begin().await?;
                        sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL work_mem='16MB'").execute(&mut *transaction).await?;
                        let prepared=refresh(&mut transaction,&cache,&key,None).await?;
                        let snapshot=Snapshot {key:key.clone(),visibility:String::new(),prepared:Arc::clone(&prepared)};
                        for owned_ids in [&[][..],active.as_slice()] {
                            let owned=load::owned(&mut transaction,if generation.is_some(){owned_ids}else{&[]}).await?;
                            for allow in [true,false] {
                                for exhausted in [false,true] {
                                    let expected: Vec<(i64,serde_json::Value)>=sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
                                        .bind(None::<String>).bind(3_i32).bind(&states).bind(&years).bind(exhausted)
                                        .bind(LOWER_48.as_slice()).bind(contacts).bind(generation).bind(generation.map(|_|1000_i64))
                                        .bind(owned_ids).bind(allow).fetch_all(&mut *transaction).await?;
                                    let actual: Vec<i64>=prepared.candidates.iter().filter(|candidate| {
                                        let deferred=candidate.operation.as_ref().filter(|operation| operation.state=="deferred");
                                        (allow || deferred.is_none()) && deferred.is_none_or(|operation|operation.retry_at_ms.is_none_or(|due|due<=1000))
                                            && !owned.iter().any(|page|conflicts(&candidate.fact,page))
                                            && (generation.is_some() || (candidate.fact.attempts>=3)==exhausted)
                                    }).map(|candidate|candidate.fact.id).collect();
                                    let expected_ids: Vec<i64>=expected.into_iter().map(|row|row.0).collect();
                                    assert_eq!(actual,expected_ids,"states {states:?} years {years:?} contacts {contacts} gen {generation:?} owned {owned_ids:?} allow {allow} exhausted {exhausted}");
                                    let first=prepared.candidates.iter().filter(|candidate| {
                                        let deferred=candidate.operation.as_ref().filter(|operation|operation.state=="deferred");
                                        (allow||deferred.is_none()) && deferred.is_none_or(|operation|operation.retry_at_ms.is_none_or(|due|due<=1000)) && !owned.iter().any(|page|conflicts(&candidate.fact,page))
                                    }).next().filter(|candidate|generation.is_some()||(candidate.fact.attempts>=3)==exhausted).map(|candidate|candidate.fact.id);
                                    assert_eq!(snapshot.selected(1000,allow,&owned,exhausted),first);
                                    compared+=actual.len();
                                }
                            }
                        }
                        transaction.rollback().await?;
                    }
                }
            }
        }
        assert!(compared>1000,"full ordered comparison must cover a substantial candidate population");
        cache.drain().await?;
        Ok(())
    })).await
}

async fn warm_order(
    database: &super::Database,
    key: &Key,
    previous: Option<&Arc<Snapshot>>,
) -> Result<Arc<Snapshot>> {
    let mut transaction = database.pool.begin().await?;
    let snapshot = super::read_snapshot(&mut transaction, database, key.clone(), previous).await?;
    let expected: Vec<(i64, serde_json::Value)> = sqlx::query_as(sqlx::AssertSqlSafe(ordered()?))
        .bind(None::<String>)
        .bind(3_i32)
        .bind(&key.states)
        .bind(&key.years)
        .bind(false)
        .bind(LOWER_48.as_slice())
        .bind(true)
        .bind(key.generation.as_deref())
        .bind(Some(1000_i64))
        .bind(&[] as &[i64])
        .bind(true)
        .fetch_all(&mut *transaction)
        .await?;
    let actual: Vec<i64> = snapshot
        .prepared
        .candidates
        .iter()
        .filter(|candidate| {
            candidate
                .operation
                .as_ref()
                .filter(|operation| operation.state == "deferred")
                .is_none_or(|operation| operation.retry_at_ms.is_none_or(|due| due <= 1000))
        })
        .map(|candidate| candidate.fact.id)
        .collect();
    assert_eq!(
        actual,
        expected.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        "every incremental candidate must retain the exact full SQL ordering"
    );
    transaction.commit().await?;
    Ok(snapshot)
}

#[tokio::test]
async fn incremental_school_groups_preserve_every_candidate_after_alias_membership_retry_and_access_changes()
-> Result<()> {
    with_database(|pool|Box::pin(async move {
        sqlx::raw_sql(include_str!("../../../tests/profile_coverage_selection_parity/seed.sql")).execute(pool).await?;
        let database=super::Database {pool:pool.clone(),coverage:Cache::default()};
        let key=Key {states:Vec::new(),years:vec![2027],generation:Some("parity-generation".into()),max_attempts:3,contacts:true};
        let mut snapshot=warm_order(&database,&key,None).await?;
        for statement in [
            "UPDATE scrape_pages SET attempts=attempts+1 WHERE id=(SELECT min(id) FROM scrape_pages WHERE status='pending')",
            "UPDATE scrape_pages SET status='succeeded' WHERE id=(SELECT min(id) FROM scrape_pages WHERE cursor->>'kind'='athlete' AND status='pending')",
            "UPDATE durable_page_operations SET retry_at_ms=999 WHERE state='deferred'",
            "UPDATE durable_page_operations SET cycle=cycle+1 WHERE state='active'",
            "UPDATE athlete_sources SET profile_url=profile_url||'-changed' WHERE source_id='10001'",
            "UPDATE scrape_pages SET cursor=cursor#-'{context,school_id}' WHERE cursor->'context'->>'athlete_id'='10001'",
            "UPDATE athlete_sources SET school_id=(SELECT id FROM schools WHERE source_id='1002') WHERE source_id='10001'",
            "UPDATE athlete_sources SET graduation_year=2028 WHERE source_id='10002'",
            "DELETE FROM athlete_sources WHERE source_id='10003'",
            "UPDATE scrape_pages SET status='blocked',cursor=jsonb_set(cursor,'{access_stop_url}',to_jsonb(url)) WHERE cursor->>'kind'='contacts'",
            "UPDATE scrape_pages SET cursor=jsonb_set(cursor,'{context,acquisition_state}','\"CO\"') WHERE cursor->'context'->>'athlete_id'='10004'",
            "DELETE FROM durable_page_operations WHERE state='deferred'",
            "DELETE FROM scrape_pages p WHERE cursor->>'kind'='roster' AND NOT EXISTS(SELECT 1 FROM durable_page_operations d WHERE d.page_id=p.id)",
            "UPDATE schools SET source_id=source_id||'9' WHERE source_id='1001'",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(statement)).execute(pool).await?;
            snapshot=warm_order(&database,&key,Some(&snapshot)).await?;
        }
        database.coverage.drain().await?;
        Ok(())
    })).await
}
