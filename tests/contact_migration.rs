mod support;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;
use track_scrapper_v2::{
    database::Database,
    model::{Capture, Coach, PageContext, PageKind, ParsedPage, School, Source, Sport, Target},
};

async fn create_populated_old_schema(test: &support::Scenario) -> Result<()> {
    let old = test.directory().join("old-migrations");
    std::fs::create_dir(&old)?;
    for entry in std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".sql")
            && name.to_string_lossy().as_ref() < "202610270005_contact_source_claims.sql"
        {
            std::fs::copy(entry.path(), old.join(name))?;
        }
    }
    sqlx::migrate::Migrator::new(old.as_path())
        .await?
        .run(&test.pool)
        .await?;
    sqlx::query("INSERT INTO schools(source,source_id,name,state,source_url) VALUES ('athleticnet','school-1','Example High School','WI','https://school.example.edu/staff')")
        .execute(&test.pool).await?;
    sqlx::query("INSERT INTO contacts(school_id,kind,name,role,email,sport,source_url,raw,effective_from) SELECT id,'coach','Ada Runner','Head Coach','STAFF@school.example.edu','outdoor','https://school.example.edu/staff','{\"school_id\":\"school-1\",\"name\":\"Ada Runner\",\"role\":\"Head Coach\",\"email\":\"STAFF@school.example.edu\",\"sport\":\"outdoor\"}'::jsonb,'2025' FROM schools")
        .execute(&test.pool).await?;
    sqlx::query("INSERT INTO contacts(school_id,kind,role,email,source_url,raw) SELECT id,'school_office','','staff@school.example.edu','https://school.example.edu/staff','{\"published\":\"office\"}'::jsonb FROM schools")
        .execute(&test.pool).await?;
    Ok(())
}

async fn replay_old_claims(database: &Database) -> Result<()> {
    let target = Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        "https://school.example.edu/staff".into(),
        PageContext {
            school_id: Some("school-1".into()),
            ..PageContext::default()
        },
    );
    let page = ParsedPage {
        schools: vec![School {
            id: "school-1".into(),
            name: "Example High School".into(),
            public_email: Some("staff@school.example.edu".into()),
            ..School::default()
        }],
        coaches: vec![Coach {
            school_id: "school-1".into(),
            name: Some("Ada Runner".into()),
            role: "Head Coach".into(),
            email: Some("staff@school.example.edu".into()),
            sport: Some(Sport::Outdoor),
            review_notes: Vec::new(),
        }],
        ..ParsedPage::default()
    };
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".into(),
        body: Vec::new(),
    };
    database.persist(None, &target, &capture, &page).await?;
    Ok(())
}

#[tokio::test]
async fn given_populated_old_schema_when_connected_and_replayed_then_rows_checksums_and_claim_identity_survive()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        create_populated_old_schema(test).await?;
        let before: Vec<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(c) FROM contacts c ORDER BY id")
            .fetch_all(&test.pool).await?;
        let checksums: Vec<(i64, Vec<u8>)> = sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&test.pool).await?;
        let database = Database::connect(&test.url).await?;
        let after: Vec<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(c) - 'source_key' FROM contacts c ORDER BY id")
            .fetch_all(&test.pool).await?;
        assert_eq!(after, before);
        let retained: Vec<(i64, Vec<u8>)> = sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations WHERE version < 202610270005 ORDER BY version")
            .fetch_all(&test.pool).await?;
        assert_eq!(retained, checksums);
        let keys: Vec<(String, String)> = sqlx::query_as("SELECT kind,source_key FROM contacts ORDER BY kind")
            .fetch_all(&test.pool).await?;
        assert_eq!(keys, vec![
            ("coach".into(), format!("{:x}", Sha256::digest(b"[\"https://school.example.edu/staff\", \"Ada Runner\", \"Head Coach\", \"outdoor\", \"staff@school.example.edu\"]"))),
            ("school_office".into(), format!("{:x}", Sha256::digest(b"[\"https://school.example.edu/staff\", null, \"\", null, \"staff@school.example.edu\"]"))),
        ]);
        replay_old_claims(&database).await?;
        replay_old_claims(&database).await?;
        let replayed: Vec<(String, String)> = sqlx::query_as("SELECT kind,source_key FROM contacts ORDER BY kind")
            .fetch_all(&test.pool).await?;
        assert_eq!(replayed, keys);
        let effective: Option<String> = sqlx::query_scalar("SELECT effective_from FROM contacts WHERE kind='coach'")
            .fetch_one(&test.pool).await?;
        assert_eq!(effective.as_deref(), Some("2025"));
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_migrated_contacts_when_email_is_null_or_malformed_then_published_identity_and_email_checks_still_apply()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            create_populated_old_schema(test).await?;
            let database = Database::connect(&test.url).await?;
            for email in ["", "not-an-email", "a@@school.edu", " coach@school.edu "] {
                let error = sqlx::query("UPDATE contacts SET email=$1 WHERE kind='coach'")
                    .bind(email)
                    .execute(&test.pool)
                    .await
                    .err()
                    .context("malformed email accepted")?;
                let error = error
                    .as_database_error()
                    .context("database constraint error")?;
                assert_eq!(error.code().as_deref(), Some("23514"));
                assert_eq!(error.constraint(), Some("contacts_email_check"));
            }
            sqlx::query("UPDATE contacts SET email=NULL WHERE kind='coach'")
                .execute(&test.pool)
                .await?;
            let named: (Option<String>, Option<String>) =
                sqlx::query_as("SELECT name,email FROM contacts WHERE kind='coach'")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(named, (Some("Ada Runner".into()), None));
            let error = sqlx::query("UPDATE contacts SET name=' ' WHERE kind='coach'")
                .execute(&test.pool)
                .await
                .err()
                .context("nameless and emailless claim accepted")?;
            let error = error
                .as_database_error()
                .context("database constraint error")?;
            assert_eq!(error.code().as_deref(), Some("23514"));
            assert_eq!(
                error.constraint(),
                Some("contacts_published_identity_check")
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 2);
            database.close().await;
            Ok(())
        })
    })
    .await
}
