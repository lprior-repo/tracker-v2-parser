use std::process::Command;

use anyhow::Result;

#[test]
fn removed_wiaa_command_cannot_reach_database_or_source_http_even_without_a_guard_flag()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_track-scrapper-v2"))
        .env_remove("SCRAPER_REQUIRE_DURABLE")
        .arg("--database-url")
        .arg("")
        .arg("scrape")
        .arg("--wiaa-directory")
        .current_dir(directory.path())
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let diagnostics = String::from_utf8(output.stderr)?;
    assert!(
        diagnostics.contains("unrecognized subcommand 'scrape'"),
        "removed parser branch must reject before database URL resolution: {diagnostics}"
    );
    assert!(!diagnostics.contains("database URL is empty"));
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}
