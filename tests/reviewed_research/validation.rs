use anyhow::Result;
use serde_json::json;

use crate::{reviewed_research::support as fixture, support::with_schema};

#[tokio::test]
async fn given_tampered_evidence_when_import_prepares_then_it_rejects_before_writing_receipt()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let body = fixture::envelope(vec![fixture::address("school-1", school_id)]);
            let (workspace, spec, evidence) = fixture::build(test, "research-tampered", body)?;
            std::fs::write(&evidence[0].path, b"changed capture")?;
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
async fn given_duplicate_changes_when_import_prepares_then_entire_manifest_is_rejected()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let body = fixture::envelope(vec![
                fixture::address("school-1", school_id),
                fixture::address("school-1", school_id),
            ]);
            let (workspace, spec, _) = fixture::build(test, "research-duplicate", body)?;
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
async fn given_non_codex_reviewer_when_manifest_is_loaded_then_ai_cannot_claim_user_review()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let mut body = fixture::envelope(vec![fixture::address("school-1", school_id)]);
            body["reviewed_by"] = json!("user");
            let (workspace, spec, _) = fixture::build(test, "research-reviewer", body)?;
            assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM schools WHERE address IS NOT NULL"
                )
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
async fn given_source_name_or_city_mismatch_when_import_runs_then_target_is_rejected() -> Result<()>
{
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let mut source = fixture::address("school-1", school_id);
            source["source"] = json!("milesplit");
            let mut name = fixture::address("school-1", school_id);
            name["expected_name"] = json!("Different School");
            let mut city = fixture::address("school-1", school_id);
            city["expected_city"] = json!("Monona");
            for (operation, change) in [
                ("research-source", source),
                ("research-name", name),
                ("research-city", city),
            ] {
                let body = fixture::envelope(vec![change]);
                let (workspace, spec, _) = fixture::build(test, operation, body)?;
                assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            }
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM schools WHERE address IS NOT NULL"
                )
                .fetch_one(&test.pool)
                .await?,
                0
            );
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
async fn given_unexpected_manifest_fields_when_import_prepares_then_schema_rejects_them()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let mut body = fixture::envelope(vec![fixture::address("school-1", school_id)]);
            body["identity_decision"] = json!("merge");
            let (workspace, spec, _) = fixture::build(test, "research-unknown", body)?;
            assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            Ok(())
        })
    })
    .await
}
