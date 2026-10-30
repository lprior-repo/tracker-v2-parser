use super::support::*;
use crate::{
    acquisition::AcquisitionPolicy, database::Database,
    durable::pacing::test_support::with_database,
};
use anyhow::Result;

#[tokio::test]
async fn preserved_local_policy_stop_is_nonfatal_only_in_public_scope_and_remains_immutable()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config()?;
            let now = chrono::Utc::now().timestamp_millis();
            let root = tempfile::tempdir()?;
            let (request, _) = preserved_stop(&database, root.path(), &config, now).await?;
            let before = checkpoint(&database, request.binding.page_id).await?;
            let attempts: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a",
            )
            .fetch_one(pool)
            .await?;
            crate::runner::durable::bootstrap(&database, &config).await?;
            assert_eq!(
                checkpoint(&database, request.binding.page_id).await?,
                before
            );
            assert_eq!(
                sqlx::query_scalar::<_, serde_json::Value>(
                    "SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a"
                )
                .fetch_one(pool)
                .await?,
                attempts
            );
            let mut complete = config.clone();
            complete.acquisition = AcquisitionPolicy::Complete;
            assert!(
                crate::runner::durable::bootstrap(&database, &complete)
                    .await
                    .is_err()
            );
            assert_eq!(
                checkpoint(&database, request.binding.page_id).await?,
                before
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                    .fetch_one(pool)
                    .await?,
                0
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_receipts")
                    .fetch_one(pool)
                    .await?,
                0
            );
            assert_eq!(before["status"], "blocked");
            assert_eq!(before["attempts"], 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn preserved_policy_shaped_stops_with_real_denial_text_or_remote_evidence_remain_provider_stopping()
-> Result<()> {
    with_database(|pool|Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let config=config()?;
        let now=chrono::Utc::now().timestamp_millis();
        let root=tempfile::tempdir()?;
        let (request,reservation)=preserved_stop(&database,root.path(),&config,now).await?;
        let exact=sqlx::query_scalar::<_,String>("SELECT last_error FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?;
        for detail in ["HTTP 403 access denied","CAPTCHA challenge","login required","subscribe to view this",&format!("{exact}; HTTP 403")] {
            sqlx::query("UPDATE scrape_pages SET last_error=$2,cursor=jsonb_set(cursor,'{public_results_exclusion}','\"public-results/v1\"') WHERE id=$1").bind(request.binding.page_id).bind(detail).execute(pool).await?;
            let before=checkpoint(&database,request.binding.page_id).await?;
            assert!(crate::runner::durable::bootstrap(&database,&config).await.is_err());
            assert_eq!(checkpoint(&database,request.binding.page_id).await?,before);
        }
        sqlx::query("UPDATE scrape_pages SET last_error=$2 WHERE id=$1").bind(request.binding.page_id).bind(&exact).execute(pool).await?;
        for field in ["capture","parsed"] {
            let sql=if field=="capture" {"UPDATE durable_page_attempts SET capture='{}'::jsonb WHERE attempt_id=$1"} else {"UPDATE durable_page_attempts SET parsed='{}'::jsonb WHERE attempt_id=$1"};
            sqlx::query(sql).bind(&reservation.attempt_id).execute(pool).await?;
            assert!(crate::runner::durable::bootstrap(&database,&config).await.is_err());
            sqlx::query("UPDATE durable_page_attempts SET capture=NULL,parsed=NULL WHERE attempt_id=$1").bind(&reservation.attempt_id).execute(pool).await?;
        }
        sqlx::query("INSERT INTO durable_wire_requests(wire_id,operation_id,attempt_id,request_sha256,occurrence,source,origin,method,state,status,finished_at) VALUES($1,$2,$3,$4,1,'milesplit','https://oh.milesplit.com','GET','headers',403,clock_timestamp())")
            .bind("c".repeat(64)).bind(&request.binding.operation_id).bind(&reservation.attempt_id).bind("d".repeat(64)).execute(pool).await?;
        let before=checkpoint(&database,request.binding.page_id).await?;
        assert!(crate::runner::durable::bootstrap(&database,&config).await.is_err());
        assert_eq!(checkpoint(&database,request.binding.page_id).await?,before);
        Ok(())
    })).await
}
