mod inventory;
pub(super) use inventory::{check_tables, collect as inventory};

use std::{ffi::OsString, str::FromStr, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use tokio::process::Command;

use super::paths;

pub(super) fn target_url(database: &str, role: Option<&str>) -> Result<url::Url> {
    paths::database(database)?;
    ensure!(
        std::env::var_os("PGSERVICE").is_none() && std::env::var_os("PGHOSTADDR").is_none(),
        "unset PGSERVICE/PGHOSTADDR; use explicit libpq connection settings"
    );
    let host = host()?;
    ensure!(
        !host.is_empty() && !host.contains(',') && !host.chars().any(char::is_control),
        "PGHOST must specify one host"
    );
    let port =
        std::env::var("PGPORT").map_or_else(|_| Ok(5432_u16), |value| value.parse::<u16>())?;
    ensure!(port > 0, "PGPORT must be positive");
    let mut address = url::Url::parse("postgresql://localhost")?;
    if host.starts_with('/') {
        address.query_pairs_mut().append_pair("host", &host);
    } else {
        address.set_host(Some(&host))?;
    }
    address
        .set_port(Some(port))
        .map_err(|()| anyhow::anyhow!("invalid PostgreSQL port"))?;
    address.set_path(database);
    if let Some(role) = role {
        ensure!(
            !role.is_empty() && !role.chars().any(char::is_control),
            "invalid target role"
        );
        address
            .set_username(role)
            .map_err(|()| anyhow::anyhow!("invalid target role"))?;
    }
    if let Ok(mode) = std::env::var("PGSSLMODE") {
        ensure!(
            [
                "disable",
                "allow",
                "prefer",
                "require",
                "verify-ca",
                "verify-full"
            ]
            .contains(&mode.as_str()),
            "invalid PGSSLMODE"
        );
        address.query_pairs_mut().append_pair("sslmode", &mode);
    }
    Ok(address)
}

#[tracing::instrument(skip_all)]
pub(super) async fn connect(database: &str) -> Result<PgConnection> {
    let url = target_url(database, None)?;
    let options = PgConnectOptions::from_str(url.as_str())?;
    tokio::time::timeout(
        Duration::from_secs(15),
        PgConnection::connect_with(&options),
    )
    .await
    .context("PostgreSQL connection deadline exceeded")?
    .context("connecting to migration PostgreSQL database")
}

#[tracing::instrument(skip_all)]
pub(super) async fn command(tool: &str, args: &[OsString]) -> Result<String> {
    command_at(tool, args, None).await
}

#[tracing::instrument(skip_all)]
pub(super) async fn command_at(
    tool: &str,
    args: &[OsString],
    directory: Option<&std::path::Path>,
) -> Result<String> {
    ensure!(
        ["pg_dump", "pg_restore", "createdb"].contains(&tool),
        "unsupported migration tool"
    );
    target_url("postgres", None)?;
    let executable =
        directory.map_or_else(|| std::path::PathBuf::from(tool), |path| path.join(tool));
    let mut command = Command::new(executable);
    if !args.iter().any(|arg| arg == "--version") {
        command.arg("--no-password");
    }
    command
        .args(args)
        .env("PGCONNECT_TIMEOUT", "15")
        .env("PGAPPNAME", "track-scrapper-migration")
        .env("PGHOST", host()?)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_mins(30), command.output())
        .await
        .with_context(|| format!("{tool} exceeded30minute deadline"))?
        .with_context(|| format!("cannot execute PostgreSQL tool {tool}"))?;
    ensure!(
        result.status.success(),
        "{tool} failed; inspect PostgreSQL locally (diagnostic output withheld to protect credentials)"
    );
    ensure!(
        result.stdout.len() <= 1024 * 1024,
        "PostgreSQL tool output exceeds limit"
    );
    Ok(String::from_utf8(result.stdout)?)
}

pub(super) fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[tracing::instrument(skip_all)]
pub(super) async fn version(connection: &mut PgConnection) -> Result<(u32, String)> {
    let (number, version): (String, String) = sqlx::query_as(
        "SELECT current_setting('server_version_num'),current_setting('server_version')",
    )
    .fetch_one(connection)
    .await?;
    let major = number
        .parse::<u32>()?
        .checked_div(10_000)
        .context("invalid PostgreSQL version")?;
    ensure!(major >= 13, "PostgreSQL13or newer is required");
    Ok((major, version))
}

