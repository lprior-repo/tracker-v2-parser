use anyhow::Result;
use sqlx::{Connection, PgConnection};

pub async fn install(url: &str, database: &str) -> Result<()> {
    assert!(
        database.starts_with("native_migration_source_")
            && database
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    );
    let mut connection = PgConnection::connect(url).await?;
    sqlx::raw_sql("CREATE SCHEMA shadow; CREATE TABLE shadow.durable_page_attempts(capture jsonb,parsed jsonb); CREATE TABLE shadow.durable_page_receipts(capture jsonb,parsed jsonb); INSERT INTO shadow.durable_page_attempts(capture) VALUES(jsonb_build_object('body',jsonb_build_object('sha256',repeat('f',64),'bytes',17))); ")
        .execute(&mut connection).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE \"{database}\" SET search_path TO shadow,public"
    )))
    .execute(&mut connection)
    .await?;
    connection.close().await?;
    let mut verify = PgConnection::connect(url).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT current_schema()")
            .fetch_one(&mut verify)
            .await?,
        "shadow"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
            .fetch_one(&mut verify)
            .await?,
        1
    );
    let public: i64 = sqlx::query_scalar("SELECT count(*) FROM public.durable_page_attempts")
        .fetch_one(&mut verify)
        .await?;
    assert!(public > 0);
    verify.close().await?;
    Ok(())
}
