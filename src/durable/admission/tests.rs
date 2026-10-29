use anyhow::Result;
use sqlx::postgres::PgPoolOptions;

use super::acquire;
use crate::{database::Database, durable::pacing::test_support::with_database};

#[path = "tests/wiaa.rs"]
mod wiaa;

#[tokio::test]
async fn given_three_slots_when_admitting_more_then_the_fourth_and_exclusive_owners_wait_until_release()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let scoped=PgPoolOptions::new().max_connections(8).connect_with(pool.connect_options().as_ref().clone()).await?;
        let database=Database::durable_test_database(scoped);
        let first=acquire(&database,3,false).await?;
        let second=acquire(&database,3,false).await?;
        let third=acquire(&database,3,false).await?;
        assert!(acquire(&database,3,false).await.expect_err("fourth slot").is::<crate::durable::AcquisitionBusy>());
        assert!(acquire(&database,1,false).await.expect_err("exclusive owner").is::<crate::durable::AcquisitionBusy>());
        first.rollback().await?;
        let replacement=acquire(&database,3,false).await?;
        replacement.rollback().await?; second.rollback().await?; third.rollback().await?;
        let exclusive=acquire(&database,1,false).await?;
        assert!(acquire(&database,3,false).await.expect_err("shared owner").is::<crate::durable::AcquisitionBusy>());
        exclusive.rollback().await?;
        let released:i64=sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND classid=hashtext(current_schema())::oid").fetch_one(database.pool()).await?;
        assert_eq!(released,0);
        database.close().await; Ok(())
    })).await
}

#[tokio::test]
async fn given_a_browser_slot_when_another_browser_arrives_then_http_can_progress_and_cancellation_releases_the_slot()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let scoped = PgPoolOptions::new()
                .max_connections(8)
                .connect_with(pool.connect_options().as_ref().clone())
                .await?;
            let database = Database::durable_test_database(scoped);
            let browser = acquire(&database, 3, true).await?;
            assert!(
                acquire(&database, 3, true)
                    .await
                    .expect_err("one browser at a time")
                    .is::<crate::durable::AcquisitionBusy>()
            );
            let http = acquire(&database, 3, false).await?;
            drop(browser);
            let replacement = acquire(&database, 3, true).await?;
            replacement.rollback().await?;
            http.rollback().await?;
            let exclusive = acquire(&database, 1, false).await?;
            exclusive.rollback().await?;
            assert!(acquire(&database, 4, false).await.is_err());
            database.close().await;
            Ok(())
        })
    })
    .await
}
