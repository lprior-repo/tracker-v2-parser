use std::time::Duration;

use anyhow::{Context, Result, ensure};
use sqlx::{Postgres, Row, Transaction};

use super::{PersistentPacing, WireReceipt, WireRequest};
use crate::durable::pacing::{Spacing, WireScope};

impl PersistentPacing {
    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn reserve_start(
        &self,
        request: &WireRequest,
        receipt: &WireReceipt,
        interval: Duration,
    ) -> Result<Option<(Duration, bool)>> {
        self.check_wire(receipt, request).await?;
        let scope = WireScope::parse(
            request.source,
            &request.address,
            interval,
            Spacing::RequestStarts,
        )?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("opening durable start admission")?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO durable_wire_pacing(pacing_key) SELECT key FROM UNNEST($1::text[]) AS key ORDER BY key ON CONFLICT(pacing_key) DO NOTHING")
            .bind(scope.keys.as_slice()).execute(&mut *transaction).await?;
        crate::durable::pacing::store::lock_scope(&mut transaction, &scope).await?;
        check_stops(&mut transaction, request).await?;
        let row = sqlx::query("SELECT GREATEST(0, CEIL(EXTRACT(EPOCH FROM (MAX(next_allowed_at)-clock_timestamp()))*1000000))::bigint AS wait_micros,bool_or(lease_token IS NOT NULL) AS leased FROM durable_wire_pacing WHERE pacing_key=ANY($1)")
            .bind(scope.keys.as_slice()).fetch_one(&mut *transaction).await?;
        let wait: i64 = row.try_get("wait_micros")?;
        if wait > 0 {
            let leased = row.try_get("leased")?;
            transaction.commit().await?;
            return Ok(Some((Duration::from_micros(u64::try_from(wait)?), leased)));
        }
        let active: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_wire_requests WHERE source=$1 AND state='intent' AND started_at>clock_timestamp()-INTERVAL '45 seconds'")
            .bind(request.source.as_str()).fetch_one(&mut *transaction).await?;
        if active >= 3 {
            transaction.commit().await?;
            return Ok(Some((Duration::from_millis(100), true)));
        }
        let inserted =
            super::super::store::insert_wire(&mut *transaction, receipt, request).await?;
        ensure!(
            inserted == 1,
            "durable wire network interruption: overlapped intent already exists; bounded next attempt required"
        );
        let changed = sqlx::query("WITH clock AS(SELECT clock_timestamp() AS now) UPDATE durable_wire_pacing SET next_allowed_at=clock.now+$2::bigint*INTERVAL '1 microsecond',lease_token=NULL,updated_at=clock.now FROM clock WHERE pacing_key=ANY($1)")
            .bind(scope.keys.as_slice()).bind(scope.interval_micros).execute(&mut *transaction).await?;
        ensure!(
            changed.rows_affected() == 2,
            "overlapped admission lost pacing rows"
        );
        transaction
            .commit()
            .await
            .context("committing durable start admission and wire intent")?;
        Ok(None)
    }
}

async fn check_stops(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WireRequest,
) -> Result<()> {
    let status: Option<i32> = sqlx::query_scalar("SELECT status FROM durable_wire_requests WHERE origin=$1 AND status IN(401,402,403,429,451) ORDER BY started_at LIMIT 1")
        .bind(&request.origin).fetch_optional(&mut **transaction).await?;
    if let Some(status) = status {
        return Err(crate::transport::AccessStop {
            origin: request.origin.clone(),
            status: Some(u16::try_from(status)?),
        }
        .into());
    }
    let (_, authority) = request
        .origin
        .split_once("://")
        .context("wire origin has no authority")?;
    let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM scrape_pages p CROSS JOIN LATERAL unnest(ARRAY[p.url,p.cursor->>'access_stop_url']) AS stop(url) WHERE p.status='blocked' AND p.cursor->>'kind'='contacts' AND regexp_replace(lower(substring(stop.url from '(?i)^https?://([^/?#]+)')),CASE WHEN lower(stop.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'')=$1)")
        .bind(authority.to_ascii_lowercase()).fetch_one(&mut **transaction).await?;
    if blocked {
        return Err(crate::transport::AccessStop {
            origin: request.origin.clone(),
            status: None,
        }
        .into());
    }
    Ok(())
}
