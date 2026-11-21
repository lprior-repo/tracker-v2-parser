#[allow(dead_code)]
mod support;

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{Database, EXPORT_MAX_OBSERVATIONS, ExportLimitExceeded},
    model::Source,
};

async fn seed(database: &Database, owned: i32, reviewed: i32) -> Result<(i64, i64)> {
    let (athlete, profile, saved_owned, saved_reviewed): (i64, i64, i64, i64) =
        sqlx::query_as(include_str!("export_count_guard/seed.sql"))
            .bind("count-guard")
            .bind(owned)
            .bind(reviewed)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(saved_owned, i64::from(owned));
    assert_eq!(saved_reviewed, i64::from(reviewed));
    Ok((athlete, profile))
}

async fn assert_export_count(database: &Database, athlete: i64, expected: i64) -> Result<()> {
    let guards: Vec<(i64, String, i64)> = sqlx::query_as(include_str!(
        "../src/database/export_snapshot_count_guards.sql"
    ))
    .bind([athlete].as_slice())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        guards,
        vec![
            (
                athlete,
                "athlete observations".to_owned(),
                expected.min(EXPORT_MAX_OBSERVATIONS + 1),
            ),
            (athlete, "athlete profiles".to_owned(), 1),
        ]
    );
    let mut snapshot = database
        .begin_export_snapshot(&["MI".to_owned()], &[2028])
        .await?;
    let outcome = snapshot.next_records(1).await;
    if expected > EXPORT_MAX_OBSERVATIONS {
        let error = outcome.err().context("oversized athlete must fail")?;
        let limit = error
            .downcast_ref::<ExportLimitExceeded>()
            .context("typed observation count limit")?;
        assert_eq!(limit.kind, "athlete observations");
        assert_eq!(limit.id, athlete);
        assert_eq!(limit.actual, EXPORT_MAX_OBSERVATIONS + 1);
        assert_eq!(limit.limit, EXPORT_MAX_OBSERVATIONS);
    } else {
        let records = outcome?.context("one exported athlete")?;
        assert_eq!(records.len(), 1);
        let record = records.first().context("exported athlete")?;
        assert_eq!(record.id, athlete);
        assert_eq!(record.performances.len(), usize::try_from(expected)?);
        let identities: BTreeSet<_> = record
            .performances
            .iter()
            .map(|row| {
                assert_eq!(row.source, Source::MileSplit);
                row.performance.result_id.as_deref()
            })
            .collect();
        assert!(!identities.contains(&None));
        assert_eq!(identities.len(), usize::try_from(expected)?);
        assert!(snapshot.next_records(1).await?.is_none());
    }
    snapshot.finish().await?;
    Ok(())
}

async fn boundary_scenario(owned: i32, reviewed: i32) -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let (athlete, _) = seed(&database, owned, reviewed).await?;
            let valid_links: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM valid_reviewed_result_links WHERE target_athlete_id=$1",
            )
            .bind(athlete)
            .fetch_one(database.pool())
            .await?;
            assert_eq!(valid_links, i64::from(reviewed));
            assert_export_count(&database, athlete, i64::from(owned) + i64::from(reviewed)).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_owned_results_around_limit_when_exporting_then_count_and_limit_are_exact()
-> Result<()> {
    for owned in [0, 9_999, 10_000, 10_001] {
        boundary_scenario(owned, 0).await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_mixed_ownership_around_limit_when_exporting_then_both_sources_count_once()
-> Result<()> {
    for owned in [9_998, 9_999, 10_000] {
        boundary_scenario(owned, 1).await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_only_reviewed_results_when_exporting_then_the_second_branch_is_counted() -> Result<()>
{
    boundary_scenario(0, 3).await
}

#[tokio::test]
async fn given_reviewed_result_receives_physical_owner_when_exporting_then_it_is_not_double_counted()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let (athlete, profile) = seed(&database, 1, 1).await?;
            assert_export_count(&database, athlete, 2).await?;
            sqlx::query(
                "UPDATE results r SET athlete_id=$1,athlete_source_id=$2, \
                 raw=jsonb_build_object('athlete_id','count-guard','event',r.event,'mark',r.raw_mark, \
                 'sport',r.sport,'result_id',r.source_result_id,'is_personal_record',false,'is_relay',false) \
                 WHERE r.source_result_key IN (SELECT source_result_key FROM reviewed_result_links)",
            )
            .bind(athlete)
            .bind(profile)
            .execute(database.pool())
            .await?;
            let invalid: Vec<String> = sqlx::query_scalar(
                "SELECT invalidation_reason FROM invalid_reviewed_result_links",
            )
            .fetch_all(database.pool())
            .await?;
            assert_eq!(invalid, ["physical_owner_present"]);
            assert_export_count(&database, athlete, 2).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}
