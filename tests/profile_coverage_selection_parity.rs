mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use track_scrapper_v2::{database::Database, scope::LOWER_48};

const BASELINE: &str = include_str!("profile_coverage_selection_parity/baseline.sql");
const CURRENT: &str = include_str!("../src/database/profile_coverage.sql");

#[derive(Clone)]
struct Scope<'a> {
    source: Option<&'a str>,
    states: &'a [String],
    years: &'a [i32],
    exhausted: bool,
    contacts: bool,
    generation: Option<&'a str>,
    owned: &'a [i64],
    deferred: bool,
}

fn ordered(query: &str) -> Result<String> {
    let order = query
        .split_once("    ORDER BY COALESCE(d.state='active'")
        .context("school-chain ordering clause")?
        .1
        .split_once("    LIMIT 1")
        .context("school-chain selection bound")?
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

async fn selected(pool: &PgPool, query: &str, scope: &Scope<'_>) -> Result<Vec<(i64, Value)>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(ordered(query)?))
        .bind(scope.source)
        .bind(3_i32)
        .bind(scope.states)
        .bind(scope.years)
        .bind(scope.exhausted)
        .bind(LOWER_48.as_slice())
        .bind(scope.contacts)
        .bind(scope.generation)
        .bind(scope.generation.map(|_| 1000_i64))
        .bind(scope.owned)
        .bind(scope.deferred)
        .fetch_all(pool)
        .await?)
}

async fn compare(pool: &PgPool, scope: &Scope<'_>) -> Result<usize> {
    let before: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id")
            .fetch_all(pool)
            .await?;
    let expected = selected(pool, BASELINE, scope).await?;
    let actual = selected(pool, CURRENT, scope).await?;
    assert_eq!(
        actual,
        expected,
        "scope {:?}, years {:?}, generation {:?}, owned {:?}, deferred {}, contacts {}, exhausted {}",
        scope.states,
        scope.years,
        scope.generation,
        scope.owned,
        scope.deferred,
        scope.contacts,
        scope.exhausted
    );
    let after: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id")
            .fetch_all(pool)
            .await?;
    assert_eq!(after, before);
    Ok(actual.len())
}

