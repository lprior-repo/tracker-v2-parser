use anyhow::Result;

use crate::{reviewed_research::support as fixture, support::with_schema};

#[tokio::test]
async fn given_valid_change_before_missing_target_when_batch_fails_then_all_database_effects_roll_back()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let missing = fixture::address("missing-school", school_id + 1);
            let body = fixture::envelope(vec![fixture::address("school-1", school_id), missing]);
            let (workspace, spec, _) = fixture::build(test, "research-rollback", body)?;
            assert!(fixture::apply(test, &workspace, &spec).await.is_err());
            let address: Option<String> =
                sqlx::query_scalar("SELECT address FROM schools WHERE id=$1")
                    .bind(school_id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(address, None);
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
