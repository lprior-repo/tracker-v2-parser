#[path = "support/records.rs"]
mod records;
mod support;

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::{PgConnection, PgPool};
use track_scrapper_v2::{
    database::Database,
    model::{ParsedPage, School, Source},
};

fn schools() -> Vec<School> {
    vec![
        School {
            id: "101".into(),
            name: "First School".into(),
            ..Default::default()
        },
        School {
            id: "102".into(),
            name: "Second School".into(),
            ..Default::default()
        },
    ]
}

async fn hold(pool: &PgPool, existing: bool) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
    let trigger = if existing {
        "CREATE TRIGGER hold_school BEFORE UPDATE ON schools FOR EACH ROW EXECUTE FUNCTION hold_school_write();"
    } else {
        "CREATE TRIGGER hold_school BEFORE INSERT ON schools FOR EACH ROW EXECUTE FUNCTION hold_school_write();"
    };
    sqlx::raw_sql("CREATE FUNCTION hold_school_write() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7318432); RETURN NEW; END $$;")
        .execute(pool).await?;
    sqlx::raw_sql(trigger).execute(pool).await?;
    let mut gate = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock(hashtext(current_schema()),7318432)")
        .execute(&mut *gate)
        .await?;
    Ok(gate)
}

async fn release(pool: &PgPool, gate: &mut PgConnection, existing: bool) -> Result<()> {
    let wait=tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            let state: (i64,bool) = sqlx::query_as("WITH waiting AS (SELECT pid FROM pg_locks WHERE locktype='advisory' AND classid::bigint=(hashtext(current_schema())::bigint & 4294967295) AND objid=7318432 AND NOT granted) SELECT (SELECT count(*) FROM waiting),EXISTS(SELECT 1 FROM pg_stat_activity a JOIN waiting w ON w.pid=ANY(pg_blocking_pids(a.pid)))")
                .fetch_one(pool).await?;
            if (existing && state==(1,true)) || (!existing && state.0==2) {
                return Ok::<_,anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;
    let unlocked: bool =
        sqlx::query_scalar("SELECT pg_advisory_unlock(hashtext(current_schema()),7318432)")
            .fetch_one(gate)
            .await?;
    assert!(unlocked);
    wait.context("both reversed school pages must reach the controlled lock boundary")??;
    Ok(())
}

#[tokio::test]
async fn given_reversed_existing_schools_when_two_pages_commit_then_numeric_prelocks_prevent_opposite_school_ownership()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let target=records::target(Source::MileSplit,"seed");
        let mut initial=schools();initial.reverse();
        sqlx::query("INSERT INTO schools(source,source_id,name,source_url) VALUES('milesplit','102','Second School','https://mi.milesplit.com/teams/102'),('milesplit','101','First School','https://mi.milesplit.com/teams/101')")
            .execute(&test.pool).await?;
        let mut gate=hold(&test.pool,true).await?;
        let left=ParsedPage{schools:schools(),..Default::default()};
        let right=ParsedPage{schools:initial,..Default::default()};
        let other=records::target(Source::MileSplit,"other");
        let (a,b,released)=tokio::join!(records::persist(&database,&target,&left),records::persist(&database,&other,&right),release(&test.pool,&mut gate,true));
        released?;a?;b?;
        let stored: Vec<(String,String)>=sqlx::query_as("SELECT source_id,name FROM schools ORDER BY source_id").fetch_all(&test.pool).await?;
        assert_eq!(stored,vec![("101".into(),"First School".into()),("102".into(),"Second School".into())]);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM scrape_pages WHERE status='succeeded'").fetch_one(&test.pool).await?,2);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_reversed_new_schools_when_pages_create_them_concurrently_then_both_finish_with_one_identity_per_school()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut gate = hold(&test.pool, false).await?;
            let left = ParsedPage {
                schools: schools(),
                ..Default::default()
            };
            let mut reversed = schools();
            reversed.reverse();
            let right = ParsedPage {
                schools: reversed,
                ..Default::default()
            };
            let first = records::target(Source::MileSplit, "new-first");
            let second = records::target(Source::MileSplit, "new-second");
            let (a, b, released) = tokio::join!(
                records::persist(&database, &first, &left),
                records::persist(&database, &second, &right),
                release(&test.pool, &mut gate, false)
            );
            released?;
            a?;
            b?;
            let stored: Vec<(i64, String)> =
                sqlx::query_as("SELECT id,source_id FROM schools ORDER BY id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(stored.len(), 2);
            assert_eq!(
                stored.iter().map(|row| row.1.as_str()).collect::<Vec<_>>(),
                ["101", "102"]
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM scrape_pages WHERE status='succeeded'"
                )
                .fetch_one(&test.pool)
                .await?,
                2
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_duplicate_school_observations_when_sorted_for_writes_then_input_precedence_for_each_identity_is_retained()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut observed = schools();
            observed.reverse();
            observed.push(School {
                id: "102".into(),
                name: "Corrected Second".into(),
                ..Default::default()
            });
            records::persist(
                &database,
                &records::target(Source::MileSplit, "duplicate"),
                &ParsedPage {
                    schools: observed,
                    ..Default::default()
                },
            )
            .await?;
            let stored: Vec<(String, String)> =
                sqlx::query_as("SELECT source_id,name FROM schools ORDER BY source_id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(
                stored,
                vec![
                    ("101".into(), "First School".into()),
                    ("102".into(), "Corrected Second".into())
                ]
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_two_malformed_research_records_when_input_is_reversed_then_first_research_error_and_atomic_rollback_are_preserved()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let target=records::target(Source::MileSplit,"research-order");
        records::persist(&database,&target,&ParsedPage {schools:schools(),..Default::default()}).await?;
        sqlx::raw_sql("UPDATE schools SET address_provenance='{\"manual_research\":42}'::jsonb WHERE source_id='101'; UPDATE schools SET website_provenance='{}'::jsonb WHERE source_id='102';")
            .execute(&test.pool).await?;
        let before:Vec<serde_json::Value>=sqlx::query_scalar("SELECT to_jsonb(s) FROM schools s ORDER BY id").fetch_all(&test.pool).await?;
        let mut reversed=schools();reversed.reverse();
        let error=records::persist(&database,&target,&ParsedPage {schools:reversed,..Default::default()}).await.expect_err("original first observation must fail first");
        assert!(format!("{error:#}").contains("manual website research provenance is malformed"));
        assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT to_jsonb(s) FROM schools s ORDER BY id").fetch_all(&test.pool).await?,before);
        let error=records::persist(&database,&target,&ParsedPage {schools:schools(),..Default::default()}).await.expect_err("forward input must retain its original first error");
        assert!(format!("{error:#}").contains("manual research address provenance is malformed"));
        database.close().await;
        Ok(())
    })).await
}
