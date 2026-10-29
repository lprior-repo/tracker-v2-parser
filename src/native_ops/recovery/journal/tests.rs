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
