use anyhow::{Context, Result};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

use crate::{
    durable::pacing::test_support::with_database,
    model::{Performance, Source},
};

pub(super) const LOCATION: &str = "https://example.invalid/bounded-result-proof";

pub(super) fn rows(count: usize, padding: usize) -> Result<Vec<Performance>> {
    (0..count)
        .map(|index| {
            Ok(serde_json::from_value(json!({
                "athlete_id":"123", "event":"100m", "mark":"11.20",
                "reported_status":null,"sport":"outdoor", "result_id":format!("batch-proof-{index}"),
                "source_row":null,"school_id":"school-1","meet_id":"proof-meet",
                "meet_name":"Proof", "date":"2026-05-01","wind":1.2,"timing":"FAT",
                "course":null,"is_personal_record":true,"is_relay":false,"season":2026,
                "grade":"11","gender":"M","source_values":{"padding":"x".repeat(padding),
                    "eventType":{"unit":"time"},"context":{"season":2026},
                    "row":{"attributes":{"place":"1","empty":""}}}
            }))?)
        })
        .collect()
}

pub(super) async fn seed(pool: &PgPool, source: Source) -> Result<()> {
    sqlx::query("INSERT INTO schools(id,source,source_id,name,source_url) OVERRIDING SYSTEM VALUE VALUES(1,$1,'school-1','Proof school',$2)")
        .bind(source.as_str()).bind(LOCATION).execute(pool).await?;
    sqlx::query(
        "INSERT INTO athletes(id,name) OVERRIDING SYSTEM VALUE VALUES(1,'First'),(2,'Second')",
    )
    .execute(pool)
    .await?;
    sqlx::query("INSERT INTO athlete_sources(id,athlete_id,source,source_id,name) OVERRIDING SYSTEM VALUE VALUES(1,1,$1,'123','First'),(2,2,$1,'456','Second')")
        .bind(source.as_str()).execute(pool).await?;
    Ok(())
}

pub(super) async fn write(
    connection: &mut PgConnection,
    source: Source,
    performances: &[Performance],
    serial: bool,
) -> Result<()> {
    if !serial {
        return super::super::save_all(connection, source, performances, LOCATION).await;
    }
    super::super::legacy_history::reconcile(connection, source, performances, LOCATION).await?;
    for (chunk, rows) in performances.chunks(128).enumerate() {
        super::super::save_chunk_serial_for_test(connection, source, rows, LOCATION, chunk * 128)
            .await?;
    }
    Ok(())
}

pub(super) async fn snapshot(connection: &mut PgConnection) -> Result<Vec<Value>> {
    Ok(sqlx::query_scalar(
        "SELECT to_jsonb(r)-'id'-'last_seen' FROM results r ORDER BY source_result_key",
    )
    .fetch_all(connection)
    .await?)
}

async fn state(
    pool: &PgPool,
    source: Source,
    initial: &[Performance],
    incoming: &[Performance],
    serial: bool,
) -> Result<Vec<Value>> {
    let mut transaction = pool.begin().await?;
    write(&mut transaction, source, initial, true).await?;
    write(&mut transaction, source, incoming, serial).await?;
    let result = snapshot(&mut transaction).await?;
    transaction.rollback().await?;
    Ok(result)
}

#[tokio::test]
async fn given_bounded_results_when_batched_then_insert_and_nested_replay_match_serial()
-> Result<()> {
    for source in [Source::AthleticNet, Source::MileSplit] {
        with_database(|pool| Box::pin(async move {
            seed(pool, source).await?;
            for count in [1,7,8,32,128,129] {
                let initial=rows(count,32)?;
                assert_eq!(state(pool,source,&[],&initial,true).await?,state(pool,source,&[],&initial,false).await?);
                let mut corrected=initial.clone();
                for (index,row) in corrected.iter_mut().enumerate() {
                    row.mark=if index%3==0 {"DNF"} else {"10.90"}.into();
                    row.is_relay=index%5==0;
                    row.source_values=Some(json!({"row":{"attributes":{"place":"2","empty":null}},"context":{"gender":"M"}}));
                }
                assert_eq!(state(pool,source,&initial,&corrected,true).await?,state(pool,source,&initial,&corrected,false).await?);
            }
            Ok(())
        })).await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_duplicate_sparse_or_large_rows_when_saved_then_serial_semantics_are_preserved()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            seed(pool, Source::AthleticNet).await?;
            let initial = rows(8, 8)?;
            let mut duplicate = initial.clone();
            duplicate[7].result_id = duplicate[0].result_id.clone();
            duplicate[7].mark = "10.50".into();
            let mut sparse = initial.clone();
            sparse[2].event.clear();
            sparse[3].mark.clear();
            let mut large = initial.clone();
            large[4].source_values = Some(json!({"padding":"y".repeat(1_048_576)}));
            for incoming in [&duplicate, &sparse, &large] {
                assert_eq!(
                    state(pool, Source::AthleticNet, &initial, incoming, true).await?,
                    state(pool, Source::AthleticNet, &initial, incoming, false).await?
                );
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_cross_athlete_conflict_when_batch_rejected_then_outer_transaction_rolls_back()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            seed(pool, Source::AthleticNet).await?;
            let initial = rows(8, 8)?;
            let mut seed_tx = pool.begin().await?;
            write(&mut seed_tx, Source::AthleticNet, &initial, true).await?;
            seed_tx.commit().await?;
            let mut check = pool.acquire().await?;
            let before = snapshot(&mut check).await?;
            drop(check);
            let mut incoming = rows(16, 8)?;
            incoming[4].athlete_id = "456".into();
            for serial in [true, false] {
                let mut transaction = pool.begin().await?;
                let error = write(&mut transaction, Source::AthleticNet, &incoming, serial)
                    .await
                    .expect_err("identity collision rejected");
                assert!(
                    error
                        .to_string()
                        .contains("source result ID belongs to a different athlete"),
                    "{error:#}"
                );
                transaction.rollback().await?;
                let mut connection = pool.acquire().await?;
                assert_eq!(before, snapshot(&mut connection).await?);
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_missing_reference_before_invalid_wind_when_falling_back_then_first_error_matches()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            seed(pool, Source::AthleticNet).await?;
            let mut incoming = rows(8, 8)?;
            incoming[2].athlete_id = "missing-profile".into();
            incoming[5].wind = Some(f64::NAN);
            let mut errors = Vec::new();
            for serial in [true, false] {
                let mut transaction = pool.begin().await?;
                let error = write(&mut transaction, Source::AthleticNet, &incoming, serial)
                    .await
                    .expect_err("invalid result rejected");
                errors.push(error.to_string());
                transaction.rollback().await?;
            }
            assert_eq!(
                errors.first().context("serial error")?,
                errors.get(1).context("batch error")?
            );
            assert!(!errors[0].contains("non-finite wind"));
            Ok(())
        })
    })
    .await
}
