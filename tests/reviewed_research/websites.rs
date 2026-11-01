mod replay;
mod validation;

use anyhow::Result;
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    durable::operations::{OperationSpec, Workspace, stage_input},
};

use crate::{
    reviewed_research::support as fixture,
    support::{Scenario, with_schema},
};

fn website(school_id: i64) -> Value {
    json!({"kind":"school_website","source":"athleticnet","source_id":"school-1",
        "expected_school_id":school_id,"expected_name":"Test High School","expected_city":"Madison","expected_state":"WI",
        "website":"https://school.example.edu/","contact_url":"https://school.example.edu/staff","season":2026,"evidence":[0]})
}

fn build(
    test: &Scenario,
    id: &str,
    change: Value,
    capture: &str,
) -> Result<(Workspace, OperationSpec)> {
    let root = test.directory().join("website-artifacts");
    let source = test.directory().join(format!("{id}.txt"));
    std::fs::write(&source, capture)?;
    let evidence = stage_input(&root, &source)?;
    let mut body = fixture::envelope(vec![change]);
    body["evidence"] = json!([{"artifact":evidence,"primary_url":"https://state.example.edu/directory","captured_at":"2026-10-30T13:00:00Z","capture_kind":"web_tool_text"}]);
    let manifest_path = test.directory().join(format!("{id}.json"));
    std::fs::write(&manifest_path, serde_json::to_vec(&body)?)?;
    let spec = OperationSpec::ReviewedResearch {
        manifest: stage_input(&root, &manifest_path)?,
    };
    let workspace = Workspace::open(&root, id, &spec)?;
    Ok((workspace, spec))
}

const CAPTURE: &str =
    "Test High School https://school.example.edu/ https://school.example.edu/staff";