#[tracing::instrument(skip_all)]
pub(super) async fn tool_major(tool: &str) -> Result<u32> {
    tool_major_at(tool, None).await
}

#[tracing::instrument(skip_all)]
pub(super) async fn tool_major_at(tool: &str, directory: Option<&std::path::Path>) -> Result<u32> {
    let text = command_at(tool, &args(&["--version"]), directory).await?;
    let expression = regex::Regex::new(r"\b([0-9]+)(?:\.[0-9]+)*")?;
    let major = expression
        .captures(&text)
        .and_then(|matched| matched.get(1))
        .context("invalid PostgreSQL tool version")?
        .as_str()
        .parse()?;
    Ok(major)
}

#[tracing::instrument(skip_all)]
pub(super) async fn prepare_snapshot(connection: &mut PgConnection) -> Result<String> {
    sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO, YMD'; SET LOCAL extra_float_digits=3; SET LOCAL bytea_output='hex'; SET LOCAL statement_timeout='10min'; SET LOCAL idle_in_transaction_session_timeout='30min';")
        .execute(&mut *connection).await?;
    let snapshot: String = sqlx::query_scalar("SELECT pg_export_snapshot()")
        .fetch_one(&mut *connection)
        .await?;
    ensure!(
        regex::Regex::new(r"^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{8}-[0-9]+$")?.is_match(&snapshot),
        "invalid exported snapshot identifier"
    );
    Ok(snapshot)
}

pub(super) fn restore_version(source: u32, target: u32, client: u32) -> Result<()> {
    ensure!(
        source >= 13 && target >= source && client >= source && client <= target,
        "restore requires PostgreSQL13+ and a same-or-newer target/client compatible with source"
    );
    Ok(())
}

#[tracing::instrument(skip_all)]
pub(super) async fn create_database(
    database: &str,
    owner: &str,
    admin_variable: Option<&str>,
    tools: Option<&std::path::Path>,
) -> Result<()> {
    paths::database(database)?;
    paths::database(owner)?;
    let Some(variable) = admin_variable else {
        command_at(
            "createdb",
            &args(&[
                "--maintenance-db=postgres",
                "--template=template0",
                "--encoding=UTF8",
                database,
            ]),
            tools,
        )
        .await?;
        return Ok(());
    };
    ensure!(
        !variable.is_empty()
            && variable.len() <= 128
            && variable
                .bytes()
                .enumerate()
                .all(|(index, byte)| byte == b'_'
                    || byte.is_ascii_alphabetic()
                    || (index > 0 && byte.is_ascii_digit())),
        "invalid administrator environment variable name"
    );
    let address = std::env::var(variable)
        .context("explicit migration administrator environment variable is missing")?;
    let admin_options = PgConnectOptions::from_str(&address).map_err(|_| {
        anyhow::anyhow!("invalid explicit migration administrator connection settings")
    })?;
    let target_options = PgConnectOptions::from_str(target_url("postgres", Some(owner))?.as_str())?;
    admin_endpoint(&admin_options, &target_options)?;
    let mut admin = tokio::time::timeout(
        Duration::from_secs(15),
        PgConnection::connect_with(&admin_options),
    )
    .await
    .context("migration administrator connection deadline")??;
    let statement = format!(
        "CREATE DATABASE \"{database}\" OWNER \"{owner}\" TEMPLATE template0 ENCODING 'UTF8'"
    );
    let created = sqlx::query(sqlx::AssertSqlSafe(statement))
        .execute(&mut admin)
        .await;
    let closed = admin.close().await;
    created.context("creating a new target database with explicit administrator")?;
    closed.context("closing migration administrator connection")?;
    Ok(())
}

pub(super) fn admin_endpoint(admin: &PgConnectOptions, target: &PgConnectOptions) -> Result<()> {
    ensure!(
        admin.get_host() == target.get_host()
            && admin.get_port() == target.get_port()
            && admin.get_socket() == target.get_socket(),
        "explicit migration administrator must address the same PostgreSQL endpoint as target settings"
    );
    Ok(())
}

pub(super) fn host() -> Result<String> {
    match std::env::var("PGHOST") {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => Ok("localhost".to_owned()),
        Err(std::env::VarError::NotUnicode(_)) => bail!("PGHOST must be UTF-8"),
    }
}
