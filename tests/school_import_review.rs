#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use serde_json::Value;
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    school_directory::{pss, published},
};

#[derive(Clone, Copy)]
enum Feed {
    Published,
    Private,
}

impl Feed {
    fn id(self) -> &'static str {
        match self {
            Self::Published => "010000500870",
            Self::Private => "pss:02004116",
        }
    }

    fn csv(self, stage: usize) -> String {
        let (name, address, city, state, zip, latitude) = match stage {
            0 => (
                "Example School",
                "10 Main",
                "Madison",
                "WI",
                "00123",
                "43.1",
            ),
            1 => ("Example School", "", "", "", "", ""),
            5 => (
                "Example School",
                "10 Main",
                "New York",
                "WI",
                "00123",
                "43.1",
            ),
            2 => (
                "Corrected School",
                "20 Main",
                "Monona",
                "WI",
                "00456",
                "43.2",
            ),
            4 => (
                "Corrected School",
                "20 Main",
                "Monona",
                "WI",
                "00456",
                "43.9",
            ),
            _ => (
                "Corrected School",
                "30 Main",
                "Monona",
                "WI",
                "00789",
                "43.3",
            ),
        };
        match self {
            Self::Published => format!(
                "unit_id,name,address,city,state,zip_code,latitude,longitude,source,validated,geocode_status\n010000500870,{name},{address},{city},{state},{zip},{latitude},,NCES_CSV,,\n"
            ),
            Self::Private => format!(
                "PPIN,PINST,PADDRS,PCITY,PSTABB,PZIP,PZIP4,PPHONE,NUMSTUDS,LATITUDE24\n02004116,{name},{address},{city},{state},{zip},,,,{latitude}\n"
            ),
        }
    }
}

async fn import(
    test: &support::Scenario,
    feed: Feed,
    stage: usize,
) -> Result<(u64, u64, u64, u64, u64, String)> {
    let content = feed.csv(stage);
    let path = test.directory().join("schools.csv");
    std::fs::write(&path, &content)?;
    let sha = format!("{:x}", Sha256::digest(content.as_bytes()));
    let counts = match feed {
        Feed::Published => {
            let directory = published::PublishedDirectory::load(&path, &sha)?;
            let report = published::import(&test.pool, &directory).await?;
            (
                report.inserted,
                report.updated,
                report.conflicts,
                report.enrichment.updated,
                report.enrichment.conflicts,
            )
        }
        Feed::Private => {
            let directory = pss::PrivateDirectory::load(&path, &sha)?;
            let report = pss::import(&test.pool, &directory).await?;
            (
                report.inserted,
                report.updated,
                report.conflicts,
                report.enrichment.updated,
                report.enrichment.conflicts,
            )
        }
    };
    Ok((counts.0, counts.1, counts.2, counts.3, counts.4, sha))
}

async fn row(test: &support::Scenario, id: &str) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT to_jsonb(s) - 'last_checked' FROM schools s WHERE source='nces' AND source_id=$1",
    )
    .bind(id)
    .fetch_one(&test.pool)
    .await?)
}

