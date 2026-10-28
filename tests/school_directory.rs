mod support;

use anyhow::Result;
use sha2::{Digest, Sha256};
use track_scrapper_v2::database::Database;

const CSV: &str = "key,name,kind,street1,street2,city,state,zip,phone,website,grades,enrollment,latitude,longitude,sources\n\
nces:1,Street School,Public,10 Main,,Madison,WI,53701,,https://street.example,,,,,nces-ccd\n\
nces:2,Web School,Private,PO Box 7,,Madison,WI,53702,,https://web.example,,,,,nces-pss\n\
nces:3,Conflict School,Public,12 Main,,Madison,WI,53703,,https://conflict.example,,,,,nces-ccd\n\
nces:4,Duplicate School,Public,13 Main,,Madison,WI,53704,,https://duplicate.example,,,,,nces-ccd\n\
nces:5,Duplicate School,Public,14 Main,,Madison,WI,53705,,https://other.example,,,,,nces-pss\n\
nces:6,Unconfirmed School,Public,15 Main,,Madison,WI,53706,,https://unconfirmed.example,,,,,nces-ccd\n\
nces:7,Database Duplicate,Public,16 Main,,Madison,WI,53707,,https://dbdup.example,,,,,nces-ccd\n\
nces:8,Postal Conflict,Public,17 Main,,Madison,WI,53708,,https://postal.example,,,,,nces-ccd\n";

fn fixture(test: &support::Scenario, content: &str) -> Result<(String, String)> {
    let csv = test.directory().join("directory.csv");
    let manifest = test.directory().join("manifest.json");
    let hash = format!("{:x}", Sha256::digest(content.as_bytes()));
    let data = serde_json::json!({"schema_revision":1,"generation_digest":"generation", "run":{"run_id":"run", "inputs":{"lanes":[{"source":"nces-ccd", "sha256":"input"}]}},"artifacts":[{"name":"school_directory.csv","sha256":hash,"bytes":content.len()}]});
    std::fs::write(&csv, content)?;
    std::fs::write(&manifest, serde_json::to_vec(&data)?)?;
    Ok((
        csv.to_string_lossy().into_owned(),
        manifest.to_string_lossy().into_owned(),
    ))
}

#[tokio::test]
async fn offline_directory_preserves_conflicts_and_ambiguity() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        Database::connect(&test.url).await?.close().await;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,address,postal_code,website,source_url) VALUES \
          ('athleticnet','1','Street School','WI','Madison','10 Main',NULL,NULL,'source'), \
          ('athleticnet','2','Web School','WI','Madison',NULL,NULL,'https://web.example','source'), \
          ('athleticnet','3','Conflict School','WI','Madison','other street',NULL,'https://conflict.example','source'), \
          ('athleticnet','4','Duplicate School','WI','Madison',NULL,NULL,'https://duplicate.example','source'), \
          ('athleticnet','5','Unconfirmed School','WI','Madison',NULL,NULL,NULL,'source'), \
          ('athleticnet','6','Database Duplicate','WI','Madison','16 Main',NULL,NULL,'source'), \
          ('milesplit','7','Database Duplicate','WI','Madison','16 Main',NULL,NULL,'source'), \
          ('athleticnet','8','Postal Conflict','WI','Madison','17 Main','99999',NULL,'source')")
          .execute(&test.pool).await?;
        let (csv, manifest) = fixture(test, CSV)?;
        let args = ["scrape", "--school-directory", &csv, "--school-directory-manifest", &manifest];
        let output = test.cli(&args).await?;
        support::assert_success(&output);
        let rows: Vec<(String, Option<String>, Option<String>, Option<serde_json::Value>, String)> = sqlx::query_as("SELECT source_id,address,postal_code,address_provenance,source_url FROM schools ORDER BY source_id").fetch_all(&test.pool).await?;
        assert_eq!(rows[0].1.as_deref(), Some("10 Main"));
        assert_eq!(rows[0].2.as_deref(), Some("53701"));
        assert_eq!(rows[0].3.as_ref().and_then(|p| p.pointer("/postal_code/directory/address_type")).and_then(|v| v.as_str()), Some("unspecified_by_directory"));
        assert_eq!(rows[1].1.as_deref(), Some("PO Box 7"));
        assert_eq!(rows[1].2.as_deref(), Some("53702"));
        assert_eq!(rows[1].3.as_ref().and_then(|p| p.pointer("/address/directory_sources")).and_then(|v| v.as_str()), Some("nces-pss"));
        assert_eq!(rows[2].1.as_deref(), Some("other street"));
        assert!(rows[3..7].iter().all(|row| row.3.is_none()));
        assert_eq!(rows[7].2.as_deref(), Some("99999"));
        assert!(rows.iter().all(|row| row.4 == "source"));
        let again = test.cli(&args).await?;
        support::assert_success(&again);
        Ok(())
    })).await
}

