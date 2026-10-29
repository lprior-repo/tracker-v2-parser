use anyhow::{Context, Result, ensure};
use sqlx::{PgPool, Postgres, QueryBuilder, postgres::PgPoolOptions};

pub(super) struct SetupFailure {
    pub error: anyhow::Error,
    pub schema_removed: bool,
}

async fn exists(pool: &PgPool, schema: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)")
            .bind(schema)
            .fetch_one(pool)
            .await?,
    )
}

pub(super) async fn remove_schema(pool: &PgPool, schema: &str) -> Result<()> {
    validate(schema)?;
    if !exists(pool, schema).await? {
        return Ok(());
    }
    let dropped = QueryBuilder::<Postgres>::new("DROP SCHEMA IF EXISTS ")
        .push(schema)
        .push(" CASCADE")
        .build()
        .execute(pool)
        .await;
    let remains = exists(pool, schema)
        .await
        .context("verifying owned schema absence after cleanup")?;
    if !remains {
        return Ok(());
    }
    dropped.context("dropping owned recovery schema")?;
    anyhow::bail!("owned recovery schema remains after cleanup")
}

pub(super) async fn setup(url: &str, schema: &str) -> std::result::Result<PgPool, SetupFailure> {
    if let Err(error) = validate(schema) {
        return Err(SetupFailure {
            error,
            schema_removed: true,
        });
    }
    let pool = match PgPoolOptions::new().max_connections(2).connect(url).await {
        Ok(pool) => pool,
        Err(error) => {
            return Err(SetupFailure {
                error: anyhow::Error::from(error)
                    .context("connecting isolated recovery database before schema creation"),
                schema_removed: true,
            });
        }
    };
    let created = QueryBuilder::<Postgres>::new("CREATE SCHEMA ")
        .push(schema)
        .build()
        .execute(&pool)
        .await;
    match created {
        Ok(_) => Ok(pool),
        Err(error) => {
            if matches!(&error,sqlx::Error::Database(database) if database.code().is_some_and(|code|code=="42P06"))
            {
                pool.close().await;
                return Err(SetupFailure {
                    error: anyhow::Error::from(error)
                        .context("recovery schema already exists; existing namespace preserved"),
                    schema_removed: false,
                });
            }
            let removed = remove_schema(&pool, schema).await;
            pool.close().await;
            let error = anyhow::Error::from(error).context("creating owned recovery schema");
            let failure = match removed {
                Ok(()) => SetupFailure {
                    error,
                    schema_removed: true,
                },
                Err(cleanup) => SetupFailure {
                    error: error.context(format!("schema cleanup unconfirmed: {cleanup:#}")),
                    schema_removed: false,
                },
            };
            Err(failure)
        }
    }
}

fn validate(schema: &str) -> Result<()> {
    ensure!(
        schema.len() <= 63
            && schema.starts_with("restate_e2e_")
            && schema
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "owned recovery schema identifier invalid"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_connection_cannot_claim_an_attempted_schema_write() -> Result<()> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let failure = match setup(
            &format!("postgres://owned:dummy@127.0.0.1:{port}/unused?connect_timeout=1"),
            "restate_e2e_owned_failure",
        )
        .await
        {
            Ok(pool) => {
                pool.close().await;
                anyhow::bail!("fixture unexpectedly connected");
            }
            Err(failure) => failure,
        };
        assert!(failure.schema_removed);
        assert!(failure.error.to_string().contains("before schema creation"));
        assert!(validate("public;DROP SCHEMA public").is_err());
        Ok(())
    }
}
