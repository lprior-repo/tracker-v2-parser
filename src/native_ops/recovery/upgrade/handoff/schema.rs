use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};

use super::super::super::runtime::Runtime;

pub(super) struct Snapshot {
    history: Vec<Value>,
    records: Vec<Value>,
}

pub(super) async fn capture(runtime: &Runtime) -> Result<Snapshot> {
    let history = sqlx::query_scalar(
        "SELECT to_jsonb(m) FROM _sqlx_migrations m ORDER BY version LIMIT 1001",
    )
    .fetch_all(&runtime.pool)
    .await?;
    ensure!(
        history.len() <= 1000,
        "owned migration history exceeds bound"
    );
    let records = stream::iter([
        "SELECT to_jsonb(t) FROM schools t ORDER BY id LIMIT 10001",
        "SELECT to_jsonb(t) FROM athletes t ORDER BY id LIMIT 10001",
        "SELECT to_jsonb(t) FROM athlete_sources t ORDER BY id LIMIT 10001",
        "SELECT to_jsonb(t) FROM results t ORDER BY id LIMIT 10001",
        "SELECT to_jsonb(t) FROM contacts t ORDER BY id LIMIT 10001",
        "SELECT to_jsonb(t) FROM scrape_pages t ORDER BY id LIMIT 10001",
    ])
    .map(Ok::<_, anyhow::Error>)
    .try_fold(Vec::new(), |mut tables, query| async move {
        let rows: Vec<Value> = sqlx::query_scalar(query).fetch_all(&runtime.pool).await?;
        ensure!(rows.len() <= 10000, "owned migration records exceed bound");
        tables.push(json!(rows));
        Ok(tables)
    })
    .await?;
    Ok(Snapshot { history, records })
}

pub(super) fn verify(before: &Snapshot, after: &Snapshot) -> Result<Value> {
    ensure!(
        before.records == after.records,
        "worker setup changed retained business records"
    );
    ensure!(
        after.history.get(..before.history.len()) == Some(before.history.as_slice()),
        "worker setup changed original migration history"
    );
    let appended = after
        .history
        .get(before.history.len()..)
        .context("migration history truncated")?;
    ensure!(
        appended
            .iter()
            .all(|row| row.get("success") == Some(&Value::Bool(true))),
        "worker setup appended failed migration"
    );
    Ok(json!({"original_migration_history_unchanged":true,
        "retained_business_records_unchanged":true,"migration_history_before":before.history,
        "migration_history_after":after.history,"appended_migrations":appended}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(history: Value, records: Value) -> Snapshot {
        Snapshot {
            history: vec![history],
            records: vec![records],
        }
    }

    #[test]
    fn accepts_successful_append_and_identical_retained_records() -> Result<()> {
        let old = json!({"version":1,"checksum":"retained","success":true});
        let before = snapshot(old.clone(), json!({"id":1,"mark":"12.34"}));
        let mut after = snapshot(old, json!({"id":1,"mark":"12.34"}));
        after
            .history
            .push(json!({"version":2,"checksum":"new","success":true}));
        assert_eq!(
            verify(&before, &after)?["appended_migrations"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        Ok(())
    }

    #[test]
    fn rejects_changed_history_changed_records_and_failed_append() {
        for (history, records, append) in [
            (
                json!({"version":1,"checksum":"changed","success":true}),
                json!(1),
                None,
            ),
            (
                json!({"version":1,"checksum":"retained","success":true}),
                json!(2),
                None,
            ),
            (
                json!({"version":1,"checksum":"retained","success":true}),
                json!(1),
                Some(json!({"version":2,"success":false})),
            ),
        ] {
            let before = snapshot(
                json!({"version":1,"checksum":"retained","success":true}),
                json!(1),
            );
            let mut after = snapshot(history, records);
            if let Some(row) = append {
                after.history.push(row);
            }
            assert!(verify(&before, &after).is_err());
        }
    }
}
