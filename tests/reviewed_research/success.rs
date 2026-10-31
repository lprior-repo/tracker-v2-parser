use anyhow::Result;
use serde_json::{Value, json};

use crate::{reviewed_research::support as fixture, support::with_schema};

#[tokio::test]
async fn given_reviewed_address_and_coach_when_operation_replays_then_changes_and_receipt_are_unique()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let body = fixture::envelope(vec![
                fixture::address("school-1", school_id),
                fixture::coach(school_id),
            ]);
            let (workspace, spec, evidence) = fixture::build(test, "research-success", body)?;
            let first = fixture::apply(test, &workspace, &spec).await?;
            assert_eq!(first["addresses_filled"], 1);
            assert_eq!(first["coaches_inserted"], 1);
            let school: (Option<String>, Option<String>, Value) = sqlx::query_as(
                "SELECT address,postal_code,address_provenance FROM schools WHERE id=$1",
            )
            .bind(school_id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(school.0.as_deref(), Some("2 Oak Street"));
            assert_eq!(school.1.as_deref(), Some("53703"));
            assert_eq!(
                school.2.pointer("/manual_research/0/reviewed_by"),
                Some(&json!("codex"))
            );
            assert_eq!(
                school.2.pointer("/manual_research/0/researcher"),
                Some(&json!("gpt-6-luna"))
            );
            assert_eq!(
                school
                    .2
                    .pointer("/manual_research/0/evidence/0/artifact_sha256"),
                Some(&json!(evidence[0].sha256))
            );
            assert_eq!(
                school
                    .2
                    .pointer("/manual_research/0/evidence/0/artifact_path"),
                Some(&json!(evidence[0].path))
            );
            let contact: (String, String, String, Value) =
                sqlx::query_as("SELECT name,role,sport,raw FROM contacts WHERE school_id=$1")
                    .bind(school_id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(
                (contact.0.as_str(), contact.1.as_str(), contact.2.as_str()),
                ("Jordan Coach", "Head Girls Track Coach", "outdoor")
            );
            assert_eq!(
                contact.3.pointer("/manual_research/0/reviewed_by"),
                Some(&json!("codex"))
            );
            assert_eq!(
                contact.3.pointer("/manual_research/0/gender"),
                Some(&json!("girls"))
            );
            let replay = fixture::apply(test, &workspace, &spec).await?;
            assert_eq!(replay, first);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM contacts WHERE school_id=$1")
                    .bind(school_id)
                    .fetch_one(&test.pool)
                    .await?,
                1
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts")
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
async fn given_populated_address_when_research_conflicts_then_existing_fields_remain_unchanged()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            sqlx::query(
                "UPDATE schools SET address='9 Pine Street',postal_code='53704' WHERE id=$1",
            )
            .bind(school_id)
            .execute(&test.pool)
            .await?;
            let body = fixture::envelope(vec![fixture::address("school-1", school_id)]);
            let (workspace, spec, _) = fixture::build(test, "research-populated", body)?;
            assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            let row: (String, String) =
                sqlx::query_as("SELECT address,postal_code FROM schools WHERE id=$1")
                    .bind(school_id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(row, ("9 Pine Street".to_owned(), "53704".to_owned()));
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
