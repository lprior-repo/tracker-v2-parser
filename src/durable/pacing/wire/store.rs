use anyhow::{Context, Result, ensure};
use sqlx::Row;

use super::{PersistentPacing, WireReceipt, WireRequest};

impl PersistentPacing {
    #[tracing::instrument(skip_all)]
    pub(super) async fn check_contact_origin(&self, request: &WireRequest) -> Result<()> {
        let (_, authority) = request
            .origin
            .split_once("://")
            .context("wire origin has no authority")?;
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM scrape_pages p \
             CROSS JOIN LATERAL unnest(ARRAY[p.url,p.cursor->>'access_stop_url']) AS stop(url) \
             WHERE p.status='blocked' AND p.cursor->>'kind'='contacts' \
             AND regexp_replace(lower(substring(stop.url from '(?i)^https?://([^/?#]+)')), \
               CASE WHEN lower(stop.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') = $1)",
        )
        .bind(authority.to_ascii_lowercase())
        .fetch_one(&self.pool)
        .await?;
        if blocked {
            return Err(crate::transport::AccessStop {
                origin: request.origin.clone(),
                status: None,
            }
            .into());
        }
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn check_wire_origin(&self, request: &WireRequest) -> Result<()> {
        let status: Option<i32> = sqlx::query_scalar(
            "SELECT status FROM durable_wire_requests WHERE origin=$1 \
             AND status IN (401,402,403,429,451) ORDER BY started_at LIMIT 1",
        )
        .bind(&request.origin)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(status) = status {
            return Err(crate::transport::AccessStop {
                origin: request.origin.clone(),
                status: Some(u16::try_from(status)?),
            }
            .into());
        }
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn begin_wire(
        &self,
        receipt: &WireReceipt,
        request: &WireRequest,
    ) -> Result<()> {
        let inserted = insert_wire(&self.pool, receipt, request).await?;
        if inserted == 1 {
            return Ok(());
        }
        self.check_wire(receipt, request).await?;
        Err(anyhow::anyhow!(
            "durable wire network interruption: conflicting intent disappeared"
        ))
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn check_wire(
        &self,
        receipt: &WireReceipt,
        request: &WireRequest,
    ) -> Result<()> {
        let row = sqlx::query("SELECT operation_id, attempt_id, request_sha256, occurrence, origin, status FROM durable_wire_requests WHERE wire_id=$1")
            .bind(&receipt.wire_id).fetch_optional(&self.pool).await?;
        let Some(row) = row else {
            return Ok(());
        };
        ensure!(
            row.try_get::<String, _>("operation_id")? == receipt.operation_id
                && row.try_get::<String, _>("attempt_id")? == receipt.attempt_id
                && row.try_get::<String, _>("request_sha256")? == receipt.request_sha256
                && row.try_get::<i32, _>("occurrence")? == receipt.occurrence
                && row.try_get::<String, _>("origin")? == request.origin,
            "durable wire identity binding mismatch"
        );
        let status: Option<i32> = row.try_get("status")?;
        if let Some(status) = status
            .map(u16::try_from)
            .transpose()?
            .filter(|status| crate::transport::access_stop_status(*status))
        {
            return Err(crate::transport::AccessStop {
                origin: request.origin.clone(),
                status: Some(status),
            }
            .into());
        }
        Err(anyhow::anyhow!(
            "durable wire network interruption: existing intent or result requires the next bounded page attempt"
        ))
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn complete_wire(
        &self,
        receipt: &WireReceipt,
        status: Option<u16>,
    ) -> Result<()> {
        self.complete_wire_executed(receipt, status, None).await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn complete_wire_executed(
        &self,
        receipt: &WireReceipt,
        status: Option<u16>,
        executed_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let state = if status.is_some() {
            "headers"
        } else {
            "failed"
        };
        let changed = sqlx::query(
            "UPDATE durable_wire_requests SET state=$2, status=$3, \
             error_class=CASE WHEN $3::integer IS NULL THEN 'transport' ELSE NULL END, \
             finished_at=clock_timestamp(), execute_started_at=$4 WHERE wire_id=$1 AND state='intent'",
        )
        .bind(&receipt.wire_id)
        .bind(state)
        .bind(status.map(i32::from))
        .bind(executed_at)
        .execute(&self.pool)
        .await
        .context("recording durable wire result")?;
        ensure!(
            changed.rows_affected() == 1,
            "durable wire result changed before completion"
        );
        Ok(())
    }
}

pub(super) async fn insert_wire<'e, E>(
    executor: E,
    receipt: &WireReceipt,
    request: &WireRequest,
) -> Result<u64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let inserted = sqlx::query(
        "INSERT INTO durable_wire_requests \
         (wire_id, operation_id, attempt_id, request_sha256, occurrence, source, origin, method, state) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'intent') ON CONFLICT (wire_id) DO NOTHING",
    ).bind(&receipt.wire_id).bind(&receipt.operation_id).bind(&receipt.attempt_id)
        .bind(&receipt.request_sha256).bind(receipt.occurrence).bind(request.source.as_str())
        .bind(&request.origin).bind(&request.method).execute(executor).await
        .context("recording pre-wire durable intent")?;
    Ok(inserted.rows_affected())
}
