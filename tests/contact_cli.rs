#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use std::collections::BTreeMap;
use track_scrapper_v2::{
    database::Database,
    model::{ParsedPage, School, Source},
};

#[tokio::test]
async fn given_published_named_coach_without_mailbox_when_cli_replayed_twice_then_xlsx_retains_name_and_blank_email()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let mut athlete = records::athlete("11");
        athlete.school_id = Some("school-1".into());
        records::persist(&database, &records::target(Source::AthleticNet, "11"), &ParsedPage {
            schools: vec![School {
                id: "school-1".into(), name: "Example High School".into(), state: Some("WI".into()),
                ..School::default()
            }],
            athletes: vec![athlete],
            ..ParsedPage::default()
        }).await?;
        let fixture = support::fixture("contacts/named-no-email.html");
        let args = ["scrape", "--source", "athleticnet", "--kind", "contacts", "--url",
            "https://school.example.edu/staff", "--input", fixture.to_str().context("UTF-8 fixture")?,
            "--school-id", "school-1", "--school-name", "Example High School", "--state", "WI"];
        support::assert_success(&test.cli(&args).await?);
        support::assert_success(&test.cli(&args).await?);
        let contacts: Vec<(String, Option<String>, String, String, String)> = sqlx::query_as(
            "SELECT name,email,role,sport,source_url FROM contacts WHERE kind='coach' ORDER BY name",
        ).fetch_all(&test.pool).await?;
        assert_eq!(contacts, vec![("Ada Unpublished".into(), None,
            "Head Outdoor Track Coach".into(), "outdoor".into(), "https://school.example.edu/staff".into())]);
        let raw: serde_json::Value = sqlx::query_scalar("SELECT raw FROM contacts WHERE kind='coach'")
            .fetch_one(&test.pool).await?;
        assert_eq!(raw["name"], "Ada Unpublished");
        assert_eq!(raw["email"], serde_json::Value::Null);
        assert_eq!(raw["role"], "Head Outdoor Track Coach");
        assert_eq!(raw["sport"], "outdoor");
        let output = test.directory().join("cli-contacts.xlsx");
        support::assert_success(&test.cli(&["export", "--output", output.to_str().context("UTF-8 output")?]).await?);
        let mut workbook = open_workbook_auto(&output)?;
        let sheet = workbook.worksheet_range("Athletes")?;
        assert_eq!(sheet.height(), 2);
        let mut rows = sheet.rows();
        let headers = rows.next().context("athlete headers")?;
        let values = rows.next().context("athlete row")?;
        let row: BTreeMap<String, String> = headers.iter().zip(values)
            .map(|(header, value)| (header.to_string(), value.to_string())).collect();
        assert_eq!(row["coach_names"], "Ada Unpublished");
        assert_eq!(row["coach_emails"], "");
        assert_eq!(row["needs_review"], "true");
        assert!(row["review_notes"].contains("missing public coach email"));
        let issues = workbook.worksheet_range("Issues")?;
        assert!(issues.rows().skip(1).any(|row| row.iter().any(|cell| cell.to_string().contains("missing public coach email"))));
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_published_mailbox_changes_when_cli_replayed_then_old_claim_remains_on_sheet_flagged_and_new_claim_is_current()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let mut athlete = records::athlete("11");
        athlete.school_id = Some("school-1".into());
        records::persist(&database, &records::target(Source::AthleticNet, "11"), &ParsedPage {
            schools: vec![School {
                id: "school-1".into(), name: "Example High School".into(), state: Some("WI".into()),
                ..School::default()
            }],
            athletes: vec![athlete],
            ..ParsedPage::default()
        }).await?;
        let input = test.directory().join("changed-contacts.html");
        let template = std::fs::read_to_string(support::fixture("contacts/named-no-email.html"))?;
        let html = |email: &str| template.replace("</article>", &format!("<a href=\"mailto:{email}\">{email}</a></article>"));
        let args = ["scrape", "--source", "athleticnet", "--kind", "contacts", "--url",
            "https://school.example.edu/staff", "--input", input.to_str().context("UTF-8 input")?,
            "--school-id", "school-1", "--school-name", "Example High School", "--state", "WI"];
        std::fs::write(&input, html("old@school.example.edu"))?;
        support::assert_success(&test.cli(&args).await?);
        let published_notes: serde_json::Value = sqlx::query_scalar(
            "SELECT COALESCE(raw->'review_notes','[]'::jsonb) FROM contacts WHERE kind='coach'",
        ).fetch_one(&test.pool).await?;
        std::fs::write(&input, html("new@school.example.edu"))?;
        support::assert_success(&test.cli(&args).await?);
        let rows: Vec<(i64, String, String, serde_json::Value)> = sqlx::query_as(
            "SELECT id,source_key,email,COALESCE(raw->'review_notes','[]'::jsonb) FROM contacts WHERE kind='coach' ORDER BY id",
        ).fetch_all(&test.pool).await?;
        let earlier = "Earlier contact source claim; not confirmed by latest parse";
        assert_eq!(rows.len(), 2);
        let mut earlier_notes = published_notes.as_array().context("published review annotations")?.clone();
        earlier_notes.push(serde_json::Value::String(earlier.into()));
        let earlier_notes = serde_json::Value::Array(earlier_notes);
        assert_eq!((&rows[0].2, &rows[0].3), (&"old@school.example.edu".into(), &earlier_notes));
        assert_eq!((&rows[1].2, &rows[1].3), (&"new@school.example.edu".into(), &published_notes));
        let output = test.directory().join("changed-contacts.xlsx");
        support::assert_success(&test.cli(&["export", "--output", output.to_str().context("UTF-8 output")?]).await?);
        let mut workbook = open_workbook_auto(&output)?;
        let sheet = workbook.worksheet_range("Athletes")?;
        assert_eq!(sheet.height(), 2);
        let mut worksheet_rows = sheet.rows();
        let headers = worksheet_rows.next().context("athlete headers")?;
        let values = worksheet_rows.next().context("athlete row")?;
        let row: BTreeMap<String, String> = headers.iter().zip(values)
            .map(|(header, value)| (header.to_string(), value.to_string())).collect();
        assert_eq!(row["coach_names"], "Ada Unpublished");
        assert_eq!(row["coach_emails"], "new@school.example.edu; old@school.example.edu");
        assert_eq!(row["needs_review"], "true");
        assert!(row["review_notes"].contains("Ada Unpublished <old@school.example.edu>"));
        assert!(row["review_notes"].contains(earlier));
        std::fs::write(&input, "<html><main><h1>Example High School</h1></main></html>")?;
        support::assert_success(&test.cli(&args).await?);
        let empty: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT raw->'review_notes' FROM contacts WHERE kind='coach' ORDER BY id",
        ).fetch_all(&test.pool).await?;
        assert_eq!(empty, vec![earlier_notes.clone(), earlier_notes]);
        std::fs::write(&input, html("new@school.example.edu"))?;
        support::assert_success(&test.cli(&args).await?);
        let restored: Vec<(i64, String, String, serde_json::Value)> = sqlx::query_as(
            "SELECT id,source_key,email,COALESCE(raw->'review_notes','[]'::jsonb) FROM contacts WHERE kind='coach' ORDER BY id",
        ).fetch_all(&test.pool).await?;
        assert_eq!(restored, rows);
        database.close().await;
        Ok(())
    })).await
}
