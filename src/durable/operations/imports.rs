use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use crate::{
    database::{Database, operation_receipts},
    school_directory::{self, Directory, pss::PrivateDirectory, published::PublishedDirectory},
};

use super::{Artifact, OperationSpec, Workspace};

enum Input {
    Directory(Directory),
    Public(PublishedDirectory),
    Private(PrivateDirectory),
    Cooperatives(Artifact),
    Identity(Artifact),
    Research(super::research::Reviewed),
    ResultLinks(crate::reviewed_result_links::Prepared),
}

fn enrichment(report: &school_directory::EnrichmentReport) -> Value {
    json!({"updated":report.updated,"conflicts":report.conflicts,"uncorroborated":report.uncorroborated})
}

fn validate_capture(workspace: &Workspace, item: &Value) -> Result<()> {
    let artifact = Artifact {
        path: item
            .get("artifact_path")
            .and_then(Value::as_str)
            .context("capture artifact path")?
            .into(),
        sha256: item
            .get("sha256")
            .and_then(Value::as_str)
            .context("capture digest")?
            .into(),
        bytes: item
            .get("bytes")
            .and_then(Value::as_u64)
            .context("capture length")?,
    };
    workspace.read(&artifact)?;
    Ok(())
}

fn validate_bundle(workspace: &Workspace, body: &[u8]) -> Result<()> {
    let manifest: Value = serde_json::from_slice(body)?;
    if let Some(original) = manifest.get("_durable_original_manifest") {
        workspace.read(&serde_json::from_value::<Artifact>(original.clone())?)?;
    }
    let reports = manifest
        .get("program_reports")
        .and_then(Value::as_array)
        .context("co-op reports")?;
    let associations = manifest
        .get("associations")
        .and_then(Value::as_array)
        .context("co-op associations")?;
    ensure!(
        reports.len() == 4 && associations.len() <= 64,
        "co-op manifest exceeds input bounds"
    );
    reports
        .iter()
        .try_for_each(|item| validate_capture(workspace, item))?;
    associations.iter().try_for_each(|item| -> Result<()> {
        validate_capture(
            workspace,
            item.get("host_capture").context("co-op host capture")?,
        )?;
        if let Some(schedules) = item.get("current_schedules").and_then(Value::as_array) {
            ensure!(schedules.len() <= 4, "co-op schedule capacity");
            schedules
                .iter()
                .try_for_each(|capture| validate_capture(workspace, capture))?;
        }
        Ok(())
    })
}

fn prepare(workspace: &Workspace, spec: &OperationSpec) -> Result<Input> {
    workspace.validate_spec(spec)?;
    match spec {
        OperationSpec::SchoolDirectory { csv, manifest } => {
            workspace.read(csv)?;
            workspace.read(manifest)?;
            Ok(Input::Directory(Directory::load(
                &csv.path,
                &manifest.path,
            )?))
        }
        OperationSpec::PublicSchools { csv, origin } => {
            workspace.read(csv)?;
            ensure!(
                origin
                    .as_deref()
                    .is_none_or(|value| value == crate::school_address_download::SOURCE),
                "unsupported public reference origin"
            );
            Ok(Input::Public(PublishedDirectory::load_with_origin(
                &csv.path,
                &csv.sha256,
                origin.as_deref(),
            )?))
        }
        OperationSpec::PrivateSchools { csv } => {
            workspace.read(csv)?;
            Ok(Input::Private(PrivateDirectory::load(
                &csv.path,
                &csv.sha256,
            )?))
        }
        OperationSpec::WiaaCooperatives { manifest } => {
            validate_bundle(workspace, &workspace.read(manifest)?)?;
            Ok(Input::Cooperatives(manifest.clone()))
        }
        OperationSpec::ReviewedIdentity { manifest } => {
            workspace.read(manifest)?;
            Ok(Input::Identity(manifest.clone()))
        }
        OperationSpec::ReviewedResearch { manifest } => Ok(Input::Research(
            super::research::prepare(workspace, manifest)?,
        )),
        OperationSpec::ReviewedResultLinks { manifest } => Ok(Input::ResultLinks(
            crate::reviewed_result_links::prepare(workspace, manifest)?,
        )),
        OperationSpec::DownloadPublicSchools { expected_sha256 } => {
            let path = workspace.path("downloaded-public-schools.csv")?;
            let artifact = Artifact {
                bytes: std::fs::metadata(&path)?.len(),
                path,
                sha256: expected_sha256.to_ascii_lowercase(),
            };
            workspace.read(&artifact)?;
            Ok(Input::Public(PublishedDirectory::load_with_origin(
                &artifact.path,
                &artifact.sha256,
                Some(crate::school_address_download::SOURCE),
            )?))
        }
        _ => bail!("operation requires its dedicated acquisition/export workflow"),
    }
}

