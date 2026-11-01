use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{entry, matching};

#[derive(Default)]
struct Debt {
    active: BTreeSet<String>,
    attached: BTreeMap<u64, String>,
    sent: Vec<String>,
    settled: BTreeSet<String>,
    maximum: usize,
    completed_ids: BTreeSet<u64>,
}

pub(super) fn validate(rows: &[Value], children: &[String], count: usize) -> Result<Value> {
    ensure!(children.len() == count, "rolling child count mismatch");
    let mut ordered = rows.to_vec();
    ordered.sort_by_key(|row| row.get("index").and_then(Value::as_u64));
    (0..count).try_for_each(|number| {
        ["select-page-", "reserve-page-"]
            .iter()
            .try_for_each(|prefix| {
                let expected = format!("{prefix}{number:06}");
                ensure!(
                    ordered
                        .iter()
                        .filter(|row| row.get("entry_type").and_then(Value::as_str)
                            == Some("Command: Run")
                            && row.get("name").and_then(Value::as_str) == Some(expected.as_str()))
                        .count()
                        == 1,
                    "rolling selection and reservation must be journaled exactly once"
                );
                Ok::<_, anyhow::Error>(())
            })
    })?;
    let debt = ordered.iter().try_fold(Debt::default(), |mut debt, row| {
        let decoded = entry(row)?;
        let kind = row
            .get("entry_type")
            .and_then(Value::as_str)
            .context("journal entry type absent")?;
        if kind.to_lowercase().contains("onewaycall")
            && serde_json::to_string(&decoded)?.contains("AthleticPage")
        {
            debt.send(row, children)?;
        } else if let Some(attach) = decoded.pointer("/Command/AttachInvocation") {
            debt.attach(attach, children)?;
        } else if let Some(completion) =
            decoded.pointer("/Notification/Completion/AttachInvocation")
        {
            debt.complete(completion)?;
        }
        Ok::<_, anyhow::Error>(debt)
    })?;
    ensure!(
        debt.sent == children
            && debt
                .settled
                .iter()
                .eq(children.iter().collect::<BTreeSet<_>>().into_iter())
            && debt.active.is_empty()
            && debt.attached.is_empty(),
        "rolling children were lost, duplicated, or left undrained"
    );
    Ok(
        json!({"scheduler_version":"rolling_v2","selected":count,"sent_child_ids":debt.sent,
        "settled_child_ids":debt.settled,"maximum_uncompleted_children":debt.maximum,
        "bounded_window":3,"exactly_once_select_reserve_send_attach_and_settle":true}),
    )
}

impl Debt {
    fn send(&mut self, row: &Value, children: &[String]) -> Result<()> {
        let ids = matching(row, children)?;
        ensure!(ids.len() == 1, "rolling send child identity is ambiguous");
        let id = ids.first().context("rolling send child absent")?;
        ensure!(
            self.active.insert(id.clone()) && !self.settled.contains(id),
            "rolling child was dispatched more than once"
        );
        self.maximum = self.maximum.max(self.active.len());
        ensure!(
            self.maximum <= 3,
            "rolling scheduler exceeded three uncompleted children"
        );
        self.sent.push(id.clone());
        Ok(())
    }

    fn attach(&mut self, value: &Value, children: &[String]) -> Result<()> {
        let id = value
            .pointer("/target/InvocationId")
            .and_then(Value::as_str)
            .context("rolling attach target absent")?;
        if !children.iter().any(|child| child == id) {
            return Ok(());
        }
        let completion = value
            .get("completion_id")
            .and_then(Value::as_u64)
            .context("rolling attach completion ID absent")?;
        ensure!(
            self.active.contains(id) && self.attached.insert(completion, id.to_owned()).is_none(),
            "rolling attach duplicated or no longer owned"
        );
        Ok(())
    }

    fn complete(&mut self, value: &Value) -> Result<()> {
        let completion = value
            .get("completion_id")
            .and_then(Value::as_u64)
            .context("rolling completion ID absent")?;
        ensure!(
            !self.completed_ids.contains(&completion),
            "rolling completion notification duplicated"
        );
        let Some(id) = self.attached.remove(&completion) else {
            return Ok(());
        };
        self.completed_ids.insert(completion);
        ensure!(
            self.active.remove(&id) && self.settled.insert(id),
            "rolling completion duplicated or child lost"
        );
        Ok(())
    }
}
