#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::Result;
use track_scrapper_v2::{
    database::Database,
    durable::page::{self, PageOutcome},
};

#[tokio::test]
async fn receipt_insert_failure_rolls_back_domain_and_success_then_same_attempt_recovers()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let scope=page_support::scope(&page_support::config()?)?;
        let root=test.directory().join("artifacts");
        sqlx::raw_sql("CREATE FUNCTION refuse_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected receipt persistence failure'; END $$; CREATE TRIGGER refuse_receipt BEFORE INSERT ON durable_page_receipts FOR EACH ROW EXECUTE FUNCTION refuse_receipt();")
            .execute(&test.pool).await?;
        let error=page::offline_replay(&database,&root,&scope,page_support::profile(),page_support::capture()?).await.expect_err("receipt refusal aborts domain transaction");
        assert!(format!("{error:#}").contains("injected receipt persistence failure"));
        let counts:(i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM schools),(SELECT count(*) FROM athletes),(SELECT count(*) FROM results),(SELECT count(*) FROM durable_page_receipts),(SELECT count(*) FROM scrape_pages WHERE status='succeeded')").fetch_one(&test.pool).await?;
        assert_eq!(counts,(0,0,0,0,0));
        let attempt:(i32,String,Option<String>)=sqlx::query_as("SELECT attempts,status,last_error FROM scrape_pages").fetch_one(&test.pool).await?;
        assert_eq!(attempt,(1,"pending".into(),None));
        assert_eq!(sqlx::query_scalar::<_,String>("SELECT state FROM durable_page_attempts").fetch_one(&test.pool).await?,"parsed");
        sqlx::query("DROP TRIGGER refuse_receipt ON durable_page_receipts").execute(&test.pool).await?;
        database.close().await;
        let recovered=Database::connect(&test.url).await?;
        assert!(matches!(page::offline_replay(&recovered,&root,&scope,page_support::profile(),page_support::capture()?).await?,PageOutcome::Completed {..}));
        assert_eq!(sqlx::query_scalar::<_,i32>("SELECT attempts FROM scrape_pages WHERE cursor->'context'->>'athlete_id'='15005189' AND status='succeeded'").fetch_one(&test.pool).await?,1);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_receipts").fetch_one(&test.pool).await?,1);
        assert!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM results").fetch_one(&test.pool).await?>=9);
        recovered.close().await;
        Ok(())
    })).await
}
