mod support;

use anyhow::Result;
use support::{assert_success, import_bio, run_cli, with_schema};
use tempfile::tempdir;

#[tokio::test]
async fn given_persisted_results_when_replay_restarts_then_migrations_preserve_original_marks()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        assert_success(&import_bio(test).await?);
        let before: Vec<(String, String)> = sqlx::query_as("SELECT event, raw_mark FROM results ORDER BY event, raw_mark")
            .fetch_all(&test.pool).await?;
        assert_eq!(before.len(), 53);
        assert_success(&import_bio(test).await?);
        let after: Vec<(String, String)> = sqlx::query_as("SELECT event, raw_mark FROM results ORDER BY event, raw_mark")
            .fetch_all(&test.pool).await?;
        assert_eq!(after, before);
        assert!(after.contains(&("200 Meters".to_owned(), "23.33a".to_owned())));
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT tablename::text FROM pg_tables WHERE schemaname=current_schema() AND tablename <> '_sqlx_migrations' ORDER BY tablename")
            .fetch_all(&test.pool).await?;
        assert_eq!(tables, vec!["athlete_sources", "athletes", "contacts", "durable_operation_receipts", "durable_page_attempts", "durable_page_operations", "durable_page_receipts", "durable_wire_pacing", "durable_wire_requests", "results", "schools", "scrape_pages"]);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_no_arguments_when_started_then_help_does_not_connect_to_database() -> Result<()> {
    let directory = tempdir()?;
    let output = run_cli(
        directory.path(),
        Some("postgres://unavailable:unavailable@127.0.0.1:1/absent"),
        &[],
    )
    .await?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("scrape"));
    assert!(text.contains("export"));
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn given_removed_commands_when_invoked_then_cli_rejects_each_without_database_access()
-> Result<()> {
    let directory = tempdir()?;
    for obsolete in ["db-check", "status", "import"] {
        let output = run_cli(directory.path(), None, &[obsolete]).await?;
        assert!(!output.status.success(), "obsolete command {obsolete}");
        assert!(!output.stderr.is_empty());
    }
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn given_unsupported_scheme_when_scrape_starts_then_failure_does_not_create_a_local_store()
-> Result<()> {
    let directory = tempdir()?;
    let input = support::fixture("athleticnet/bio-28872883.json");
    let output = run_cli(
        directory.path(),
        Some("https://localhost/database"),
        &[
            "scrape",
            "--input",
            input
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("UTF-8 input path"))?,
            "--source",
            "athleticnet",
            "--kind",
            "athlete",
            "--url",
            "https://www.athletic.net/athlete/28872883",
        ],
    )
    .await?;
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn given_closed_endpoint_when_scrape_starts_then_connection_failure_is_visible() -> Result<()>
{
    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.bind("127.0.0.1:0".parse()?)?;
    let directory = tempdir()?;
    let url = format!(
        "postgres://unavailable:unavailable@127.0.0.1:{}/absent?connect_timeout=1",
        socket.local_addr()?.port()
    );
    let input = support::fixture("athleticnet/bio-28872883.json");
    let output = run_cli(
        directory.path(),
        Some(&url),
        &[
            "scrape",
            "--input",
            input
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("UTF-8 input path"))?,
            "--source",
            "athleticnet",
            "--kind",
            "athlete",
            "--url",
            "https://www.athletic.net/athlete/28872883",
        ],
    )
    .await?;
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn given_out_of_contract_configuration_when_scrape_starts_then_limits_and_unknown_fields_are_rejected()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let config = test.directory().join("invalid-config.toml");
        support::write_config(&config)?;
        let valid = std::fs::read_to_string(&config)?;
        let mutations = [
            valid.replace("max_attempts = 3","max_attempts = 0"),
            valid.replace("max_attempts = 3","max_attempts = 4"),
            valid.replace("concurrency = 1","concurrency = 2"),
            valid.replace("requests_per_second = 1.0","requests_per_second = 0.0"),
            valid.replace("requests_per_second = 1.0","requests_per_second = 3.0001"),
            format!("unknown_configuration = true\n{valid}"),
        ];
        let input = support::fixture("athleticnet/bio-28872883.json");
        for mutation in mutations {
            std::fs::write(&config,mutation)?;
            let output = test.cli(&["scrape","--config",config.to_str().ok_or_else(|| anyhow::anyhow!("UTF-8 config path"))?,
                "--input",input.to_str().ok_or_else(|| anyhow::anyhow!("UTF-8 input path"))?,
                "--source","athleticnet","--kind","athlete","--url",
                "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0",
                "--athlete-id","28872883","--state","WI"]).await?;
            assert!(!output.status.success());
            assert!(!output.stderr.is_empty());
        }
        assert_success(&import_bio(test).await?);
        let results: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(results,53);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_restored_carriage_return_path_when_replayed_then_evidence_uses_generated_dotenv()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let directory = test.directory().join("restored\rdata");
        std::fs::create_dir(&directory)?;
        let evidence = directory.join("evidence");
        let scripts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/migration");
        let generated = tokio::process::Command::new("python3")
            .args(["-c", "import sys; sys.path.insert(0, sys.argv[1]); from restore import dotenv_value; print('TRACK_SCRAPPER_EVIDENCE_DIR=' + dotenv_value(sys.argv[2]))"])
            .arg(scripts).arg(&evidence)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .output().await?;
        assert_success(&generated);
        std::fs::write(directory.join(".env"), generated.stdout)?;
        let config = directory.join("config.toml");
        support::write_config(&config)?;
        let input = support::fixture("milesplit/athlete-oh-15005189-current.html");
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_track-scrapper-v2"));
        let output = command
            .current_dir(&directory)
            .env_remove("DATABASE_URL").env_remove("TRACK_SCRAPPER_EVIDENCE_DIR")
            .args(["--database-url", &test.url, "scrape", "--config"])
            .arg(&config).arg("--input").arg(&input)
            .args(["--source", "milesplit", "--kind", "athlete", "--url",
                "https://oh.milesplit.com/athletes/15005189-brice-fuller",
                "--athlete-id", "15005189", "--state", "OH", "--sport", "outdoor"])
            .kill_on_drop(true).output();
        let output = tokio::time::timeout(std::time::Duration::from_secs(30), output).await??;
        assert_success(&output);
        let artifact: String = sqlx::query_scalar(
            "SELECT details#>>'{source_values,_capture,artifact_path}' FROM athlete_sources WHERE source='milesplit'",
        ).fetch_one(&test.pool).await?;
        assert_eq!(std::path::Path::new(&artifact).parent(), Some(evidence.as_path()));
        assert!(std::path::Path::new(&artifact).is_file());
        let results: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
            .fetch_one(&test.pool).await?;
        assert_eq!(results, 28);
        Ok(())
    })).await
}