#[tokio::test]
async fn given_mixed_states_years_guards_and_school_phases_when_indexed_selection_runs_then_all_ordered_candidates_match_prior_query()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            sqlx::raw_sql(include_str!("profile_coverage_selection_parity/seed.sql"))
                .execute(&test.pool)
                .await?;
            let mut total = 0;
            for states in [
                vec![],
                vec!["MI".into()],
                vec!["WI".into()],
                vec!["WI".into(), "MI".into()],
            ] {
                for years in [vec![2027], vec![2028], vec![2027, 2028]] {
                    for contacts in [false, true] {
                        for exhausted in [false, true] {
                            total += compare(
                                &test.pool,
                                &Scope {
                                    source: Some("milesplit"),
                                    states: &states,
                                    years: &years,
                                    exhausted,
                                    contacts,
                                    generation: None,
                                    owned: &[],
                                    deferred: true,
                                },
                            )
                            .await?;
                        }
                    }
                }
            }
            assert!(total > 100);
            assert_eq!(
                compare(
                    &test.pool,
                    &Scope {
                        source: Some("athleticnet"),
                        states: &[],
                        years: &[2027],
                        exhausted: false,
                        contacts: true,
                        generation: None,
                        owned: &[],
                        deferred: true
                    }
                )
                .await?,
                0
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_owned_active_and_deferred_campaign_operations_when_indexed_selection_runs_then_retry_and_ownership_order_are_preserved()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        sqlx::raw_sql(include_str!("profile_coverage_selection_parity/seed.sql")).execute(&test.pool).await?;
        let active:Vec<i64>=sqlx::query_scalar("SELECT min(d.page_id) FROM durable_page_operations d JOIN scrape_pages p ON p.id=d.page_id WHERE d.state='active' GROUP BY p.cursor->>'kind' ORDER BY min(d.page_id)").fetch_all(&test.pool).await?;
        assert_eq!(active.len(),4);
        let mut total=0;
        for owned in [&[][..],&active[..1],&active[..3],&active[1..]] {
            for deferred in [false,true] {
                for states in [vec![],vec!["MI".into()],vec!["WI".into()]] {
                    total+=compare(&test.pool,&Scope {source:None,states:&states,years:&[2027],exhausted:false,contacts:true,generation:Some("parity-generation"),owned,deferred}).await?;
                }
            }
        }
        assert!(total>50);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_missing_null_empty_and_malformed_acquisition_states_when_selected_then_only_valid_nonempty_override_or_null_fallback_is_used()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        sqlx::raw_sql("INSERT INTO athletes(name,graduation_year) VALUES('State Runner',2027); INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,graduation_year) SELECT id,'milesplit','901','https://www.milesplit.com/athletes/901-runner',2027 FROM athletes;").execute(&test.pool).await?;
        let contexts=[
            serde_json::json!({"state":"MI"}),
            serde_json::json!({"state":"MI","acquisition_state":null}),
            serde_json::json!({"state":"WI","acquisition_state":" mi "}),
            serde_json::json!({"state":"MI","acquisition_state":""}),
            serde_json::json!({"state":"MI","acquisition_state":"WI"}),
            serde_json::json!({"state":"MI","acquisition_state":42}),
            serde_json::json!({"state":"MI","acquisition_state":{}}),
            serde_json::json!({}),
            serde_json::json!({"acquisition_state":"MI"}),
            serde_json::json!({"state":"MI","acquisition_state":false}),
            serde_json::json!({"state":"MI","acquisition_state":["MI"]}),
            serde_json::json!({"state":null,"acquisition_state":null}),
        ];
        for (i,context) in contexts.into_iter().enumerate() {
            let cursor=serde_json::json!({"source":"milesplit","kind":"athlete","url":"https://www.milesplit.com/athletes/901-runner","context":context.as_object().context("object fixture")?.iter().map(|(k,v)|(k.clone(),v.clone())).chain([("athlete_id".into(),serde_json::json!("901")),("variant".into(),serde_json::json!(i))]).collect::<serde_json::Map<_,_>>()});
            sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor) VALUES('milesplit',$1,'https://www.milesplit.com/athletes/901-runner',$2)")
                .bind(format!("{i:064x}")).bind(cursor).execute(&test.pool).await?;
        }
        let mut scope=Scope {source:None,states:&["MI".into()],years:&[2027],exhausted:false,contacts:false,generation:None,owned:&[],deferred:true};
        assert_eq!(selected(&test.pool,CURRENT,&scope).await?.iter().map(|(id,_)|*id).collect::<Vec<_>>(),vec![1,2,3,9]);
        let first=database.next_profile_coverage_job(None,3,scope.states,scope.years,false).await?.context("valid fallback selected through public API")?;
        assert_eq!(first.id,1);
        assert_eq!(first.target.context.athlete_id.as_deref(),Some("901"));
        scope.states=&[];
        assert_eq!(selected(&test.pool,CURRENT,&scope).await?.iter().map(|(id,_)|*id).collect::<Vec<_>>(),(1..=12).collect::<Vec<_>>());
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_long_incompressible_state_when_checkpoint_is_saved_then_lookup_key_remains_bounded_without_truncating_or_reclassifying_the_state()
-> Result<()> {
    use sha2::{Digest, Sha256};
    use track_scrapper_v2::model::{PageContext, PageKind, Source, Target};
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let state=format!("MI{}",(0_u32..512).map(|n|format!("{:x}",Sha256::digest(n.to_be_bytes()))).collect::<String>());
        let target=Target::get(Source::MileSplit,PageKind::Athlete,"https://www.milesplit.com/athletes/902-runner".into(),PageContext {athlete_id:Some("902".into()),state:Some("WI".into()),acquisition_state:Some(state.clone()),..Default::default()});
        sqlx::raw_sql("INSERT INTO athletes(name,graduation_year) VALUES('Long State Runner',2027); INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,graduation_year) SELECT id,'milesplit','902','https://www.milesplit.com/athletes/902-runner',2027 FROM athletes;").execute(&test.pool).await?;
        assert_eq!(database.enqueue(std::slice::from_ref(&target),false).await?,1);
        let rejected=sqlx::query("CREATE INDEX unbounded_state_regression ON scrape_pages((upper(btrim(COALESCE(cursor->'context'->>'acquisition_state',cursor->'context'->>'state')))))")
            .execute(&test.pool).await.expect_err("unbounded text index must reproduce PostgreSQL tuple-size failure");
        assert!(rejected.as_database_error().is_some_and(|error|error.code().is_some_and(|code|code=="54000")));
        let mut scope=Scope {source:None,states:&["MI".into()],years:&[2027],exhausted:false,contacts:false,generation:None,owned:&[],deferred:true};
        assert_eq!(compare(&test.pool,&scope).await?,0);
        let requested=[state.to_uppercase()];scope.states=&requested;
        assert_eq!(compare(&test.pool,&scope).await?,1);
        let job=database.next_profile_coverage_job(None,3,&[],&[2027],false).await?.context("unrestricted long-state checkpoint retained")?;
        assert_eq!(job.target.context.acquisition_state.as_deref(),Some(state.as_str()));
        let retained:Value=sqlx::query_scalar("SELECT cursor FROM scrape_pages WHERE id=$1").bind(job.id).fetch_one(&test.pool).await?;
        sqlx::query("DROP INDEX scrape_pages_profile_scope_lookup_idx").execute(&test.pool).await?;
        sqlx::raw_sql(include_str!("../migrations/202611010001_profile_scope_lookup.sql")).execute(&test.pool).await?;
        let statistics:(f64,f64)=sqlx::query_as("SELECT s.n_distinct::float8,s.null_frac::float8 FROM pg_stats s JOIN pg_attribute a ON a.attrelid='scrape_pages_profile_scope_lookup_idx'::regclass AND a.attnum=2 AND a.attname=s.attname WHERE s.schemaname=current_schema() AND s.tablename='scrape_pages_profile_scope_lookup_idx'").fetch_one(&test.pool).await?;
        assert_eq!(statistics,(-1.0,0.0));
        let after:Value=sqlx::query_scalar("SELECT cursor FROM scrape_pages WHERE id=$1").bind(job.id).fetch_one(&test.pool).await?;
        assert_eq!(after,retained);
        assert_eq!(compare(&test.pool,&scope).await?,1);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_multibyte_requested_states_when_scoped_selection_runs_then_postgresql_character_boundaries_and_exact_scope_are_preserved()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        sqlx::raw_sql("INSERT INTO athletes(name,graduation_year) VALUES('Unicode Scope Runner',2027); INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,graduation_year) SELECT id,'milesplit','903','https://www.milesplit.com/athletes/903-runner',2027 FROM athletes;").execute(&test.pool).await?;
        let states=["😀MI","😀WI","M😀I","M😀W","ÉÀ","A🦋MI",""];
        for (index,state) in states.iter().enumerate() {
            let cursor=serde_json::json!({"source":"milesplit","kind":"athlete","url":"https://www.milesplit.com/athletes/903-runner","context":{"athlete_id":"903","state":state}});
            sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor) VALUES('milesplit',$1,'https://www.milesplit.com/athletes/903-runner',$2)")
                .bind(format!("{index:064x}")).bind(cursor).execute(&test.pool).await?;
        }
        let before:Vec<Value>=sqlx::query_scalar("SELECT cursor FROM scrape_pages ORDER BY id").fetch_all(&test.pool).await?;
        for (index,state) in states.iter().enumerate() {
            let wanted=[(*state).to_owned()];
            let scope=Scope {source:None,states:&wanted,years:&[2027],exhausted:false,contacts:false,generation:None,owned:&[],deferred:true};
            assert_eq!(compare(&test.pool,&scope).await?,1);
            let job=database.next_profile_coverage_job(None,3,&wanted,&[2027],false).await?.context("exact Unicode state through native database API")?;
            assert_eq!(job.id,i64::try_from(index+1)?);
            assert_eq!(job.target.context.state.as_deref(),Some(*state));
        }
        assert_eq!(sqlx::query_scalar::<_,Value>("SELECT cursor FROM scrape_pages ORDER BY id").fetch_all(&test.pool).await?,before);
        database.close().await;
        Ok(())
    })).await
}
