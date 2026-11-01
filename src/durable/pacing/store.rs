use std::time::Duration;

use anyhow::{Context, Result, ensure};
use sqlx::{Postgres, Row, Transaction};

use super::{Admission, PersistentPacing, Spacing, WireScope};

impl PersistentPacing {
    pub(super) async fn reserve(&self, scope: &WireScope) -> Result<Admission> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("opening wire pacing transaction")?;
        sqlx::query("SET LOCAL lock_timeout = '5s'")
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO durable_wire_pacing (pacing_key) \
             SELECT key FROM UNNEST($1::text[]) AS key ORDER BY key \
             ON CONFLICT (pacing_key) DO NOTHING",
        )
        .bind(scope.keys.as_slice())
        .execute(&mut *transaction)
        .await
        .context("ensuring persistent provider and origin pacing rows")?;
        lock_scope(&mut transaction, scope).await?;
        let row = sqlx::query(
            "SELECT GREATEST(0, CEIL(EXTRACT(EPOCH FROM \
             (MAX(next_allowed_at) - clock_timestamp())) * 1000000))::bigint AS wait_micros, \
             bool_or(lease_token IS NOT NULL) AS leased, \
             md5(random()::text || clock_timestamp()::text) AS token \
             FROM durable_wire_pacing WHERE pacing_key = ANY($1)",
        )
        .bind(scope.keys.as_slice())
        .fetch_one(&mut *transaction)
        .await?;
        let wait: i64 = row.try_get("wait_micros")?;
        if wait > 0 {
            let leased: bool = row.try_get("leased")?;
            transaction.commit().await?;
            let micros = u64::try_from(wait).context("negative persistent pacing delay")?;
            return Ok(Admission::Wait {
                remaining: Duration::from_micros(micros),
                leased,
            });
        }
        let token: String = row.try_get("token")?;
        let changed = sqlx::query(
            "WITH clock AS (SELECT clock_timestamp() AS now) \
             UPDATE durable_wire_pacing SET \
             next_allowed_at = clock.now + $2::bigint * INTERVAL '1 microsecond', \
             lease_token = $3, updated_at = clock.now FROM clock \
             WHERE pacing_key = ANY($1)",
        )
        .bind(scope.keys.as_slice())
        .bind(scope.reservation_micros)
        .bind(&token)
        .execute(&mut *transaction)
        .await?;
        ensure!(
            changed.rows_affected() == 2,
            "wire reservation lost its pacing scope"
        );
        transaction
            .commit()
            .await
            .context("committing pre-wire pacing reservation")?;
        Ok(Admission::Reserved(token))
    }

    pub(super) async fn complete(&self, scope: &WireScope, token: &str) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("opening wire pacing completion transaction")?;
        sqlx::query("SET LOCAL lock_timeout = '5s'")
            .execute(&mut *transaction)
            .await?;
        lock_scope(&mut transaction, scope).await?;
        let changed = sqlx::query(
            "WITH clock AS (SELECT clock_timestamp() AS now) \
             UPDATE durable_wire_pacing SET \
             next_allowed_at = CASE WHEN $4 THEN GREATEST(clock.now, updated_at + $2::bigint * INTERVAL '1 microsecond') \
               ELSE clock.now + $2::bigint * INTERVAL '1 microsecond' END, \
             lease_token = NULL, updated_at = clock.now FROM clock \
             WHERE pacing_key = ANY($1) AND lease_token = $3",
        )
        .bind(scope.keys.as_slice())
        .bind(scope.interval_micros)
        .bind(token)
        .bind(matches!(scope.spacing, Spacing::RequestStarts))
        .execute(&mut *transaction)
        .await
        .context("persisting completed wire request cooldown")?;
        ensure!(
            changed.rows_affected() == 2,
            "wire pacing lease changed before completion"
        );
        transaction
            .commit()
            .await
            .context("committing completed wire request cooldown")
    }
}

pub(super) async fn lock_scope(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &WireScope,
) -> Result<()> {
    let locked = sqlx::query(
        "SELECT pacing_key FROM durable_wire_pacing \
         WHERE pacing_key = ANY($1) ORDER BY pacing_key FOR UPDATE",
    )
    .bind(scope.keys.as_slice())
    .fetch_all(&mut **transaction)
    .await?;
    ensure!(locked.len() == 2, "persistent pacing scope lost a row");
    Ok(())
}
