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
    assert!(text.contains("durable"));
    assert!(
        !text
            .lines()
            .any(|line| matches!(line.split_whitespace().next(), Some("scrape" | "export")))
    );
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn given_removed_commands_when_invoked_then_cli_rejects_each_without_database_access()
-> Result<()> {
    let directory = tempdir()?;
    for obsolete in ["db-check", "status", "import", "scrape", "export"] {
        let output = run_cli(directory.path(), None, &[obsolete]).await?;
        assert!(!output.status.success(), "obsolete command {obsolete}");
        assert!(!output.stderr.is_empty());
    }
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn given_unsupported_scheme_when_offline_replay_starts_then_failure_does_not_create_a_local_store()
-> Result<()> {
    let directory = tempdir()?;
    let operation = support::OfflineOperation::Replay {
        config: None,
        input: support::fixture("athleticnet/bio-28872883.json"),
        target: track_scrapper_v2::model::Target::get(
            track_scrapper_v2::model::Source::AthleticNet,
            track_scrapper_v2::model::PageKind::Athlete,
            "https://www.athletic.net/athlete/28872883".into(),
            Default::default(),
        ),
    };
    let output = support::run_operation(
        directory.path(),
        Some("https://localhost/database"),
        &operation,
    )
    .await?;
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn given_closed_endpoint_when_offline_replay_starts_then_connection_failure_is_visible()
-> Result<()> {
    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.bind("127.0.0.1:0".parse()?)?;
    let directory = tempdir()?;
    let url = format!(
        "postgres://unavailable:unavailable@127.0.0.1:{}/absent?connect_timeout=1",
        socket.local_addr()?.port()
    );
    let operation = support::OfflineOperation::Replay {
        config: None,
        input: support::fixture("athleticnet/bio-28872883.json"),
        target: track_scrapper_v2::model::Target::get(
            track_scrapper_v2::model::Source::AthleticNet,
            track_scrapper_v2::model::PageKind::Athlete,
            "https://www.athletic.net/athlete/28872883".into(),
            Default::default(),
        ),
    };
    let output = support::run_operation(directory.path(), Some(&url), &operation).await?;
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
            valid.replace("concurrency = 1","concurrency = 4"),
            valid.replace("requests_per_second = 1.0","requests_per_second = 0.0"),
            valid.replace("requests_per_second = 1.0","requests_per_second = 3.0001"),
            format!("unknown_configuration = true\n{valid}"),
        ];
        let input = support::fixture("athleticnet/bio-28872883.json");
        for mutation in mutations {
            std::fs::write(&config,mutation)?;
            let output = test.operation(&crate::support::OfflineOperation::Replay {config:Some(std::path::PathBuf::from((config.to_str().ok_or_else(|| anyhow::anyhow!("UTF-8 config path"))?).to_string())),input:std::path::PathBuf::from((input.to_str().ok_or_else(|| anyhow::anyhow!("UTF-8 input path"))?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet,track_scrapper_v2::model::PageKind::Athlete,("https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0").to_string(),track_scrapper_v2::model::PageContext {state:Some(("WI").to_string()),athlete_id:Some(("28872883").to_string()), ..Default::default()})}).await?;
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
async fn given_carriage_return_evidence_path_when_replayed_then_explicit_capability_preserves_it()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let directory=test.directory().join("restored\rdata");std::fs::create_dir(&directory)?;
        let evidence=directory.join("evidence");
        let config=directory.join("config.toml");support::write_config(&config)?;
        let input=support::fixture("milesplit/athlete-oh-15005189-current.html");
        let target=track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::MileSplit,track_scrapper_v2::model::PageKind::Athlete,"https://oh.milesplit.com/athletes/15005189-brice-fuller".into(),track_scrapper_v2::model::PageContext {state:Some("OH".into()),athlete_id:Some("15005189".into()),sport:Some(track_scrapper_v2::model::Sport::Outdoor),..Default::default()});
        track_scrapper_v2::runner::Runner::new().replay(&track_scrapper_v2::config::Config::load(&config)?,&test.url,&input,target,&evidence).await?;
        let artifact:String=sqlx::query_scalar("SELECT details#>>'{source_values,_capture,artifact_path}' FROM athlete_sources WHERE source='milesplit'").fetch_one(&test.pool).await?;
        assert_eq!(std::path::Path::new(&artifact).parent(),Some(evidence.as_path()));
        assert!(std::path::Path::new(&artifact).is_file());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM results").fetch_one(&test.pool).await?,28);
        Ok(())
    })).await
}
