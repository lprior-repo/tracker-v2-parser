mod support;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use sqlx::{PgConnection, PgPool};
use track_scrapper_v2::database::Database;

async fn checked_out(pool: &PgPool, pid: i32, monitor: &PgPool) -> Result<()> {
    let mut connection = pool.acquire().await?;
    let actual: (i32, String) =
        sqlx::query_as("SELECT pg_backend_pid(),current_setting('work_mem')")
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(actual, (pid, "4MB".into()));
    drop(connection);
    let idle: bool = sqlx::query_scalar(
        "SELECT state='idle' AND xact_start IS NULL FROM pg_stat_activity WHERE pid=$1",
    )
    .bind(pid)
    .fetch_one(monitor)
    .await?;
    assert!(
        idle,
        "selector must return the same pooled backend without an open transaction"
    );
    Ok(())
}

async fn blocked(pool: &PgPool, pid: i32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT COALESCE(state='active' AND wait_event_type='Lock' AND xact_start IS NOT NULL,false) FROM pg_stat_activity WHERE pid=$1")
                .bind(pid).fetch_one(pool).await?;
            if waiting { return Ok::<_,anyhow::Error>(()); }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.context("selector must reach its statement after transaction-local configuration")?
}

async fn baseline(connection: &mut PgConnection) -> Result<i32> {
    sqlx::query("SET work_mem='4MB'")
        .execute(&mut *connection)
        .await?;
    Ok(sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(connection)
        .await?)
}

#[tokio::test]
async fn given_a_selector_pool_when_success_failure_and_cancellation_occur_then_readonly_memory_scope_is_reset_before_reuse()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        sqlx::raw_sql("INSERT INTO athletes(name,graduation_year) VALUES('Memory Scope Runner',2027); INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,graduation_year) SELECT id,'milesplit','904','https://www.milesplit.com/athletes/904-runner',2027 FROM athletes; INSERT INTO scrape_pages(source,page_key,url,cursor) VALUES('milesplit',repeat('a',64),'https://www.milesplit.com/athletes/904-runner','{\"source\":\"milesplit\",\"kind\":\"athlete\",\"url\":\"https://www.milesplit.com/athletes/904-runner\",\"context\":{\"athlete_id\":\"904\",\"state\":\"MI\"}}'); CREATE FUNCTION enforce_selector_settings() RETURNS boolean LANGUAGE plpgsql STABLE AS $$ BEGIN IF current_setting('work_mem')<>'16MB' OR current_setting('transaction_read_only')<>'on' THEN RAISE EXCEPTION 'selector memory/read-only configuration was not local'; END IF; RETURN TRUE; END $$; ALTER TABLE athlete_sources RENAME TO guarded_profiles; CREATE VIEW athlete_sources AS SELECT * FROM guarded_profiles WHERE enforce_selector_settings();")
            .execute(&test.pool).await?;
        let mut held=Vec::new();
        for _ in 0..7 { held.push(database.pool().acquire().await?); }
        let mut remaining=database.pool().acquire().await?;
        let pid=baseline(&mut remaining).await?;
        drop(remaining);
        let states=["MI".into()];
        let job=database.next_profile_coverage_job(None,3,&states,&[2027],false).await.context("cold compiled selection under enforced settings")?.context("valid row under enforced read-only16MB selection")?;
        assert_eq!(job.id,1);
        checked_out(database.pool(),pid,&test.pool).await.context("pooled connection after successful compiled selection")?;
        sqlx::query("SELECT selector_changed('contact','qualification',true)").execute(&test.pool).await.context("invalidating contact coverage before cancellation")?;
        let mut lock=test.pool.begin().await?;
        sqlx::query("LOCK TABLE contacts IN ACCESS EXCLUSIVE MODE").execute(&mut *lock).await?;
        let mut selection=Box::pin(database.next_profile_coverage_job(None,3,&states,&[2027],false));
        tokio::select! {
            result=&mut selection => bail!("selector should block before cancellation: {result:?}"),
            result=blocked(&test.pool,pid) => result.context("observing blocked compiled refresh")?,
        }
        drop(selection);
        lock.rollback().await?;
        tokio::time::timeout(Duration::from_secs(3),checked_out(database.pool(),pid,&test.pool)).await.context("cancelled transaction must release its pooled connection")??;
        sqlx::query("DROP TABLE contacts CASCADE").execute(&test.pool).await?;
        let error=database.next_profile_coverage_job(None,3,&states,&[2027],false).await.expect_err("statement failure after SET LOCAL must roll back");
        assert!(format!("{error:#}").contains("contacts"));
        checked_out(database.pool(),pid,&test.pool).await?;
        drop(held);
        database.close().await;
        Ok(())
    })).await
}
