mod support;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use track_scrapper_v2::{database::Database, scope::LOWER_48};

const BASELINE: &str = include_str!("public_discovery_selection_parity/baseline.sql");
const CURRENT: &str = include_str!("../src/database/queue/selection_scoped.sql");
const FLAT: &str = include_str!("../src/database/queue/selection.sql");
const SEED: &str = include_str!("profile_coverage_selection_parity/seed.sql");

struct Scope<'a> {
    source: Option<&'a str>,
    states: &'a [String],
    years: &'a [i32],
    preferred: &'a str,
    generation: Option<&'a str>,
    owned: &'a [i64],
}

fn ordered(query: &str) -> Result<String> {
    let (prefix, suffix) = query
        .trim_end()
        .rsplit_once(" LIMIT 1")
        .context("selection bound absent")?;
    ensure!(suffix.is_empty(), "unexpected selection suffix");
    Ok(format!("{prefix} LIMIT 10000"))
}

async fn selected(
    tx: &mut Transaction<'_, Postgres>,
    query: &str,
    scope: &Scope<'_>,
) -> Result<Vec<(i64, Value)>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(ordered(query)?))
        .bind(scope.source)
        .bind(3_i32)
        .bind(scope.preferred)
        .bind(scope.states)
        .bind(scope.years)
        .bind(scope.generation)
        .bind(scope.generation.map(|_| 1000_i64))
        .bind(scope.owned)
        .bind(true)
        .bind(LOWER_48.as_slice())
        .fetch_all(&mut **tx)
        .await?)
}

async fn compare(pool: &PgPool, scope: &Scope<'_>) -> Result<usize> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    let expected = selected(&mut tx, BASELINE, scope).await?;
    let actual = selected(&mut tx, CURRENT, scope).await?;
    let flat = selected(&mut tx, FLAT, scope).await?;
    assert_eq!(
        flat, expected,
        "flat SQL oracle differs from frozen scoped query"
    );
    assert_eq!(
        actual, expected,
        "state {:?}, source {:?}, years {:?}, owned {:?}, preference {}",
        scope.states, scope.source, scope.years, scope.owned, scope.preferred
    );
    assert!(actual.len() < 10000);
    tx.rollback().await?;
    Ok(actual.len())
}

async fn seed(pool: &PgPool) -> Result<Vec<i64>> {
    sqlx::raw_sql(SEED).execute(pool).await?;
    sqlx::raw_sql(include_str!("public_discovery_selection_parity/seed.sql"))
        .execute(pool)
        .await?;
    Ok(sqlx::query_scalar(
        "SELECT page_id FROM durable_page_operations WHERE state='active' ORDER BY page_id LIMIT 3",
    )
    .fetch_all(pool)
    .await?)
}

#[tokio::test]
async fn given_mixed_public_targets_when_all_ordered_candidates_are_selected_then_scope_policy_and_order_match_the_frozen_query()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let owned = seed(&test.pool).await?;
            assert_eq!(owned.len(), 3);
            let mut count = 0;
            for states in [
                vec![],
                vec!["MI".into()],
                vec!["WI".into()],
                vec!["IA".into()],
                vec!["MI".into(), "WI".into()],
            ] {
                for years in [vec![], vec![2027], vec![2028]] {
                    for preference in ["profile", "bulk", "roster", "contacts"] {
                        for source in [Some("milesplit"), None] {
                            for owned in [&[][..], &owned[..1], &owned[..]] {
                                count += compare(
                                    &test.pool,
                                    &Scope {
                                        source,
                                        states: &states,
                                        years: &years,
                                        preferred: preference,
                                        generation: Some("parity-generation"),
                                        owned,
                                    },
                                )
                                .await?;
                            }
                        }
                    }
                }
            }
            assert!(count > 100);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_committed_registration_and_retry_changes_when_selection_repeats_then_no_stale_candidate_or_reset_ledger_is_used()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let owned = seed(&test.pool).await?;
        let states=vec!["IA".into()];
        let scope=Scope {source:Some("milesplit"),states:&states,years:&[2027],preferred:"bulk",generation:Some("parity-generation"),owned:&owned};
        assert!(compare(&test.pool,&scope).await?>0);
        sqlx::query("UPDATE durable_page_operations SET state='deferred',retry_at_ms=2000 WHERE state='active'").execute(&test.pool).await?;
        compare(&test.pool,&scope).await?;
        sqlx::query("UPDATE durable_page_operations SET state='exhausted' WHERE state='deferred'").execute(&test.pool).await?;
        compare(&test.pool,&scope).await?;
        sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE cursor->>'kind'='roster'").execute(&test.pool).await?;
        compare(&test.pool,&scope).await?;
        sqlx::query("UPDATE athlete_sources SET graduation_year=2028").execute(&test.pool).await?;
        compare(&test.pool,&scope).await?;
        database.close().await;
        Ok(())
    })).await
}
