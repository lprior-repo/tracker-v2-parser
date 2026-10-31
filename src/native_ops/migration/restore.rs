use std::{fs, io::Write, os::unix::fs::DirBuilderExt, path::Path};

use anyhow::{Context, Result, ensure};
use sqlx::Connection;

use super::{RestoreArgs, archive, database, files, manifest::LEGACY, paths};

#[tracing::instrument(skip_all)]
pub(super) async fn run(args: RestoreArgs) -> Result<()> {
    paths::database(&args.database)?;
    let destination = paths::absolute(&args.data_dir)?;
    ensure!(
        !destination.try_exists()?,
        "target data directory exists; refusing overwrite"
    );
    let parent = destination
        .ancestors()
        .skip(1)
        .find(|path| path.exists())
        .context("target has no existing parent")?;
    let parent = paths::source(parent, true)?;
    let archive = paths::source(&args.archive, false)?;
    let temporary = tempfile::Builder::new()
        .prefix(".native-restore-")
        .tempdir_in(parent)?;
    let root = temporary.path().to_owned();
    let extracted = root.clone();
    let (manifest, expected) =
        tokio::task::spawn_blocking(move || archive::unpack(&archive, &extracted))
            .await
            .context("validating migration archive")??;
    let tools = if args.bootstrap_macos {
        Some(super::bootstrap::run(manifest.source_postgres_major).await?)
    } else {
        None
    };
    let mut connection = database::connect("postgres").await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1)")
            .bind(&args.database)
            .fetch_one(&mut connection)
            .await?;
    ensure!(
        !exists,
        "target database exists; choose a new database name"
    );
    let (major, _) = database::version(&mut connection).await?;
    database::restore_version(
        manifest.source_postgres_major,
        major,
        database::tool_major_at("pg_restore", tools.as_deref()).await?,
    )?;
    let role: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut connection)
        .await?;
    let url = database::target_url(&args.database, Some(&role))?;
    connection.close().await?;
    ensure!(
        !destination.try_exists()?,
        "target data directory appeared during validation"
    );
    database::create_database(
        &args.database,
        &role,
        args.create_with_admin_env.as_deref(),
        tools.as_deref(),
    )
    .await?;
    let result = restore_database(
        &args.database,
        &root,
        &expected,
        manifest.format == LEGACY,
        tools.as_deref(),
        &manifest.required_artifacts,
    )
    .await;
    if let Err(error) = result {
        return Err(error).context(format!(
            "new database {} retained as partial target; no existing database was changed",
            args.database
        ));
    }
    let target = destination.clone();
    let address = url.to_string();
    tokio::task::spawn_blocking(move || publish(&root,&target,&address)).await.context("publishing restored application directory")?
        .context("restored database retained; any partial data directory is retained, not a completed restore")?;
    instructions(&destination, &args.database, expected.tables.len())?;
    if manifest.format == LEGACY {
        println!(
            "Legacy v1 verifies only seven core tables. Any restored durability tables were not fingerprinted by the legacy producer; durability completeness is unverified."
        );
    }
    Ok(())
}

#[tracing::instrument(skip_all)]
async fn restore_database(
    database: &str,
    root: &Path,
    expected: &super::manifest::Inventory,
    legacy: bool,
    tools: Option<&Path>,
    required: &[crate::durable::page::ArtifactRef],
) -> Result<()> {
    let mut command = database::args(&[
        "--dbname",
        database,
        "--no-owner",
        "--no-privileges",
        "--single-transaction",
        "--exit-on-error",
        "--clean",
        "--if-exists",
    ]);
    command.push(root.join("database.dump").into_os_string());
    database::command_at("pg_restore", &command, tools).await?;
    let mut connection = database::connect(database).await?;
    let mut snapshot = connection.begin().await?;
    database::prepare_snapshot(&mut snapshot).await?;
    let actual = database::inventory(&mut snapshot, expected.tables.len()).await?;
    ensure!(
        &actual == expected,
        "restored logical table hashes or counts differ from source inventory"
    );
    if !legacy {
        ensure!(
            super::references::collect(&mut snapshot).await? == required,
            "restored artifact references differ from snapshot manifest"
        );
    }
    snapshot.rollback().await?;
    connection.close().await?;
    Ok(())
}

pub(super) fn dotenv(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

fn publish(root: &Path, destination: &Path, url: &str) -> Result<()> {
    ensure!(
        !destination.try_exists()?,
        "target data directory appeared during restore"
    );
    let parent = destination.parent().context("restore destination parent")?;
    files::directory(parent)?;
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    ["evidence", "exports", "durable-artifacts"]
        .iter()
        .try_for_each(|name| {
            let source = root.join(name);
            if source.try_exists()? {
                fs::rename(source, destination.join(name))?;
            }
            Ok::<_, anyhow::Error>(())
        })?;
    ["config.toml", "scrape.toml"].iter().try_for_each(|name| {
        let source = root.join(name);
        if source.try_exists()? {
            files::copy(&source, &destination.join(name))?;
        }
        Ok::<_, anyhow::Error>(())
    })?;
    [".env", "private.env"].iter().try_for_each(|name| {
        let value = if *name == ".env" {
            format!("DATABASE_URL={}\n", dotenv(url))
        } else {
            private_env(url)?
        };
        let mut file = files::create(&destination.join(name))?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        Ok::<_, anyhow::Error>(())
    })?;
    fs::File::open(destination)?.sync_all()?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub(super) fn private_env(url: &str) -> Result<String> {
    Ok(format!("DATABASE_URL={}\n", serde_json::to_string(url)?))
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn instructions(destination: &Path, database: &str, tables: usize) -> Result<()> {
    let operator = std::env::current_exe()?;
    let binary = operator
        .parent()
        .context("native executable directory")?
        .join("track-scrapper-v2");
    let worker = format!(
        "cd {} && env -u DATABASE_URL {} durable serve --bind 127.0.0.1:9086 --artifacts {} --published-workbook {}",
        quote(destination),
        quote(&binary),
        quote(&destination.join("durable-artifacts")),
        quote(&destination.join("exports/restored.xlsx"))
    );
    let export = format!(
        "{} durable export --config {} --operation-id restored-export-{} --ingress \"$RESTORED_RESTATE_INGRESS\"",
        quote(&binary),
        quote(&destination.join("config.toml")),
        chrono::Utc::now().timestamp_millis()
    );
    println!(
        "Restore complete: database {database}; {tables} logical table fingerprints match.\nPrivate application data: {}\nPasswordless private.env/.env use the target role's own libpq credentials. Source credentials were not copied.\nStart a dedicated worker for this restored database:\n{worker}\nRegister with a separate Restate server on free loopback ports; never use a production worker bound to another database.\nSet RESTORED_RESTATE_INGRESS to that isolated server, then export without acquisition:\n{export}\nThis application bundle does not restore Restate server journals.",
        destination.display()
    );
    Ok(())
}
