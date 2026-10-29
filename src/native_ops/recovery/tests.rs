use super::*;

#[test]
fn setup_failure_report_is_saved_with_accurate_cleanup_and_redacted_credentials() -> Result<()> {
    let root = tempfile::tempdir()?;
    let original = "postgres://private_user:private_password@localhost/private_db";
    let mut report = json!({"status":"failed"});
    let outcome = Err(anyhow::anyhow!(
        "authentication failed for private_user at {original}"
    ));
    assert!(finish_report(&mut report, outcome, root.path(), original, original, true).is_err());
    let bytes = fs::read(root.path().join("report.json"))?;
    let text = std::str::from_utf8(&bytes)?;
    assert!(!text.contains("private_user") && !text.contains("private_password"));
    let saved: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(saved.pointer("/cleanup/schema_removed"), Some(&json!(true)));
    assert_eq!(saved.get("status"), Some(&json!("failed")));
    Ok(())
}
