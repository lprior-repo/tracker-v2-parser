mod support;

use anyhow::Result;
use sha2::{Digest, Sha256};
use track_scrapper_v2::database::Database;

const CSV: &str = "unit_id,name,address,city,state,zip_code,latitude,longitude,source,validated,geocode_status\n010000500870,Example School,10 Main,Madison,WI,53701,43.1,-89.2,NCES_CSV,False,\n010000500871,Other School,20 Main,Madison,WI,53702,43.2,-89.3,NCES_CSV,False,\n";

fn input(test: &support::Scenario, content: &str) -> Result<(String, String)> {
    let path = test.directory().join("published.csv");
    std::fs::write(&path, content)?;
    Ok((
        path.to_string_lossy().into_owned(),
        format!("{:x}", Sha256::digest(content.as_bytes())),
    ))
}

#[tokio::test]
async fn published_school_import_is_atomic_idempotent_and_keeps_source_rows_distinct() -> Result<()>
{
    support::with_schema(|test| Box::pin(async move {
        Database::connect(&test.url).await?.close().await;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,address,source_url) VALUES ('athleticnet','abc','Example School','WI','Madison','10 Main','athletic-source')").execute(&test.pool).await?;
        let (path, sha) = input(test, CSV)?;
        let args = ["scrape", "--school-addresses", &path, "--school-addresses-sha256", &sha];
        let output = test.cli(&args).await?;
        support::assert_success(&output);
        let rows: Vec<(String,String,Option<String>,Option<serde_json::Value>,String)> = sqlx::query_as("SELECT source,source_id,postal_code,address_provenance,uuid::text FROM schools ORDER BY source,source_id").fetch_all(&test.pool).await?;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "athleticnet");
        assert_eq!(rows[0].2.as_deref(), Some("53701"));
        assert_eq!(rows[1].1, "010000500870");
        assert_ne!(rows[0].4, rows[1].4);
        assert_eq!(rows[1].3.as_ref().and_then(|v| v.pointer("/address/row_details/latitude")).and_then(|v| v.as_str()), Some("43.1"));
        assert_eq!(rows[1].3.as_ref().and_then(|v| v.pointer("/address/row_details/validated")).and_then(|v| v.as_str()), Some("False"));
        assert_eq!(rows[1].3.as_ref().and_then(|v| v.pointer("/address/artifact_sha256")).and_then(|v| v.as_str()), Some(sha.as_str()));
        let output = test.cli(&args).await?;
        support::assert_success(&output);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schools").fetch_one(&test.pool).await?;
        assert_eq!(count, 3);
        let replayed: Vec<(String,String,Option<String>,Option<serde_json::Value>,String)> =
            sqlx::query_as("SELECT source,source_id,postal_code,address_provenance,uuid::text FROM schools ORDER BY source,source_id")
                .fetch_all(&test.pool).await?;
        assert_eq!(rows, replayed);
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,address,source_url) VALUES ('milesplit','later','Other School','WI','Madison','20 Main','later-source')").execute(&test.pool).await?;
        let output = test.cli(&args).await?;
        support::assert_success(&output);
        let postal: Option<String> = sqlx::query_scalar("SELECT postal_code FROM schools WHERE source='milesplit' AND source_id='later'").fetch_one(&test.pool).await?;
        assert_eq!(postal.as_deref(), Some("53702"));
        Ok(())
    })).await
}

#[tokio::test]
async fn published_duplicate_ids_and_bad_hash_never_write() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            Database::connect(&test.url).await?.close().await;
            let duplicated =
                format!("{CSV}010000500870,Duplicate,3 Main,Madison,WI,53703,,,NCES_CSV,False,\n");
            let (path, sha) = input(test, &duplicated)?;
            let output = test
                .cli(&[
                    "scrape",
                    "--school-addresses",
                    &path,
                    "--school-addresses-sha256",
                    &sha,
                ])
                .await?;
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate NCES school IDs"));
            let output = test
                .cli(&[
                    "scrape",
                    "--school-addresses",
                    &path,
                    "--school-addresses-sha256",
                    "wrong",
                ])
                .await?;
            assert!(!output.status.success());
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schools")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 0);
            Ok(())
        })
    })
    .await
}
