mod support;

use anyhow::Result;
use track_scrapper_v2::{
    database::Database,
    durable::operations::{OperationSpec, Workspace, execute_import, stage_input},
};

const CSV: &str = "unit_id,name,address,city,state,zip_code,latitude,longitude,source\n010000500870,Example School,10 Main,Madison,WI,53701,43.1,-89.2,NCES_CSV\n";

#[tokio::test]
async fn public_reference_commit_and_receipt_survive_missing_filesystem_ack_without_reapplication()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let root = test.directory().join("artifacts");
            let source = test.directory().join("source.csv");
            std::fs::write(&source, CSV)?;
            let csv = stage_input(&root, &source)?;
            let spec = OperationSpec::PublicSchools { csv, origin: None };
            let workspace = Workspace::open(&root, "reference-a", &spec)?;
            let first = execute_import(&workspace, &spec, &test.url).await?;
            assert_eq!(first["inserted"], 1);
            let before: serde_json::Value =
                sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM schools s")
                    .fetch_one(database.pool())
                    .await?;
            std::fs::remove_file(workspace.path("import-receipt.json")?)?;
            let replay = execute_import(&workspace, &spec, &test.url).await?;
            assert_eq!(replay, first);
            let after: serde_json::Value =
                sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM schools s")
                    .fetch_one(database.pool())
                    .await?;
            assert_eq!(before, after);
            assert_eq!(
                sqlx::query_scalar::<_, String>(
                    "SELECT source_id FROM schools WHERE source='nces'"
                )
                .fetch_one(database.pool())
                .await?,
                "010000500870"
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts")
                    .fetch_one(database.pool())
                    .await?,
                1
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn tampered_import_input_fails_hash_validation_before_database_connection() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source.csv");
    std::fs::write(&source, CSV)?;
    let csv = stage_input(root.path(), &source)?;
    let spec = OperationSpec::PublicSchools {
        csv: csv.clone(),
        origin: None,
    };
    let workspace = Workspace::open(root.path(), "reference-a", &spec)?;
    std::fs::write(&csv.path, b"tampered")?;
    let error = execute_import(&workspace, &spec, "not a database URL")
        .await
        .err()
        .expect("input must fail");
    assert!(error.to_string().contains("hash"));
    assert!(!workspace.path("import-receipt.json")?.exists());
    Ok(())
}
