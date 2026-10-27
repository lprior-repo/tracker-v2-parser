mod support;

use anyhow::Result;
use sha2::{Digest, Sha256};
use track_scrapper_v2::database::Database;

const CSV: &str = "PPIN,PINST,PADDRS,PCITY,PSTABB,PZIP,PZIP4,PPHONE,NUMSTUDS,LATITUDE24,LONGITUDE24,LOGR2024,HIGR2024,PL_ADD\n\
02004116,Box Academy,PO BOX 17,Madison,WI,53701,1234,555-123-4567,120,43.1,-89.2,01,12,99 CAMPUS ROAD\n\
a2380006,Day School,12 MAIN,Madison,WI,53702,,555-123-9876,42,43.2,-89.3,PK,08,55 CAMPUS AVE\n";

fn fixture(test: &support::Scenario, content: &str) -> Result<(String, String)> {
    let path = test.directory().join("pss.csv");
    std::fs::write(&path, content)?;
    Ok((
        path.to_string_lossy().into_owned(),
        format!("{:x}", Sha256::digest(content.as_bytes())),
    ))
}

#[tokio::test]
async fn private_school_mail_tuples_are_preserved_and_reimport_is_idempotent() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        Database::connect(&test.url).await?.close().await;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,address,source_url) VALUES ('athleticnet','1','Box Academy','WI','Madison','PO BOX 17','source')").execute(&test.pool).await?;
        let (path, sha) = fixture(test, CSV)?;
        let args = ["scrape", "--private-school-addresses", &path, "--school-addresses-sha256", &sha];
        let output = test.cli(&args).await?;
        support::assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("2 inserted; 1 enriched"));
        let row: (String, String, Option<String>, serde_json::Value, String) = sqlx::query_as("SELECT source_id,address,postal_code,address_provenance,uuid::text FROM schools WHERE source='nces' AND source_id='pss:02004116'").fetch_one(&test.pool).await?;
        assert_eq!(row.0, "pss:02004116");
        assert_eq!(row.1, "PO BOX 17");
        assert_eq!(row.2.as_deref(), Some("53701-1234"));
        assert_eq!(row.3.pointer("/address/address_type").and_then(|v| v.as_str()), Some("mailing"));
        assert_eq!(row.3.pointer("/record/row_details/latitude").and_then(|v| v.as_str()), Some("43.1"));
        assert_eq!(row.3.pointer("/record/school_year").and_then(|v| v.as_str()), Some("2023-24"));
        let id: String = sqlx::query_scalar("SELECT source_id FROM schools WHERE source_id='pss:A2380006'").fetch_one(&test.pool).await?;
        assert_eq!(id, "pss:A2380006");
        let enriched: (Option<String>, serde_json::Value) = sqlx::query_as("SELECT postal_code,address_provenance FROM schools WHERE source='athleticnet' AND source_id='1'").fetch_one(&test.pool).await?;
        assert_eq!(enriched.0.as_deref(), Some("53701-1234"));
        assert_eq!(enriched.1.pointer("/postal_code/directory/address_type").and_then(|v| v.as_str()), Some("mailing"));
        let output = test.cli(&args).await?;
        support::assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("0 inserted; 0 enriched"));
        let uuid: String = sqlx::query_scalar("SELECT uuid::text FROM schools WHERE source_id='pss:02004116'").fetch_one(&test.pool).await?;
        assert_eq!(uuid, row.4);
        Ok(())
    })).await
}

#[tokio::test]
async fn bad_hash_and_duplicate_ppin_are_rejected_without_writes() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            Database::connect(&test.url).await?.close().await;
            let duplicate =
                format!("{CSV}A2380006,Duplicate,1 MAIN,Madison,WI,53703,,555,10,,,,,\n");
            let (path, sha) = fixture(test, &duplicate)?;
            let output = test
                .cli(&[
                    "scrape",
                    "--private-school-addresses",
                    &path,
                    "--school-addresses-sha256",
                    &sha,
                ])
                .await?;
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate school IDs"));
            let output = test
                .cli(&[
                    "scrape",
                    "--private-school-addresses",
                    &path,
                    "--school-addresses-sha256",
                    "bad",
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
