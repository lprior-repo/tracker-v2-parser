use anyhow::{Context, Result};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};

pub const ID_INDEX: &str = "results_milesplit_source_result_id_hash_idx";
pub const URL_INDEX: &str = "results_milesplit_source_url_hash_idx";
pub const MIGRATION: &str =
    include_str!("../../migrations/202611010002_result_reconciliation_lookup.sql");

pub fn raw(kind: &str) -> Value {
    json!({"source_values":{"kind":kind,"mask_reconciliation":"retained_mark_masked","revision_status":"unverified_prior_revision"}})
}

pub async fn insert(
    pool: &PgPool,
    key: &str,
    source: &str,
    identity: Option<&str>,
    url: &str,
    raw: Value,
) -> Result<()> {
    sqlx::query("INSERT INTO results(source_result_key,source,source_result_id,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw) VALUES(md5($1)||md5($1),$2,$3,'800m','2:00.00','outdoor','recorded',TRUE,FALSE,$4,$5)")
        .bind(key).bind(source).bind(identity).bind(url).bind(raw).execute(pool).await?;
    Ok(())
}

pub async fn drop_indexes(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql("DROP INDEX results_milesplit_source_result_id_hash_idx; DROP INDEX results_milesplit_source_url_hash_idx;")
        .execute(pool).await?;
    Ok(())
}

pub async fn install(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(MIGRATION).execute(pool).await?;
    Ok(())
}

pub async fn snapshot(pool: &PgPool) -> Result<Vec<Value>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(r) FROM results r ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn seed(pool: &PgPool) -> Result<()> {
    sqlx::query("INSERT INTO results(source_result_key,source,source_result_id,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw) SELECT md5(i::text)||md5(i::text),'milesplit','pr:'||i||':outdoor:all:800m','800m','2:00.00','outdoor','recorded',TRUE,FALSE,'https://mi.milesplit.com/athletes/'||i||'-runner',jsonb_build_object('source_values',jsonb_build_object('kind','personal_record','mask_reconciliation','retained_mark_masked','revision_status','unverified_prior_revision','payload',repeat(md5(i::text),8))) FROM generate_series(1,50000) i")
        .execute(pool).await?;
    sqlx::query("ANALYZE results").execute(pool).await?;
    Ok(())
}

pub async fn by_ids(pool: &PgPool, ids: &[String]) -> Result<Vec<(i64, Option<String>)>> {
    Ok(sqlx::query_as("SELECT id,source_result_id FROM results WHERE source='milesplit' AND source_result_id=ANY($1) AND raw#>>'{source_values,mask_reconciliation}'='retained_mark_masked' ORDER BY id")
        .bind(ids).fetch_all(pool).await?)
}

pub async fn by_url(pool: &PgPool, url: &str) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM results WHERE source='milesplit' AND source_url=$1 AND raw#>>'{source_values,kind}' IN ('career_result','personal_record') ORDER BY id LIMIT 10001")
        .bind(url).fetch_all(pool).await?)
}

async fn transaction(pool: &PgPool) -> Result<Transaction<'_, Postgres>> {
    let mut transaction = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

pub async fn explain_ids(pool: &PgPool, ids: &[String], generic: bool) -> Result<Value> {
    let mut transaction = transaction(pool).await?;
    let query = if generic {
        "EXPLAIN (GENERIC_PLAN,FORMAT JSON) UPDATE results SET raw=raw #- '{source_values,revision_status}' #- '{source_values,mask_reconciliation}' WHERE source='milesplit' AND source_result_id=ANY($1::text[]) AND raw#>>'{source_values,mask_reconciliation}'='retained_mark_masked'"
    } else {
        "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) UPDATE results SET raw=raw #- '{source_values,revision_status}' #- '{source_values,mask_reconciliation}' WHERE source='milesplit' AND source_result_id=ANY($1) AND raw#>>'{source_values,mask_reconciliation}'='retained_mark_masked'"
    };
    let plan = if generic {
        sqlx::raw_sql(query)
            .fetch_one(&mut *transaction)
            .await?
            .try_get(0)?
    } else {
        sqlx::query_scalar(query)
            .bind(ids)
            .fetch_one(&mut *transaction)
            .await?
    };
    transaction.rollback().await?;
    Ok(plan)
}

pub async fn explain_url(pool: &PgPool, url: &str) -> Result<Value> {
    let mut transaction = transaction(pool).await?;
    let plan = sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT r.id,r.source_url,r.raw FROM results r WHERE r.source='milesplit' AND r.source_url=$1 AND r.raw#>>'{source_values,kind}' IN ('career_result','personal_record') ORDER BY r.id LIMIT 10001 FOR UPDATE")
        .bind(url).fetch_one(&mut *transaction).await?;
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
    plan[0]["Execution Time"]
        .as_f64()
        .context("EXPLAIN execution time")
}

pub fn updated_rows(plan: &Value) -> f64 {
    nodes(plan)
        .iter()
        .filter(|node| {
            matches!(
                node.get("Node Type").and_then(Value::as_str),
                Some("Index Scan" | "Bitmap Heap Scan")
            )
        })
        .map(|node| node["Actual Rows"].as_f64().unwrap_or(0.0))
        .sum()
}

pub fn rechecked_rows(plan: &Value) -> f64 {
    nodes(plan)
        .iter()
        .map(|node| {
            node["Rows Removed by Index Recheck"]
                .as_f64()
                .unwrap_or(0.0)
        })
        .sum()
}