async fn apply(connection: &mut sqlx::PgConnection, input: Input) -> Result<Value> {
    match input {
        Input::Directory(directory) => Ok(enrichment(
            &school_directory::enrich_transaction(connection, &directory).await?,
        )),
        Input::Public(directory) => {
            let report =
                school_directory::published::import_transaction(connection, &directory).await?;
            Ok(
                json!({"inserted":report.inserted,"updated":report.updated,"conflicts":report.conflicts,"enrichment":enrichment(&report.enrichment)}),
            )
        }
        Input::Private(directory) => {
            let report = school_directory::pss::import_transaction(connection, &directory).await?;
            Ok(
                json!({"inserted":report.inserted,"updated":report.updated,"conflicts":report.conflicts,"enrichment":enrichment(&report.enrichment)}),
            )
        }
        Input::Cooperatives(manifest) => Ok(serde_json::to_value(
            crate::wiaa_coop_import::import_transaction(
                connection,
                &manifest.path,
                &manifest.sha256,
            )
            .await?,
        )?),
        Input::Identity(manifest) => Ok(serde_json::to_value(
            crate::athlete_identity_import::import_transaction(
                connection,
                &manifest.path,
                &manifest.sha256,
            )
            .await?,
        )?),
        Input::Research(reviewed) => Ok(serde_json::to_value(
            super::research::apply(connection, reviewed).await?,
        )?),
        Input::ResultLinks(prepared) => Ok(serde_json::to_value(
            crate::reviewed_result_links::apply(connection, prepared).await?,
        )?),
    }
}

#[tracing::instrument(skip_all)]
pub async fn execute_import(
    workspace: &Workspace,
    spec: &OperationSpec,
    database_url: &str,
) -> Result<Value> {
    let owned = workspace.clone();
    let spec = spec.clone();
    let input = tokio::task::spawn_blocking(move || prepare(&owned, &spec)).await??;
    let database = Database::connect(database_url).await?;
    let result = operation_receipts::with_receipt(
        database.pool(),
        &workspace.operation_key()?,
        workspace.binding(),
        move |connection| Box::pin(async move { apply(connection, input).await }),
    )
    .await;
    database.close().await;
    let value = result?;
    workspace.record("import-receipt.json", &value)?;
    Ok(value)
}

#[tracing::instrument(skip_all)]
pub async fn download_reference(
    workspace: &Workspace,
    expected: &str,
    pool: sqlx::PgPool,
) -> Result<Artifact> {
    workspace.validate_spec(&OperationSpec::DownloadPublicSchools {
        expected_sha256: expected.to_owned(),
    })?;
    ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "reference digest must be SHA256"
    );
    let path = workspace.path("downloaded-public-schools.csv")?;
    if !path.try_exists()? {
        crate::school_address_download::download_durable(
            expected,
            &path,
            pool,
            workspace.binding(),
        )
        .await?;
    }
    let owned = workspace.clone();
    let expected = expected.to_ascii_lowercase();
    tokio::task::spawn_blocking(move || {
        let artifact = Artifact {
            bytes: std::fs::metadata(&path)?.len(),
            path,
            sha256: expected,
        };
        let bytes = owned.read(&artifact)?;
        owned.store("downloaded-public-schools.csv", &bytes)
    })
    .await?
}
