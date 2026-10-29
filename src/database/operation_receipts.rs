use anyhow::{Context, Result, ensure};
use futures::future::BoxFuture;
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{PgConnection, PgPool};

#[tracing::instrument(skip_all)]
pub async fn with_receipt<T, F>(
    pool: &PgPool,
    operation_key: &str,
    input_digest: &str,
    apply: F,
) -> Result<T>
where
    T: Serialize + DeserializeOwned,
    F: for<'a> FnOnce(&'a mut PgConnection) -> BoxFuture<'a, Result<T>>,
{
    validate(operation_key, input_digest)?;
    let mut transaction = pool.begin().await?;
    let result = execute(&mut transaction, operation_key, input_digest, apply).await;
    match result {
        Ok(value) => {
            transaction
                .commit()
                .await
                .context("committing durable operation and receipt")?;
            Ok(value)
        }
        Err(error) => {
            transaction
                .rollback()
                .await
                .context("rolling back durable operation")?;
            Err(error)
        }
    }
}

fn validate(key: &str, digest: &str) -> Result<()> {
    ensure!(
        !key.is_empty() && key.len() <= 512 && key.bytes().all(|b| b.is_ascii_graphic()),
        "operation key must contain 1..512 printable ASCII bytes"
    );
    ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "operation input digest must be SHA-256"
    );
    Ok(())
}

#[tracing::instrument(skip_all)]
async fn execute<T, F>(
    connection: &mut PgConnection,
    key: &str,
    digest: &str,
    apply: F,
) -> Result<T>
where
    T: Serialize + DeserializeOwned,
    F: for<'a> FnOnce(&'a mut PgConnection) -> BoxFuture<'a, Result<T>>,
{
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext(current_schema()),hashtext($1))")
        .bind(key)
        .execute(&mut *connection)
        .await?;
    let receipt: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT input_sha256,result FROM durable_operation_receipts WHERE operation_key=$1",
    )
    .bind(key)
    .fetch_optional(&mut *connection)
    .await?;
    if let Some((original, value)) = receipt {
        ensure!(
            original.eq_ignore_ascii_case(digest),
            "operation key was reused for different input"
        );
        return serde_json::from_value(value).context("decoding committed durable receipt");
    }
    let value = apply(&mut *connection).await?;
    let serialized = serde_json::to_value(&value)?;
    ensure!(
        serde_json::to_vec(&serialized)?.len() <= 1_048_576,
        "operation receipt exceeds 1 MiB"
    );
    sqlx::query("INSERT INTO durable_operation_receipts(operation_key,input_sha256,result) VALUES($1,$2,$3)")
        .bind(key).bind(digest.to_ascii_lowercase()).bind(serialized)
        .execute(connection).await?;
    Ok(value)
}
