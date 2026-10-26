use std::process::{Command, Output};

use anyhow::{Context, Result};
use sqlx::{Connection, SqliteConnection};
use tempfile::tempdir;

fn run_app(directory: &std::path::Path, database_url: Option<&str>) -> Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_track-scrapper-v2"));
    command.current_dir(directory).env_remove("DATABASE_URL");
    if let Some(url) = database_url {
        command.env("DATABASE_URL", url);
    }
    command.output().context("running the bootstrap CLI")
}

#[tokio::test]
async fn given_no_configuration_when_started_then_creates_a_usable_database() -> Result<()> {
    // Given an isolated working directory and no DATABASE_URL.
    let directory = tempdir()?;

    // When the actual executable starts.
    let output = run_app(directory.path(), None)?;

    // Then it succeeds and leaves a real writable SQLite database.
    assert!(output.status.success(), "{output:?}");
    assert!(directory.path().join("track-scrapper.sqlite3").is_file());
    let url = format!(
        "sqlite://{}",
        directory.path().join("track-scrapper.sqlite3").display()
    );
    let mut connection = SqliteConnection::connect(&url).await?;
    sqlx::query("CREATE TABLE smoke (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .execute(&mut connection)
        .await?;
    sqlx::query("INSERT INTO smoke (id, name) VALUES (1, 'athlete')")
        .execute(&mut connection)
        .await?;
    let name: String = sqlx::query_scalar("SELECT name FROM smoke WHERE id = 1")
        .fetch_one(&mut connection)
        .await?;
    assert_eq!(name, "athlete");
    connection.close().await?;
    Ok(())
}

#[tokio::test]
async fn given_existing_data_when_restarted_then_preserves_it_at_the_configured_path() -> Result<()>
{
    // Given a database created at an explicitly configured path containing spaces.
    let directory = tempdir()?;
    let url = format!(
        "sqlite://{}",
        directory.path().join("existing results.sqlite3").display()
    );
    let first = run_app(directory.path(), Some(&url))?;
    assert!(first.status.success(), "{first:?}");
    let mut connection = SqliteConnection::connect(&url).await?;
    sqlx::query("CREATE TABLE results (athlete TEXT NOT NULL)")
        .execute(&mut connection)
        .await?;
    sqlx::query("INSERT INTO results (athlete) VALUES ('retained athlete')")
        .execute(&mut connection)
        .await?;
    connection.close().await?;

    // When the CLI starts again against the same file.
    let second = run_app(directory.path(), Some(&url))?;

    // Then it succeeds without resetting previously stored data.
    assert!(second.status.success(), "{second:?}");
    let mut connection = SqliteConnection::connect(&url).await?;
    let athlete: String = sqlx::query_scalar("SELECT athlete FROM results")
        .fetch_one(&mut connection)
        .await?;
    assert_eq!(athlete, "retained athlete");
    assert!(!directory.path().join("track-scrapper.sqlite3").exists());
    connection.close().await?;
    Ok(())
}

#[test]
fn given_a_missing_parent_when_started_then_fails_instead_of_using_another_database() -> Result<()>
{
    // Given a configured database under a nonexistent parent directory.
    let directory = tempdir()?;
    let url = format!(
        "sqlite://{}/missing/results.sqlite3",
        directory.path().display()
    );

    // When startup attempts to open it.
    let output = run_app(directory.path(), Some(&url))?;

    // Then failure is visible and no fallback database is created.
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!directory.path().join("track-scrapper.sqlite3").exists());
    Ok(())
}

#[test]
fn given_a_corrupt_database_when_started_then_fails_without_replacing_the_file() -> Result<()> {
    // Given an existing file that is not a SQLite database.
    let directory = tempdir()?;
    let path = directory.path().join("corrupt.sqlite3");
    let contents = b"not a SQLite database";
    std::fs::write(&path, contents)?;
    let url = format!("sqlite://{}", path.display());

    // When startup checks the file.
    let output = run_app(directory.path(), Some(&url))?;

    // Then it reports failure and preserves the bytes for investigation.
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(std::fs::read(path)?, contents);
    Ok(())
}