#[tokio::test]
async fn wrong_manifest_rejects_before_database_updates() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        Database::connect(&test.url).await?.close().await;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,website,source_url) VALUES ('athleticnet','1','Web School','WI','Madison','https://web.example','source')").execute(&test.pool).await?;
        let (csv, manifest) = fixture(test, CSV)?;
        std::fs::write(&csv, "changed")?;
        let output = test.cli(&["scrape", "--school-directory", &csv, "--school-directory-manifest", &manifest]).await?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("does not match manifest"));
        let address: Option<String> = sqlx::query_scalar("SELECT address FROM schools WHERE source_id='1'").fetch_one(&test.pool).await?;
        assert_eq!(address, None);
        Ok(())
    })).await
}

#[tokio::test]
async fn website_corroboration_preserves_paths_but_canonicalizes_hosts() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        Database::connect(&test.url).await?.close().await;
        let content = "key,name,street1,street2,city,state,zip,website,sources\n\
          nces:1,Case School,10 Main,,Madison,WI,53701,https://web.example/school,nces-ccd\n\
          nces:2,Slash School,20 Main,,Madison,WI,53702,https://web.example/campus/,nces-ccd\n\
          nces:3,Host School,30 Main,,Madison,WI,53703,https://HOST.example/School,nces-ccd\n";
        let csv = test.directory().join("urls.csv");
        let manifest = test.directory().join("urls.json");
        std::fs::write(&csv, content)?;
        let hash = format!("{:x}", Sha256::digest(content.as_bytes()));
        let data = serde_json::json!({"schema_revision":1,"generation_digest":"urls",
          "run":{}, "artifacts":[{"name":"school_directory.csv","sha256":hash,"bytes":content.len()}]});
        std::fs::write(&manifest, serde_json::to_vec(&data)?)?;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,website,source_url) VALUES \
          ('athleticnet','1','Case School','WI','Madison','https://web.example/School','source'), \
          ('athleticnet','2','Slash School','WI','Madison','https://web.example/campus','source'), \
          ('athleticnet','3','Host School','WI','Madison','https://host.example/School','source')")
          .execute(&test.pool).await?;
        let output = test.cli(&["scrape", "--school-directory", &csv.to_string_lossy(),
          "--school-directory-manifest", &manifest.to_string_lossy()]).await?;
        support::assert_success(&output);
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
          "SELECT source_id,address FROM schools ORDER BY source_id").fetch_all(&test.pool).await?;
        assert_eq!(rows, vec![("1".into(), None), ("2".into(), None), ("3".into(), Some("30 Main".into()))]);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_duplicate_directory_identity_when_replayed_then_owned_address_is_unchanged()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        Database::connect(&test.url).await?.close().await;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,website,source_url) VALUES ('milesplit','alpha','Alpha High','WI','Madison','https://alpha.example/','source')")
            .execute(&test.pool).await?;
        let original = "key,name,street1,street2,city,state,zip,website,sources\nsource:1,Alpha High,10 Main,,Madison,WI,53701,https://alpha.example/,reference\n";
        let (csv, manifest) = fixture(test, original)?;
        let args = ["scrape", "--school-directory", &csv, "--school-directory-manifest", &manifest];
        support::assert_success(&test.cli(&args).await?);
        let before: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(s) FROM schools s WHERE source_id='alpha'")
            .fetch_one(&test.pool).await?;
        let duplicate = format!("{original}source:1,Beta High,20 Main,,Madison,WI,53702,https://beta.example/,reference\n");
        fixture(test, &duplicate)?;
        let output = test.cli(&args).await?;
        assert!(!output.status.success(), "ambiguous source identity was accepted");
        assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate source identities"));
        let after: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(s) FROM schools s WHERE source_id='alpha'")
            .fetch_one(&test.pool).await?;
        assert_eq!(after, before);
        assert_eq!(after["address"], "10 Main");
        assert_eq!(after["postal_code"], "53701");
        Ok(())
    })).await
}

#[tokio::test]
async fn given_source_scoped_directory_ids_when_names_change_then_both_owned_addresses_update()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        Database::connect(&test.url).await?.close().await;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,website,source_url) VALUES \
            ('milesplit','alpha','Alpha High','WI','Madison','https://alpha.example/','source'), \
            ('milesplit','beta','Beta High','WI','Madison','https://beta.example/','source')")
            .execute(&test.pool).await?;
        let original = "key,name,street1,street2,city,state,zip,website,sources\n1,Alpha High,10 Main,,Madison,WI,53701,https://alpha.example/,alpha-directory\n1,Beta High,20 Main,,Madison,WI,53702,https://beta.example/,beta-directory\n";
        let (csv, manifest) = fixture(test, original)?;
        let args = ["scrape", "--school-directory", &csv, "--school-directory-manifest", &manifest];
        support::assert_success(&test.cli(&args).await?);
        let corrected = original
            .replace("Alpha High,10 Main", "Renamed Alpha,11 Main")
            .replace("Beta High,20 Main", "Renamed Beta,21 Main");
        fixture(test, &corrected)?;
        support::assert_success(&test.cli(&args).await?);
        let addresses: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT source_id,address,postal_code FROM schools ORDER BY source_id",
        ).fetch_all(&test.pool).await?;
        assert_eq!(addresses, vec![
            ("alpha".into(), "11 Main".into(), "53701".into()),
            ("beta".into(), "21 Main".into(), "53702".into()),
        ]);
        Ok(())
    })).await
}
