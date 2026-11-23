use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::{AssertSqlSafe, PgPool};
use track_scrapper_v2::database::{Database, EXPORT_BATCH_BYTES, EXPORT_BATCH_ROWS};

pub const DETAIL: &str = "unassigned result | 100m | 11.25 | recorded";
pub const LATE_CURSOR: i64 = 99_500;
pub const RESULTS: i64 = 100_000;
pub const RECEIPTS: i64 = 50_000;
const INDEX: &str = "durable_page_receipts_capture_url_hash_idx";
const COVERAGE: &str = include_str!("../../src/database/export_issues.sql");

type IssueToken = (String, String, String, i32, i64, i64, i64, bool);

pub fn capture_url(result_id: i64) -> String {
    let page = result_id
        .saturating_sub(1)
        .rem_euclid(RECEIPTS)
        .saturating_add(1);
    format!(
        "https://www.milesplit.com/meets/{page}/results/{page}/raw?retained={}",
        "a".repeat(160)
    )
}

pub async fn seed_workload(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(include_str!("seed.sql"))
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn late_issues(pool: &PgPool) -> Result<Vec<IssueToken>> {
    Ok(sqlx::query_as(COVERAGE)
        .bind(&["WI"][..])
        .bind(2_i32)
        .bind(LATE_CURSOR)
        .bind(0_i64)
        .bind(i64::try_from(EXPORT_BATCH_BYTES)?)
        .bind(i64::try_from(EXPORT_BATCH_ROWS)?)
        .fetch_all(pool)
        .await?)
}

pub async fn late_plan(pool: &PgPool) -> Result<Value> {
    let mut transaction = pool.begin().await?;
    sqlx::raw_sql(
        "SET TRANSACTION READ ONLY; SET LOCAL statement_timeout='30s'; SET LOCAL work_mem='4MB'",
    )
    .execute(&mut *transaction)
    .await?;
    let plan = sqlx::query_scalar(AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {COVERAGE}"
    )))
    .bind(&["WI"][..])
    .bind(2_i32)
    .bind(LATE_CURSOR)
    .bind(0_i64)
    .bind(i64::try_from(EXPORT_BATCH_BYTES)?)
    .bind(i64::try_from(EXPORT_BATCH_ROWS)?)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.rollback().await?;
    Ok(plan)
}

pub fn receipt_work(plan: &Value) -> Result<(f64, bool)> {
    let root = plan
        .get(0)
        .and_then(|explanation| explanation.get("Plan"))
        .context("Coverage EXPLAIN root")?;
    scan_work(root)
}

fn scan_work(node: &Value) -> Result<(f64, bool)> {
    let own = if node.get("Relation Name").and_then(Value::as_str) == Some("durable_page_receipts")
    {
        let metric = |name| node.get(name).and_then(Value::as_f64);
        let rows = metric("Actual Rows").context("receipt scan rows")?;
        let loops = metric("Actual Loops").context("receipt scan loops")?;
        let filtered = metric("Rows Removed by Filter").unwrap_or(0.0);
        let rechecked = metric("Rows Removed by Index Recheck").unwrap_or(0.0);
        (
            (rows + filtered + rechecked) * loops,
            loops > 0.0 && node.get("Index Name").and_then(Value::as_str) == Some(INDEX),
        )
    } else {
        (0.0, false)
    };
    node.get("Plans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .try_fold(own, |(work, indexed), child| {
            let (child_work, child_indexed) = scan_work(child)?;
            Ok((work + child_work, indexed || child_indexed))
        })
}

const LOOKUP_MIGRATION: i64 = 202611220001;

pub async fn migrate_prior(pool: &PgPool) -> Result<()> {
    let mut prior = sqlx::migrate!("./migrations");
    prior
        .migrations
        .to_mut()
        .retain(|migration| migration.version < LOOKUP_MIGRATION);
    prior.run(pool).await?;
    Ok(())
}

pub async fn long_url(pool: &PgPool) -> Result<String> {
    Ok(sqlx::query_scalar(
        "SELECT 'https://www.milesplit.com/meets/900/results/900/raw?retained=' ||
         string_agg(md5(n::text), '' ORDER BY n) FROM generate_series(1,245) n",
    )
    .fetch_one(pool)
    .await?)
}

pub async fn retain_capture(
    pool: &PgPool,
    label: &str,
    source: &str,
    state: &str,
    url: &str,
) -> Result<()> {
    sqlx::query(
        "WITH page AS (
            INSERT INTO scrape_pages(source,page_key,url,cursor,status)
            VALUES ($2,md5($1)||md5($1),'https://wi.milesplit.com/request/'||$1,
                    jsonb_build_object('kind','results','context',jsonb_build_object(
                        'acquisition_state',$3::text)), 'succeeded')
            RETURNING id
        ), operation AS (
            INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,original_attempts)
            SELECT repeat(md5('operation:'||$1),2),id,'receipt-lookup','{}','completed',0 FROM page
            RETURNING operation_id,page_id
        ), attempt AS (
            INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts,state)
            SELECT repeat(md5('attempt:'||$1),2),operation_id,1,1,1,'completed' FROM operation
            RETURNING operation_id,attempt_id
        )
        INSERT INTO durable_page_receipts(operation_id,attempt_id,page_id,capture,parsed,receipt_sha256,outcome)
        SELECT operation_id,attempt_id,page_id,jsonb_build_object('url',$4::text),
               '{}',repeat('0',64),'{}' FROM operation JOIN attempt USING(operation_id)",
    )
    .bind(label)
    .bind(source)
    .bind(state)
    .bind(url)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn retained_receipts(pool: &PgPool) -> Result<Vec<Value>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(r) FROM durable_page_receipts r ORDER BY page_id")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn unassigned_result(pool: &PgPool, label: &str, url: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO results(source_result_key,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw)
         VALUES(repeat(md5($1),2),'milesplit','100m','11.25','outdoor','recorded',false,false,$2,'{}')",
    )
    .bind(label)
    .bind(url)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn assert_public_issues(
    database: &Database,
    states: &[String],
    urls: &[&str],
) -> Result<()> {
    let (athletes, issues) = database.export_snapshot(states, &[2028]).await?;
    assert!(athletes.is_empty());
    let actual: Vec<_> = issues
        .iter()
        .map(|issue| {
            (
                issue.source.as_str(),
                issue.url.as_str(),
                issue.detail.as_str(),
            )
        })
        .collect();
    let expected: Vec<_> = urls.iter().map(|url| ("milesplit", *url, DETAIL)).collect();
    assert_eq!(actual, expected, "Coverage scope {states:?}");
    Ok(())
}