async fn original_sparse_corrected_repeat(test: &support::Scenario, feed: Feed) -> Result<()> {
    Database::connect(&test.url).await?.close().await;
    let original_counts = import(test, feed, 0).await?;
    assert_eq!(
        (original_counts.0, original_counts.1, original_counts.2),
        (1, 0, 0)
    );
    let original = row(test, feed.id()).await?;
    assert_eq!(original["source_id"], feed.id());
    assert_eq!(original["postal_code"], "00123");
    assert_eq!(
        original.pointer("/address_provenance/address/artifact_sha256"),
        Some(&Value::String(original_counts.5))
    );
    let sparse_counts = import(test, feed, 1).await?;
    assert_eq!(
        (sparse_counts.0, sparse_counts.1, sparse_counts.2),
        (0, 0, 0)
    );
    assert_eq!(row(test, feed.id()).await?, original);
    let corrected_counts = import(test, feed, 2).await?;
    assert_eq!(
        (corrected_counts.0, corrected_counts.1, corrected_counts.2),
        (0, 1, 0)
    );
    let corrected = row(test, feed.id()).await?;
    assert_eq!(corrected["uuid"], original["uuid"]);
    assert_eq!(corrected["id"], original["id"]);
    assert_eq!(corrected["name"], "Corrected School");
    assert_eq!(corrected["city"], "Monona");
    assert_eq!(corrected["state"], "WI");
    assert_eq!(corrected["address"], "20 Main");
    assert_eq!(corrected["postal_code"], "00456");
    assert_eq!(
        corrected.pointer("/address_provenance/address/artifact_sha256"),
        Some(&Value::String(corrected_counts.5.clone()))
    );
    assert_eq!(
        corrected.pointer("/address_provenance/record/row_details/latitude"),
        Some(&Value::String("43.2".into()))
    );
    let repeated = import(test, feed, 2).await?;
    assert_eq!((repeated.0, repeated.1, repeated.2), (0, 0, 0));
    assert_eq!(row(test, feed.id()).await?, corrected);
    sqlx::query("UPDATE schools SET address='Manual address', website='https://manual.example', public_email='office@manual.example' WHERE source='nces' AND source_id=$1")
        .bind(feed.id()).execute(&test.pool).await?;
    let conflict = import(test, feed, 3).await?;
    assert_eq!((conflict.0, conflict.1, conflict.2), (0, 1, 1));
    let manual = row(test, feed.id()).await?;
    assert_eq!(manual["address"], "Manual address");
    assert_eq!(manual["postal_code"], "00456");
    assert_eq!(
        manual.pointer("/address_provenance/postal_conflict/incoming/postal_code"),
        Some(&Value::String("00789".into()))
    );
    assert_eq!(manual["website"], "https://manual.example");
    assert_eq!(manual["public_email"], "office@manual.example");
    assert_eq!(
        manual.pointer("/address_provenance/address"),
        corrected.pointer("/address_provenance/address")
    );
    let again = import(test, feed, 3).await?;
    assert_eq!((again.0, again.1, again.2), (0, 0, 1));
    assert_eq!(row(test, feed.id()).await?, manual);
    Ok(())
}

#[tokio::test]
async fn given_published_import_when_sparse_corrected_and_repeated_then_owned_fields_update()
-> Result<()> {
    support::with_schema(|test| Box::pin(original_sparse_corrected_repeat(test, Feed::Published)))
        .await
}

#[tokio::test]
async fn given_private_import_when_sparse_corrected_and_repeated_then_owned_fields_update()
-> Result<()> {
    support::with_schema(|test| Box::pin(original_sparse_corrected_repeat(test, Feed::Private)))
        .await
}

