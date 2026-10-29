use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use sqlx::Connection;

use super::{
    BackupArgs, archive, database, exports, files,
    manifest::{FORMAT, Manifest, TABLES},
    paths,
};

#[tracing::instrument(skip_all)]
pub(super) async fn run(args: BackupArgs) -> Result<()> {
    paths::database(&args.database)?;
    let data = paths::source(&args.data_dir, true)?;
    let artifacts = paths::source(&args.artifacts, true)?;
    let config = paths::source(&args.config, false)?;
    let output = paths::absolute(&args.output)?;
    let parent = paths::source(output.parent().context("backup output parent")?, true)?;
    ensure!(
        output
            .to_str()
            .is_some_and(|name| name.ends_with(".tar.gz"))
            && !output.try_exists()?,
        "backup output must be a new.tar.gz file"
    );
    ensure!(
        !output.starts_with(&data) && !output.starts_with(&artifacts) && output != config,
        "backup output must be outside source data and artifacts"
    );
    let temporary = tempfile::Builder::new()
        .prefix(".native-migration-")
        .tempdir_in(&parent)?;
    let staging = temporary.path().join("payload");
    files::directory(&staging)?;
    let staging_copy = staging.clone();
    let source_data = data.clone();
    let selected = args.export_file;
    tokio::task::spawn_blocking(move || stage(&source_data, &config, &selected, &staging_copy))
        .await
        .context("staging backup payload")??;
    let mut connection = database::connect(&args.database).await?;
    let (major, version) = database::version(&mut connection).await?;
    database::check_tables(&mut connection, TABLES.len()).await?;
    ensure!(
        database::tool_major("pg_dump").await? == major,
        "pg_dump major must match source server for portable backup"
    );
    let mut snapshot = connection.begin().await?;
    let snapshot_id = database::prepare_snapshot(&mut snapshot).await?;
    let dump = staging.join("database.dump");
    let mut command = database::args(&[
        "--dbname",
        &args.database,
        "--format=custom",
        "--schema=public",
        "--no-owner",
        "--no-privileges",
        "--snapshot",
        &snapshot_id,
        "--file",
    ]);
    command.push(dump.clone().into_os_string());
    database::command("pg_dump", &command).await?;
    ensure!(
        fs::metadata(&dump)?.len() > 5,
        "pg_dump did not produce a custom archive"
    );
    fs::set_permissions(&dump, fs::Permissions::from_mode(0o600))?;
    let inventory = database::inventory(&mut snapshot, TABLES.len()).await?;
    let references = super::references::collect(&mut snapshot).await?;
    snapshot.rollback().await?;
    connection.close().await?;
    files::json(&staging.join("inventory.json"), &inventory)?;
    let bundle = temporary.path().join("bundle.partial");
    let destination = output.clone();
    tokio::task::spawn_blocking(move || {
        if data.join("evidence").try_exists()? {
            copy_tree(
                &data.join("evidence"),
                &staging.join("evidence"),
                "evidence",
            )?;
        }
        copy_tree(
            &artifacts,
            &staging.join("durable-artifacts"),
            "durable-artifacts",
        )?;
        pack(&staging, &bundle, &destination, major, version, references)?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("publishing native backup")??;
    tracing::info!(path=%output.display(),tables=TABLES.len(),"native migration backup complete; Restate server journals require separate backup");
    Ok(())
}

fn pack(
    staging: &Path,
    bundle: &Path,
    destination: &Path,
    major: u32,
    version: String,
    references: Vec<crate::durable::page::ArtifactRef>,
) -> Result<()> {
    let entries = files::list(staging)?
        .iter()
        .map(|path| files::fingerprint(staging, path))
        .collect::<Result<Vec<_>>>()?;
    let manifest = Manifest {
        format: FORMAT.into(),
        created_at_utc: chrono::Utc::now().to_rfc3339(),
        source_postgres_major: major,
        source_postgres_version: version,
        database_dump: "database.dump".into(),
        config: "config.toml".into(),
        evidence_dir: "evidence".into(),
        exports_dir: "exports".into(),
        inventory: "inventory.json".into(),
        artifacts_dir: Some("durable-artifacts".into()),
        required_artifacts: references,
        files: entries,
    };
    super::references::validate(staging, &manifest.required_artifacts, &manifest.files)?;
    files::json(&staging.join("manifest.json"), &manifest)?;
    archive::write(staging, bundle)?;
    fs::hard_link(bundle, destination).context("publishing backup without overwrite")?;
    fs::File::open(destination.parent().context("backup parent")?)?.sync_all()?;
    Ok(())
}

fn stage(data: &Path, config: &Path, selected: &[PathBuf], staging: &Path) -> Result<()> {
    files::copy(config, &staging.join("config.toml"))?;
    paths::config(&staging.join("config.toml"))?;
    if data.join("scrape.toml").try_exists()? {
        files::copy(&data.join("scrape.toml"), &staging.join("scrape.toml"))?;
        paths::config(&staging.join("scrape.toml"))?;
    }
    ["evidence", "exports", "durable-artifacts"]
        .iter()
        .try_for_each(|name| files::directory(&staging.join(name)))?;
    exports::collect(&data.join("exports"), selected)?
        .iter()
        .try_for_each(|(relative, source)| {
            files::copy(source, &staging.join("exports").join(relative))
        })?;
    exports::validate(&staging.join("exports"))
}

fn copy_tree(root: &Path, destination: &Path, kind: &str) -> Result<()> {
    files::list(root)?
        .iter()
        .filter(|path| path.extension().is_none_or(|extension| extension != "tmp"))
        .try_for_each(|source| {
            let relative = source.strip_prefix(root)?;
            let name = format!(
                "{kind}/{}",
                relative.to_str().context("UTF-8 artifact path")?
            );
            paths::payload(&name, false)?;
            let target = destination.join(relative);
            files::copy(source, &target)?;
            if kind == "evidence"
                || target
                    .extension()
                    .is_some_and(|extension| extension == "blob" || extension == "input")
            {
                let fingerprint = files::fingerprint(destination, &target)?;
                ensure!(
                    target.file_stem().and_then(|name| name.to_str())
                        == Some(fingerprint.sha256.as_str()),
                    "artifact content differs from SHA256 filename"
                );
            }
            Ok(())
        })
}
