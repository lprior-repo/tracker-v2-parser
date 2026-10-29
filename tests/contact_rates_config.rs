use anyhow::Result;
use track_scrapper_v2::config::{Config, Limits};

#[test]
fn explicit_contact_rate_and_three_durable_slots_are_bounded() -> Result<()> {
    let config = Config::parse(
        "[limits]\nrequests_per_second=3.0\ncontact_requests_per_second=3.0\nconcurrency=3",
    )?;
    assert_eq!(config.limits.contact_requests_per_second, Some(3.0));
    assert_eq!(config.limits.concurrency, 3);
    for concurrency in [1, 2, 3] {
        assert!(Config::parse(&format!("[limits]\nconcurrency={concurrency}")).is_ok());
    }
    for input in [
        "contact_requests_per_second=nan",
        "contact_requests_per_second=inf",
        "contact_requests_per_second=-inf",
        "contact_requests_per_second=0.0",
        "contact_requests_per_second=-1.0",
        "contact_requests_per_second=3.0001",
        "contact_request_per_second=3.0",
        "concurrency=0",
        "concurrency=4",
    ] {
        assert!(
            Config::parse(&format!("[limits]\n{input}")).is_err(),
            "{input}"
        );
    }
    Ok(())
}

#[test]
fn omitted_contact_rate_preserves_the_exact_preexisting_limits_serialization() -> Result<()> {
    let legacy = r#"{"requests_per_second":1.0,"max_pages":100,"max_attempts":3,"concurrency":1}"#;
    assert_eq!(serde_json::to_string(&Limits::default())?, legacy);
    let decoded: Limits = serde_json::from_str(legacy)?;
    assert_eq!(decoded.contact_requests_per_second, None);
    assert_eq!(serde_json::to_string(&decoded)?, legacy);
    assert_eq!(Config::default().limits.concurrency, 1);
    Ok(())
}

#[tokio::test]
async fn legacy_live_scrape_rejects_parallel_config_before_opening_database() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let config = directory.path().join("parallel.toml");
    std::fs::write(&config, "[limits]\nconcurrency=3")?;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_track-scrapper-v2"))
        .args(["--database-url", "not-a-database-url", "scrape", "--config"])
        .arg(config)
        .current_dir(directory.path())
        .env_remove("DATABASE_URL")
        .env_remove("TEST_DATABASE_URL")
        .output()
        .await?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("use durable crawl"));
    Ok(())
}