async fn enriched_correction(test: &support::Scenario, feed: Feed) -> Result<()> {
    Database::connect(&test.url).await?.close().await;
    sqlx::query("INSERT INTO schools(source,source_id,name,state,city,address,source_url) VALUES ('athleticnet','manual','Example School','WI','Madison','10 Main','source')")
        .execute(&test.pool).await?;
    let original = import(test, feed, 0).await?;
    assert_eq!((original.3, original.4), (1, 0));
    let corrected = import(test, feed, 2).await?;
    assert_eq!((corrected.3, corrected.4), (1, 1));
    let school: (String, String, String, String, Value) = sqlx::query_as("SELECT name,address,postal_code,source_url,address_provenance FROM schools WHERE source='athleticnet'")
        .fetch_one(&test.pool).await?;
    assert_eq!(
        (&*school.0, &*school.1, &*school.2, &*school.3),
        ("Example School", "10 Main", "00123", "source")
    );
    assert_eq!(
        school
            .4
            .pointer("/postal_conflict/source/directory/artifact_sha256"),
        Some(&Value::String(corrected.5))
    );
    let repeated = import(test, feed, 2).await?;
    assert_eq!((repeated.3, repeated.4), (0, 1));
    let database = Database::connect(&test.url).await?;
    let mut athlete = records::athlete("postal-review");
    athlete.school_id = Some("manual".into());
    let target = records::target(
        track_scrapper_v2::model::Source::AthleticNet,
        "postal-review",
    );
    records::persist(
        &database,
        &target,
        &track_scrapper_v2::model::ParsedPage {
            athletes: vec![athlete],
            ..Default::default()
        },
    )
    .await?;
    database.close().await;
    let output = test.directory().join("postal.xlsx");
    support::assert_success(
        &test
            .operation(&crate::support::OfflineOperation::Export {
                config: None,
                output: std::path::PathBuf::from(
                    (output.to_str().context("UTF-8 output")?).to_string(),
                ),
                per_athlete: false,
            })
            .await?,
    );
    let mut workbook = open_workbook_auto(&output)?;
    let sheet = workbook.worksheet_range("Athletes")?;
    let mut rows = sheet.rows();
    let headers = rows.next().context("headers")?;
    let values = rows.next().context("athlete row")?;
    let address = headers
        .iter()
        .zip(values)
        .find(|(header, _)| header.to_string() == "school_postal_addresses")
        .context("postal column")?
        .1
        .to_string();
    assert!(address.contains("10 Main"));
    assert!(address.contains("Madison"));
    assert!(address.contains("00123"));
    assert!(!address.contains("00456"));
    sqlx::query("UPDATE schools SET postal_code='manual zip' WHERE source='athleticnet'")
        .execute(&test.pool)
        .await?;
    let manual = import(test, feed, 3).await?;
    assert_eq!((manual.3, manual.4), (1, 1));
    let postal: String =
        sqlx::query_scalar("SELECT postal_code FROM schools WHERE source='athleticnet'")
            .fetch_one(&test.pool)
            .await?;
    assert_eq!(postal, "manual zip");
    Ok(())
}

#[tokio::test]
async fn given_other_source_when_published_correction_then_only_import_owned_fields_change()
-> Result<()> {
    support::with_schema(|test| Box::pin(enriched_correction(test, Feed::Published))).await
}

#[tokio::test]
async fn given_other_source_when_private_correction_then_only_import_owned_fields_change()
-> Result<()> {
    support::with_schema(|test| Box::pin(enriched_correction(test, Feed::Private))).await
}

#[test]
fn given_oversized_import_when_loaded_then_rejected_before_materializing_csv() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("oversized.csv");
    let file = std::fs::File::create(&path)?;
    file.set_len(134_217_729)?;
    assert!(
        published::PublishedDirectory::load(&path, "irrelevant")
            .err()
            .is_some_and(|error| format!("{error:#}").contains("size limit"))
    );
    assert!(
        pss::PrivateDirectory::load(&path, "irrelevant")
            .err()
            .is_some_and(|error| format!("{error:#}").contains("size limit"))
    );
    Ok(())
}

async fn detail_correction(test: &support::Scenario, feed: Feed) -> Result<()> {
    Database::connect(&test.url).await?.close().await;
    import(test, feed, 2).await?;
    let before = row(test, feed.id()).await?;
    let changed = import(test, feed, 4).await?;
    assert_eq!((changed.0, changed.1, changed.2), (0, 1, 0));
    let after = row(test, feed.id()).await?;
    assert_eq!(after["uuid"], before["uuid"]);
    assert_eq!(after["address"], before["address"]);
    assert_eq!(
        after.pointer("/address_provenance/record/row_details/latitude"),
        Some(&Value::String("43.9".into()))
    );
    assert_eq!(
        after.pointer("/address_provenance/record/artifact_sha256"),
        Some(&Value::String(changed.5))
    );
    let repeated = import(test, feed, 4).await?;
    assert_eq!((repeated.0, repeated.1, repeated.2), (0, 0, 0));
    assert_eq!(row(test, feed.id()).await?, after);
    Ok(())
}

