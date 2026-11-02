use anyhow::Result;
use serde_json::Value;

use super::evidence;

pub(super) fn pending(rows: &[Value]) -> Result<bool> {
    let entries = rows
        .iter()
        .map(evidence::decode)
        .collect::<Result<Vec<_>>>()?;
    Ok(entries.iter().any(|entry| {
        let name = entry.pointer("/Command/Run/name").and_then(Value::as_str);
        let recognized = matches!(
            name,
            Some("bootstrap-discovery-and-contact-queue" | "select-page-000000")
        );
        let completion = entry
            .pointer("/Command/Run/completion_id")
            .and_then(Value::as_u64);
        recognized
            && completion.is_some_and(|completion| {
                !entries.iter().any(|entry| {
                    entry
                        .pointer("/Notification/Completion/Run/completion_id")
                        .and_then(Value::as_u64)
                        == Some(completion)
                })
            })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command(name: &str, completion: u64) -> Value {
        json!({"entry_json":{"Command":{"Run":{"name":name,"completion_id":completion}}}})
    }

    fn completion(id: u64) -> Value {
        json!({"entry_json":{"Notification":{"Completion":{"Run":{"completion_id":id}}}}})
    }

    #[test]
    fn given_prepared_and_legacy_workers_when_their_first_read_is_blocked_then_both_are_freezable()
    -> Result<()> {
        for name in [
            "bootstrap-discovery-and-contact-queue",
            "select-page-000000",
        ] {
            assert!(pending(&[command(name, 0)])?);
            assert!(!pending(&[command(name, 0), completion(0)])?);
        }
        Ok(())
    }

    #[test]
    fn given_interleaved_notifications_when_an_unrelated_run_finishes_then_the_barrier_stays_pending()
    -> Result<()> {
        let rows = [
            command("bootstrap-discovery-and-contact-queue", 3),
            command("unrelated", 4),
            completion(4),
        ];
        assert!(pending(&rows)?);
        assert!(!pending(&[rows.to_vec(), vec![completion(3)]].concat())?);
        Ok(())
    }

    #[test]
    fn given_other_commands_or_absent_identity_when_freezing_then_no_barrier_is_claimed()
    -> Result<()> {
        assert!(!pending(&[command("commit-page-and-receipt", 0)])?);
        assert!(!pending(&[
            json!({"entry_json":{"Command":{"Run":{"name":"select-page-000000"}}}})
        ])?);
        assert!(!pending(&[])?);
        assert!(pending(&[json!({"entry_json":"invalid-json"})]).is_err());
        Ok(())
    }
}
