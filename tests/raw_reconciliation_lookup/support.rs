use anyhow::Result;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

pub const MEET: &str = "results_milesplit_meet_hash_idx";
pub const OBSERVED: &str = "results_milesplit_observed_file_hash_idx";
pub const UNASSIGNED: &str = "results_milesplit_unassigned_file_hash_idx";
const MIGRATION: &str = include_str!("../../migrations/202611130001_raw_reconciliation_lookup.sql");
const FILE: &str = "SELECT id FROM results WHERE source='milesplit' AND ((raw#>>'{source_values,kind}'='raw_result' AND raw#>>'{source_values,published_file_identity}'=$1) OR (raw#>>'{raw,kind}'='raw_result' AND raw#>>'{raw,published_file_identity}'=$1)) ORDER BY id";
const ADOPTION: &str = "SELECT id,source_result_id,source_url,raw FROM results WHERE source='milesplit' AND meet_id=ANY($1) AND (source_result_id LIKE 'raw:%' OR source_result_id LIKE 'table:%') AND NOT COALESCE((raw#>>'{source_values,kind}'='raw_result' AND raw#>>'{source_values,published_file_identity}' ~ '^milesplit-file:[0-9]+:[0-9]+$') OR (raw#>>'{raw,kind}'='raw_result' AND raw#>>'{raw,published_file_identity}' ~ '^milesplit-file:[0-9]+:[0-9]+$'),FALSE) ORDER BY id LIMIT 10001 FOR UPDATE";

pub fn raw(branch: &str, kind: &str, file: &str) -> Value {
    json!({branch: {"kind":kind,"published_file_identity":file}})
}

pub async fn insert(
    pool: &PgPool,
    key: &str,
    source: &str,
    meet: Option<&str>,
    raw: Value,
) -> Result<i64> {
    Ok(sqlx::query_scalar("INSERT INTO results(source_result_key,source,source_result_id,event,raw_mark,sport,meet_id,status,is_personal_record,is_relay,source_url,raw) VALUES(md5($1)||md5($1),$2,'raw:'||$1,'800m','2:00.00','outdoor',$3,'recorded',FALSE,FALSE,'https://co.milesplit.com/retained',$4) RETURNING id")
        .bind(key).bind(source).bind(meet).bind(raw).fetch_one(pool).await?)
}

pub async fn remove_indexes(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql("DROP INDEX results_milesplit_meet_hash_idx; DROP INDEX results_milesplit_observed_file_hash_idx; DROP INDEX results_milesplit_unassigned_file_hash_idx")
        .execute(pool).await?;
    Ok(())
}

pub async fn install(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(MIGRATION).execute(pool).await?;
    Ok(())
}

pub async fn seed(pool: &PgPool) -> Result<()> {
    sqlx::query("INSERT INTO results(source_result_key,source,source_result_id,event,raw_mark,sport,meet_id,status,is_personal_record,is_relay,source_url,raw) SELECT md5(i::text)||md5(i::text),'milesplit','raw:'||i,'800m','2:00.00','outdoor',i::text,'recorded',FALSE,FALSE,'https://co.milesplit.com/meets/'||i,jsonb_build_object(CASE WHEN i%2=0 THEN 'source_values' ELSE 'raw' END,jsonb_build_object('kind','raw_result','published_file_identity','milesplit-file:'||i||':1','payload',repeat(md5(i::text),8))) FROM generate_series(1,50000) i")
        .execute(pool).await?;
    sqlx::query("ANALYZE results").execute(pool).await?;
    Ok(())
}

pub async fn snapshot(pool: &PgPool) -> Result<Vec<Value>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(r) FROM results r ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn file_rows(pool: &PgPool, file: &str) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(FILE).bind(file).fetch_all(pool).await?)
}

pub async fn meet_rows(pool: &PgPool, meets: &[String]) -> Result<Vec<i64>> {
    let mut transaction = pool.begin().await?;
    let rows = sqlx::query(ADOPTION)
        .bind(meets)
        .fetch_all(&mut *transaction)
        .await?;
    transaction.rollback().await?;
    rows.iter()
        .map(|row| row.try_get(0).map_err(Into::into))
        .collect()
}

pub async fn explain(pool: &PgPool, value: &str, meet: bool, generic: bool) -> Result<Value> {
    let mut transaction = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *transaction)
        .await?;
    let query = if meet { ADOPTION } else { FILE };
    let plan = if generic {
        let query = format!("EXPLAIN(GENERIC_PLAN,FORMAT JSON) {query}");
        sqlx::raw_sql(sqlx::AssertSqlSafe(query.as_str()))
            .fetch_one(&mut *transaction)
            .await?
            .try_get(0)?
    } else {
        let query = format!("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) {query}");
        if meet {
            sqlx::query_scalar(sqlx::AssertSqlSafe(query.as_str()))
                .bind(vec![value])
                .fetch_one(&mut *transaction)
                .await?
        } else {
            sqlx::query_scalar(sqlx::AssertSqlSafe(query.as_str()))
                .bind(value)
                .fetch_one(&mut *transaction)
                .await?
        }
    };
    transaction.rollback().await?;
    Ok(plan)
}

fn nodes(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(rows) => rows.iter().flat_map(nodes).collect(),
        Value::Object(row) => std::iter::once(value)
            .chain(row.values().flat_map(nodes))
            .collect(),
        _ => Vec::new(),
    }
}

pub fn has_index(plan: &Value, name: &str) -> bool {
    nodes(plan)
        .iter()
        .any(|node| node.get("Index Name").and_then(Value::as_str) == Some(name))
}

pub fn scans_results(plan: &Value) -> bool {
    nodes(plan).iter().any(|node| {
        node.get("Relation Name").and_then(Value::as_str) == Some("results")
            && node.get("Node Type").and_then(Value::as_str) == Some("Seq Scan")
    })
}

pub fn heap_blocks(plan: &Value) -> u64 {
    let root = &plan[0]["Plan"];
    root["Shared Hit Blocks"].as_u64().unwrap_or(0)
        + root["Shared Read Blocks"].as_u64().unwrap_or(0)
}

pub fn elapsed(plan: &Value) -> Result<f64> {
    use anyhow::Context;
    plan[0]["Execution Time"]
        .as_f64()
        .context("EXPLAIN execution time")
}