#[tokio::test]
async fn given_published_school_website_when_review_replays_then_fill_audit_and_contact_checkpoint_are_atomic_and_unique()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let (workspace, spec) = build(test, "website-success", website(school_id), CAPTURE)?;
            let first = fixture::apply(test, &workspace, &spec).await?;
            assert_eq!(first["websites_filled"], 1);
            assert_eq!(first["contact_pages_enqueued"], 1);
            assert_eq!(fixture::apply(test, &workspace, &spec).await?, first);
            let row: (String, Value) =
                sqlx::query_as("SELECT website,website_provenance FROM schools WHERE id=$1")
                    .bind(school_id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(row.0, "https://school.example.edu/");
            assert_eq!(row.1["manual_research"].as_array().unwrap().len(), 1);
            assert_eq!(row.1["manual_research"][0]["reviewed_by"], "codex");
            assert_eq!(
                row.1["manual_research"][0]["contact_url"],
                "https://school.example.edu/staff"
            );
            assert_eq!(
                row.1["manual_research"][0]["evidence"][0]["artifact_sha256"]
                    .as_str()
                    .unwrap()
                    .len(),
                64
            );
            let page: (String, String, i32, Value) =
                sqlx::query_as("SELECT url,status,attempts,cursor FROM scrape_pages")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(
                (page.0.as_str(), page.1.as_str(), page.2),
                ("https://school.example.edu/staff", "pending", 0)
            );
            assert_eq!(page.3["context"]["school_id"], "school-1");
            assert_eq!(page.3["context"]["state"], "WI");
            let (workspace, spec) =
                build(test, "website-distinct-replay", website(school_id), CAPTURE)?;
            let next = fixture::apply(test, &workspace, &spec).await?;
            assert_eq!(next["websites_filled"], 0);
            assert_eq!(next["contact_pages_enqueued"], 0);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scrape_pages")
                    .fetch_one(&test.pool)
                    .await?,
                1
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_existing_website_or_stale_identity_when_review_applies_then_no_mutation_or_receipt_occurs()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let school_id = fixture::insert_school(test, "school-1").await?;
        sqlx::query("UPDATE schools SET website='https://existing.example.edu/' WHERE id=$1").bind(school_id).execute(&test.pool).await?;
        let (workspace, spec) = build(test, "website-conflict", website(school_id), CAPTURE)?;
        assert!(fixture::apply(test, &workspace, &spec).await.is_err());
        sqlx::query("UPDATE schools SET website=NULL WHERE id=$1").bind(school_id).execute(&test.pool).await?;
        for (index, field, value) in [(0,"expected_name",json!("Another School")),(1,"expected_city",json!("Elsewhere")),(2,"expected_school_id",json!(school_id+1)),(3,"source_id",json!("missing")),(4,"expected_state",json!("MN"))] {
            let mut change = website(school_id); change[field] = value;
            let capture = format!("Another School {CAPTURE}");
            let (workspace, spec) = build(test, &format!("website-stale-{index}"), change, &capture)?;
            assert!(fixture::apply(test, &workspace, &spec).await.is_err());
        }
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scrape_pages").fetch_one(&test.pool).await?, 0);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts").fetch_one(&test.pool).await?, 0);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM schools WHERE website_provenance IS NOT NULL OR website IS NOT NULL").fetch_one(&test.pool).await?, 0);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_invalid_or_unsupported_publication_when_review_prepares_then_no_website_is_imported()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            for (index, url) in [
                "http://school.example.edu/",
                "https://127.0.0.1/",
                "https://localhost/",
                "https://foo.local/",
                "https://user:secret@school.example.edu/",
                "https://school.example.edu/?token=secret",
                "https://school.example.edu:8443/",
                "https://school.example.edu/#fragment",
            ]
            .iter()
            .enumerate()
            {
                let mut change = website(school_id);
                change["website"] = json!(url);
                change["contact_url"] = json!(url);
                let capture = format!("Test High School {url}");
                let (workspace, spec) =
                    build(test, &format!("website-invalid-{index}"), change, &capture)?;
                assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            }
            for (index, capture) in [
                "Unrelated School https://school.example.edu/ https://school.example.edu/staff",
                "Test High School https://other.example.edu/",
                "Test High School https://school.example.edu/",
            ]
            .iter()
            .enumerate()
            {
                let (workspace, spec) = build(
                    test,
                    &format!("website-evidence-{index}"),
                    website(school_id),
                    capture,
                )?;
                assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            }
            let mut change = website(school_id);
            change["contact_url"] = json!("https://unrelated.example.edu/staff");
            let (workspace, spec) = build(
                test,
                "website-other-origin",
                change,
                &format!("{CAPTURE} https://unrelated.example.edu/staff"),
            )?;
            assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts")
                    .fetch_one(&test.pool)
                    .await?,
                0
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_saved_contact_stop_when_website_import_enqueues_then_stop_and_consumed_attempts_are_preserved()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let school_id = fixture::insert_school(test, "school-1").await?;
        let database = Database::connect(&test.url).await?;
        let target = track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet, track_scrapper_v2::model::PageKind::Contacts, "https://school.example.edu/staff".to_owned(), track_scrapper_v2::model::PageContext {school_id:Some("school-1".to_owned()),school_name:Some("Test High School".to_owned()),state:Some("WI".to_owned()),season:Some(2025),subject_id:Some("contacts-depth:0".to_owned()),..Default::default()});
        database.enqueue(&[target], false).await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=3,last_error='HTTP 403 retained stop',notes=ARRAY['original']").execute(&test.pool).await?;
        let before: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p").fetch_one(&test.pool).await?;
        let (workspace, spec) = build(test, "website-stop", website(school_id), CAPTURE)?;
        let report = fixture::apply(test, &workspace, &spec).await?;
        assert_eq!(report["contact_pages_enqueued"], 0);
        let after: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p").fetch_one(&test.pool).await?;
        assert_eq!(after, before);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts").fetch_one(&test.pool).await?, 0);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_saved_origin_stop_when_new_published_contact_is_queued_then_it_inherits_stop_without_wire_attempt()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let school_id = fixture::insert_school(test, "school-1").await?;
        let database = Database::connect(&test.url).await?;
        let target = track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet, track_scrapper_v2::model::PageKind::Contacts, "https://school.example.edu/blocked".to_owned(), track_scrapper_v2::model::PageContext {school_id:Some("another-school".to_owned()),school_name:Some("Other High School".to_owned()),state:Some("WI".to_owned()),season:Some(2025),subject_id:Some("contacts-depth:0".to_owned()),..Default::default()});
        database.enqueue(&[target], false).await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=3,last_error='HTTP 403 retained stop',notes=ARRAY['original']").execute(&test.pool).await?;
        let before: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p").fetch_one(&test.pool).await?;
        let (workspace, spec) = build(test, "website-origin-stop", website(school_id), CAPTURE)?;
        assert_eq!(fixture::apply(test, &workspace, &spec).await?["contact_pages_enqueued"], 1);
        let existing: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE status='blocked'").fetch_one(&test.pool).await?;
        assert_eq!(existing, before);
        let page: (i64, i32, Vec<String>) = sqlx::query_as("SELECT id,attempts,notes FROM scrape_pages WHERE url='https://school.example.edu/staff'").fetch_one(&test.pool).await?;
        assert_eq!(page.1, 0);
        assert!(page.2.iter().any(|note| note.contains("HTTP 403 retained stop")));
        assert!(!database.start_attempt(page.0, 3).await?);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts").fetch_one(&test.pool).await?, 0);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_duplicate_website_changes_when_manifest_prepares_then_it_rejects_the_entire_manifest()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let (workspace, OperationSpec::ReviewedResearch { manifest }) =
                build(test, "website-duplicate-input", website(school_id), CAPTURE)?
            else {
                unreachable!()
            };
            let mut body: Value = serde_json::from_slice(&workspace.read(&manifest)?)?;
            body["changes"] = json!([website(school_id), website(school_id)]);
            let path = test.directory().join("website-duplicate.json");
            std::fs::write(&path, serde_json::to_vec(&body)?)?;
            let root = test.directory().join("website-artifacts");
            let spec = OperationSpec::ReviewedResearch {
                manifest: stage_input(&root, &path)?,
            };
            let workspace = Workspace::open(&root, "website-duplicate", &spec)?;
            assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts")
                    .fetch_one(&test.pool)
                    .await?,
                0
            );
            Ok(())
        })
    })
    .await
}
