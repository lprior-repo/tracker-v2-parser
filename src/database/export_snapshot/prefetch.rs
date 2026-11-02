use std::collections::VecDeque;

use anyhow::{Context, Result, ensure};
use sqlx::{Postgres, Transaction};

use super::{EXPORT_MAX_OBSERVATIONS, ExportLimitExceeded};

pub(super) const EXPORT_ID_PREFETCH_ROWS: usize = 128;
pub(super) type PendingAthlete = (i64, Result<(), anyhow::Error>);
type CountGuardRow = (i64, String, i64);

fn validate_keyset(ids: &[i64], cursor: i64) -> Result<()> {
    ensure!(
        ids.len() <= EXPORT_ID_PREFETCH_ROWS
            && ids.iter().all(|id| *id > cursor)
            && ids
                .windows(2)
                .all(|pair| matches!(pair, [left, right] if left < right)),
        "export athlete keyset must be bounded and strictly ascending"
    );
    Ok(())
}

fn validate_count_guard_rows(
    ids: &[i64],
    rows: Vec<CountGuardRow>,
) -> Result<VecDeque<PendingAthlete>> {
    let grouped = rows.into_iter().fold(
        Vec::<(i64, Vec<CountGuardRow>)>::new(),
        |mut groups, row| {
            if groups.last().is_some_and(|(id, _)| *id == row.0) {
                if let Some((_, items)) = groups.last_mut() {
                    items.push(row);
                }
            } else {
                groups.push((row.0, vec![row]));
            }
            groups
        },
    );
    ensure!(
        grouped.len() == ids.len(),
        "export guards must cover each selected athlete"
    );
    ids.iter()
        .zip(grouped)
        .map(|(id, (guard_id, guards))| {
            ensure!(
                *id == guard_id,
                "export guard IDs must match selected athlete IDs"
            );
            ensure!(
                guards.len() == 2
                    && guards
                        .first()
                        .is_some_and(|(_, kind, _)| kind == "athlete observations")
                    && guards
                        .get(1)
                        .is_some_and(|(_, kind, _)| kind == "athlete profiles"),
                "export guards must include observations and profiles in order"
            );
            let result = guards.into_iter().try_for_each(|(_, kind, count)| {
                ensure!(
                    count <= EXPORT_MAX_OBSERVATIONS,
                    ExportLimitExceeded {
                        kind: if kind == "athlete observations" {
                            "athlete observations"
                        } else {
                            "athlete profiles"
                        },
                        id: *id,
                        actual: count,
                        limit: EXPORT_MAX_OBSERVATIONS
                    }
                );
                Ok::<_, anyhow::Error>(())
            });
            Ok((*id, result))
        })
        .collect::<Result<VecDeque<_>>>()
}

#[tracing::instrument(skip(transaction, states, years))]
pub(super) async fn read_batch(
    transaction: &mut Transaction<'_, Postgres>,
    states: &[String],
    years: &[i32],
    cursor: i64,
) -> Result<(Vec<i64>, VecDeque<PendingAthlete>)> {
    let ids: Vec<i64> = sqlx::query_scalar(include_str!("../export_snapshot_scope.sql"))
        .bind(states)
        .bind(years)
        .bind(cursor)
        .bind(i64::try_from(EXPORT_ID_PREFETCH_ROWS)?)
        .fetch_all(&mut **transaction)
        .await
        .context("loading bounded export athlete keyset")?;
    if ids.is_empty() {
        return Ok((ids, VecDeque::new()));
    }
    validate_keyset(&ids, cursor)?;
    let rows: Vec<CountGuardRow> =
        sqlx::query_as(include_str!("../export_snapshot_count_guards.sql"))
            .bind(&ids)
            .fetch_all(&mut **transaction)
            .await
            .context("checking bounded export athlete counts")?;
    let guards = validate_count_guard_rows(&ids, rows)?;
    Ok((ids, guards))
}

#[cfg(test)]
mod tests {
    use super::{EXPORT_ID_PREFETCH_ROWS, validate_count_guard_rows, validate_keyset};
    use crate::database::ExportLimitExceeded;

    #[test]
    fn given_batched_count_guards_when_a_later_athlete_exceeds_limit_then_error_is_deferred() {
        let rows = vec![
            (10, "athlete observations".to_owned(), 0),
            (10, "athlete profiles".to_owned(), 0),
            (20, "athlete observations".to_owned(), 10_001),
            (20, "athlete profiles".to_owned(), 0),
        ];
        let mut guards = validate_count_guard_rows(&[10, 20], rows)
            .expect("count response should cover selected athletes");
        let (first_id, first_guard) = guards.pop_front().expect("first athlete guard");
        assert_eq!(first_id, 10);
        assert!(first_guard.is_ok());
        let (second_id, second_guard) = guards.pop_front().expect("second athlete guard");
        assert_eq!(second_id, 20);
        let error = second_guard.expect_err("second athlete exceeds the count limit");
        let limit = error
            .downcast_ref::<ExportLimitExceeded>()
            .expect("typed observation limit");
        assert_eq!(limit.kind, "athlete observations");
        assert_eq!(limit.id, 20);
        assert_eq!(limit.actual, 10_001);
        assert_eq!(limit.limit, 10_000);
    }

    #[test]
    fn given_guard_rows_when_an_athlete_is_missing_or_duplicated_then_prefetch_fails_closed() {
        let missing_profile = vec![(10, "athlete observations".to_owned(), 0)];
        assert!(validate_count_guard_rows(&[10], missing_profile).is_err());
        let duplicate_profile = vec![
            (10, "athlete observations".to_owned(), 0),
            (10, "athlete profiles".to_owned(), 0),
            (10, "athlete profiles".to_owned(), 0),
        ];
        assert!(validate_count_guard_rows(&[10], duplicate_profile).is_err());
    }

    #[test]
    fn given_prefetch_keyset_when_ids_are_out_of_order_or_unbounded_then_it_is_rejected() {
        assert!(validate_keyset(&[11, 12, 13], 10).is_ok());
        assert!(validate_keyset(&[11, 13, 12], 10).is_err());
        assert!(validate_keyset(&[10], 10).is_err());
        let too_many = vec![1; EXPORT_ID_PREFETCH_ROWS + 1];
        assert!(validate_keyset(&too_many, 0).is_err());
    }
}
