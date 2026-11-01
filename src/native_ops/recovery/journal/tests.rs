use super::*;

fn run(index: u64, name: String) -> Value {
    json!({"index":index,"name":name,"entry_type":"Command: Run","entry_json":{}})
}

#[test]
fn journal_oracle_requires_exact_once_selection_and_eager_ordered_children() -> Result<()> {
    let ids = vec!["inv_first".to_owned(), "inv_second".to_owned()];
    let mut rows = vec![
        run(0, "select-page-000000".into()),
        run(1, "reserve-page-000000".into()),
        json!({"index":2,"entry_type":"Command: OneWayCall","entry_json":{"target":"AthleticPage","invocation_id":"inv_first"}}),
        run(3, "select-page-000001".into()),
        run(4, "reserve-page-000001".into()),
        json!({"index":5,"entry_type":"Command: OneWayCall","entry_json":{"target":"AthleticPage","invocation_id":"inv_second"}}),
        json!({"index":6,"entry_type":"Command: AttachInvocation","entry_json":{"invocation_id":"inv_first"}}),
        json!({"index":7,"entry_type":"Command: AttachInvocation","entry_json":{"invocation_id":"inv_second"}}),
    ];
    assert!(validate_root(&rows, &ids, 2).is_ok());
    rows.get_mut(6)
        .context("fixture missing")?
        .as_object_mut()
        .context("fixture object")?
        .insert("index".into(), json!(4));
    assert!(validate_root(&rows, &ids, 2).is_err());
    rows.get_mut(6)
        .context("fixture missing")?
        .as_object_mut()
        .context("fixture object")?
        .insert("index".into(), json!(6));
    rows.push(run(8, "select-page-000000".into()));
    assert!(validate_root(&rows, &ids, 2).is_err());
    Ok(())
}

fn rolling_rows() -> (Vec<Value>, Vec<String>) {
    let ids = [
        "inv_first",
        "inv_second",
        "inv_third",
        "inv_fourth",
        "inv_fifth",
    ]
    .map(str::to_owned)
    .to_vec();
    let mut rows = Vec::new();
    let mut push = |entry_type: &str, body: Value, name: Option<String>| {
        rows.push(
            json!({"index":rows.len(),"entry_type":entry_type,"entry_json":body,"name":name}),
        );
    };
    for (child, settle) in [
        (0, None),
        (1, None),
        (2, Some(1)),
        (3, Some(2)),
        (4, Some(3)),
    ] {
        push(
            "Command: Run",
            json!({}),
            Some(format!("select-page-{child:06}")),
        );
        push(
            "Command: Run",
            json!({}),
            Some(format!("reserve-page-{child:06}")),
        );
        push(
            "Command: OneWayCall",
            json!({"target":"AthleticPage","invocation_id":ids[child]}),
            None,
        );
        push(
            "Command: AttachInvocation",
            json!({"Command":{"AttachInvocation":{"target":{"InvocationId":ids[child]},"completion_id":child}}}),
            None,
        );
        if let Some(child) = settle {
            push(
                "Notification: Completion",
                json!({"Notification":{"Completion":{"AttachInvocation":{"completion_id":child}}}}),
                None,
            );
        }
    }
    for child in [4, 0] {
        push(
            "Notification: Completion",
            json!({"Notification":{"Completion":{"AttachInvocation":{"completion_id":child}}}}),
            None,
        );
    }
    (rows, ids)
}

#[test]
fn rolling_oracle_accepts_fast_sibling_refill_and_rejects_lost_debt() -> Result<()> {
    let (rows, ids) = rolling_rows();
    let proof = validate_versioned_root(&rows, &ids, 5, true)?;
    assert_eq!(proof.get("maximum_uncompleted_children"), Some(&json!(3)));
    assert!(validate_versioned_root(&rows, &ids, 5, false).is_err());
    let mut missing = rows.clone();
    missing.pop();
    assert!(validate_versioned_root(&missing, &ids, 5, true).is_err());
    let mut duplicated = rows.clone();
    let mut copy = rows.last().context("last fixture absent")?.clone();
    copy["index"] = json!(rows.len());
    duplicated.push(copy);
    assert!(validate_versioned_root(&duplicated, &ids, 5, true).is_err());
    let overflow = rows
        .iter()
        .filter(|row| {
            row.get("entry_type").and_then(Value::as_str) != Some("Notification: Completion")
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(validate_versioned_root(&overflow, &ids, 5, true).is_err());
    Ok(())
}
