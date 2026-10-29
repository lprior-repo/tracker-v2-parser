mod operations;
pub use operations::OfflineOperation;
pub use operations::run as run_operation;

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::FutureExt;
use futures::future::BoxFuture;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection, PgPool};
use tempfile::TempDir;
use tokio::process::Command;
use url::Url;

static SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct Scenario {
    pub url: String,
    pub pool: PgPool,
    directory: TempDir,
}

impl Scenario {
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    pub async fn cli(&self, args: &[&str]) -> Result<Output> {
        run_cli(self.directory(), Some(&self.url), args).await
    }

    pub fn operation<'a>(
        &'a self,
        operation: &'a OfflineOperation,
    ) -> BoxFuture<'a, Result<Output>> {
        run_operation(self.directory(), Some(&self.url), operation)
    }
}

pub fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(relative)
}

pub async fn run_cli(directory: &Path, url: Option<&str>, args: &[&str]) -> Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_track-scrapper-v2"));
    command
        .current_dir(directory)
        .env_remove("DATABASE_URL")
        .env_remove("TEST_DATABASE_URL")
        .env("RUST_LOG", "error")
        .env("TRACK_SCRAPPER_EVIDENCE_DIR", directory.join("evidence"))
        .kill_on_drop(true);
    if let Some(url) = url {
        command.args(["--database-url", url]);
    }
    command.args(args);
    tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .context("CLI exceeded its 30 second test deadline")?
        .context("running the real scraper CLI")
}

pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed: status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub async fn with_schema<F>(scenario: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a Scenario) -> BoxFuture<'a, Result<()>>,
{
    let configured = match std::env::var("TEST_DATABASE_URL") {
        Ok(value) => value,
        Err(_) => dotenvy::from_path_iter(Path::new(env!("CARGO_MANIFEST_DIR")).join(".env"))
            .context("TEST_DATABASE_URL is required; configure it in the environment or .env")?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .find_map(|(key, value)| (key == "TEST_DATABASE_URL").then_some(value))
            .context("TEST_DATABASE_URL is required; database scenarios are never skipped")?,
    };
    let mut scoped = Url::parse(&configured).context("parsing TEST_DATABASE_URL")?;
    if !matches!(scoped.scheme(), "postgres" | "postgresql") {
        bail!("TEST_DATABASE_URL must use PostgreSQL");
    }
    let schema = format!(
        "scenario_{}_{}",
        std::process::id(),
        SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut admin = PgConnection::connect(&configured)
        .await
        .context("connecting to required test PostgreSQL database")?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut admin)
        .await?;
    let pairs: Vec<(String, String)> = scoped
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    scoped.set_query(None);
    scoped.query_pairs_mut().extend_pairs(pairs);
    scoped
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let result = AssertUnwindSafe(run_scenario(scoped.as_str(), scenario))
        .catch_unwind()
        .await;
    let cleanup = sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&mut admin)
        .await;
    let close = admin.close().await;
    cleanup.context("removing isolated test schema")?;
    close.context("closing test schema administration connection")?;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn run_scenario<F>(url: &str, scenario: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a Scenario) -> BoxFuture<'a, Result<()>>,
{
    let test = Scenario {
        url: url.to_owned(),
        pool: PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(10))
            .connect(url)
            .await?,
        directory: tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?,
    };
    let result = scenario(&test).await;
    test.pool.close().await;
    result
}

pub async fn import_bio(test: &Scenario) -> Result<Output> {
    let path = fixture("athleticnet/bio-28872883.json");
    test.operation(&crate::support::OfflineOperation::Replay {config:None,input:std::path::PathBuf::from((path.to_str().context("fixture path is not UTF-8")?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet,track_scrapper_v2::model::PageKind::Athlete,("https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0").to_string(),track_scrapper_v2::model::PageContext {state:Some(("WI").to_string()),athlete_id:Some(("28872883").to_string()),season:Some(("2026").parse::<i32>()?),sport:Some(track_scrapper_v2::model::Sport::Outdoor), ..Default::default()})})
    .await
}

pub fn write_config(path: &Path) -> Result<()> {
    std::fs::write(
        path,
        r#"states = ["WI", "OH", "CA"]
graduation_years = [2027, 2028]
season = 2026
[limits]
requests_per_second = 1.0
max_pages = 10
max_attempts = 3
concurrency = 1
[sources.athleticnet]
enabled = true
transport = "http"
[sources.milesplit]
enabled = true
transport = "http"
[export]
rows_per_file = 1000
per_athlete_sheets = false
"#,
    )?;
    Ok(())
}
