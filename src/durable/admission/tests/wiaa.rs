use anyhow::Result;

use crate::{database::Database, durable::pacing::test_support::with_database};

#[tokio::test]
async fn given_another_scraper_owns_lock_when_wiaa_admission_is_requested_then_it_fails_without_wire_or_checkpoint_changes()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let mut connection = pool.acquire().await?;
            sqlx::query("SELECT pg_advisory_lock(hashtext(current_schema()),194832741)")
                .execute(&mut *connection)
                .await?;
            for slots in [1, 3] {
                let error = super::super::acquire(&database, slots, false)
                    .await
                    .expect_err("another scraper owns acquisition");
                assert!(error.is::<crate::durable::AcquisitionBusy>());
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scrape_pages")
                        .fetch_one(pool)
                        .await?,
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                        .fetch_one(pool)
                        .await?,
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_operations")
                        .fetch_one(pool)
                        .await?,
                    0
                );
            }
            let released: bool = sqlx::query_scalar(
                "SELECT pg_advisory_unlock(hashtext(current_schema()),194832741)",
            )
            .fetch_one(&mut *connection)
            .await?;
            assert!(released);
            drop(connection);
            let available = super::super::acquire(&database, 1, false).await?;
            available.rollback().await?;
            Ok(())
        })
    })
    .await
}