#[tokio::test]
async fn given_published_details_when_corrected_then_provenance_updates_once() -> Result<()> {
    support::with_schema(|test| Box::pin(detail_correction(test, Feed::Published))).await
}

#[tokio::test]
async fn given_private_details_when_corrected_then_provenance_updates_once() -> Result<()> {
    support::with_schema(|test| Box::pin(detail_correction(test, Feed::Private))).await
}

async fn unverifiable_legacy(test: &support::Scenario, feed: Feed) -> Result<()> {
    Database::connect(&test.url).await?.close().await;
    import(test, feed, 0).await?;
    sqlx::query("UPDATE schools SET address_provenance=(SELECT jsonb_object_agg(key,value-'owner'-'value') FROM jsonb_each(address_provenance)) WHERE source='nces'")
        .execute(&test.pool).await?;
    let before = row(test, feed.id()).await?;
    let corrected = import(test, feed, 2).await?;
    assert_eq!((corrected.0, corrected.1, corrected.2), (0, 1, 1));
    let after = row(test, feed.id()).await?;
    for field in ["name", "state", "city", "address", "postal_code"] {
        assert_eq!(after[field], before[field]);
    }
    assert_eq!(
        after.pointer("/address_provenance/postal_conflict/incoming/postal_code"),
        Some(&Value::String("00456".into()))
    );
    let repeated = import(test, feed, 2).await?;
    assert_eq!((repeated.0, repeated.1, repeated.2), (0, 0, 1));
    assert_eq!(row(test, feed.id()).await?, after);
    Ok(())
}

#[tokio::test]
async fn given_unverifiable_published_ownership_when_corrected_then_conflict_preserves_values()
-> Result<()> {
    support::with_schema(|test| Box::pin(unverifiable_legacy(test, Feed::Published))).await
}

#[tokio::test]
async fn given_unverifiable_private_ownership_when_corrected_then_conflict_preserves_values()
-> Result<()> {
    support::with_schema(|test| Box::pin(unverifiable_legacy(test, Feed::Private))).await
}

async fn equivalent_location(test: &support::Scenario, feed: Feed) -> Result<()> {
    Database::connect(&test.url).await?.close().await;
    sqlx::query("INSERT INTO schools(source,source_id,name,state,city,address,source_url) VALUES ('athleticnet','equivalent','Example School','wi','NEW  YORK','10 Main','source')").execute(&test.pool).await?;
    let first = import(test, feed, 5).await?;
    assert_eq!((first.3, first.4), (1, 0));
    let location: (String, String, String, String) = sqlx::query_as(
        "SELECT state,city,address,postal_code FROM schools WHERE source='athleticnet'",
    )
    .fetch_one(&test.pool)
    .await?;
    assert_eq!(
        location,
        (
            "wi".into(),
            "NEW  YORK".into(),
            "10 Main".into(),
            "00123".into()
        )
    );
    let repeat = import(test, feed, 5).await?;
    assert_eq!((repeat.3, repeat.4), (0, 0));
    let changed = import(test, feed, 2).await?;
    assert_eq!((changed.3, changed.4), (1, 1));
    let retained: (String, String, String, String) = sqlx::query_as(
        "SELECT state,city,address,postal_code FROM schools WHERE source='athleticnet'",
    )
    .fetch_one(&test.pool)
    .await?;
    assert_eq!(retained, location);
    Ok(())
}

#[tokio::test]
async fn given_equivalent_public_location_when_enriched_then_zip_fills_without_rewriting_spelling()
-> Result<()> {
    support::with_schema(|test| Box::pin(equivalent_location(test, Feed::Published))).await
}

#[tokio::test]
async fn given_equivalent_private_location_when_enriched_then_zip_fills_without_rewriting_spelling()
-> Result<()> {
    support::with_schema(|test| Box::pin(equivalent_location(test, Feed::Private))).await
}
