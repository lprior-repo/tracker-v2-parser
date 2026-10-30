use anyhow::{Context, Result, ensure};
use sqlx::PgConnection;

use super::{Database, lock_request};
use crate::durable::page::{
    PageOutcome, PageRequest, PageReservation, ReservationDecision, RetryPolicy,
};

impl Database {
    pub(crate) async fn durable_reserve_configured(
        &self,
        request: &PageRequest,
        now_ms: i64,
        permit: Option<&crate::acquisition::LegacyFailurePermit>,
        policy: &RetryPolicy,
    ) -> Result<ReservationDecision> {
        crate::durable::page::validate_request(request)?;
        policy.check(request)?;
        let mut transaction = self.pool.begin().await?;
        let mode = permit.map_or(Mode::Acquire, Mode::Reviewed);
        let result = reserve(&mut transaction, request, now_ms, mode, policy).await;
        super::super::finish(transaction, result).await
    }

    pub async fn durable_reserve(
        &self,
        request: &PageRequest,
        now_ms: i64,
    ) -> Result<ReservationDecision> {
        crate::durable::page::validate_request(request)?;
        let mut transaction = self.pool.begin().await?;
        let result = reserve(
            &mut transaction,
            request,
            now_ms,
            Mode::Acquire,
            &RetryPolicy::default(),
        )
        .await;
        super::super::finish(transaction, result).await
    }

    pub async fn durable_reserve_offline(
        &self,
        request: &PageRequest,
        now_ms: i64,
    ) -> Result<ReservationDecision> {
        crate::durable::page::validate_request(request)?;
        let mut transaction = self.pool.begin().await?;
        let result = reserve(
            &mut transaction,
            request,
            now_ms,
            Mode::Offline,
            &RetryPolicy::default(),
        )
        .await;
        super::super::finish(transaction, result).await
    }
}

#[derive(Clone, Copy)]
enum Mode<'a> {
    Acquire,
    Offline,
    Reviewed(&'a crate::acquisition::LegacyFailurePermit),
}

async fn register(connection: &mut PgConnection, request: &PageRequest) -> Result<()> {
    let (cursor, attempts, status): (serde_json::Value, i32, String) =
        sqlx::query_as("SELECT cursor,attempts,status FROM scrape_pages WHERE id=$1 FOR UPDATE")
            .bind(request.binding.page_id)
            .fetch_one(&mut *connection)
            .await?;
    let target: crate::model::Target = serde_json::from_value(cursor)?;
    ensure!(
        serde_json::to_value(target)? == serde_json::to_value(&request.target)?,
        "durable target does not match checkpoint"
    );
    let other_owner: Option<String> = sqlx::query_scalar("SELECT d.operation_id FROM durable_page_operations d JOIN durable_page_attempts a ON a.operation_id=d.operation_id WHERE d.page_id=$1 AND d.operation_id<>$2 AND a.state IN ('reserved','captured','parsed') ORDER BY d.created_at LIMIT 1")
        .bind(request.binding.page_id).bind(&request.binding.operation_id).fetch_optional(&mut *connection).await?;
    ensure!(
        other_owner.is_none(),
        "another durable operation owns unfinished work for this page; explicit handoff required"
    );
    let replayed_stop: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM durable_page_operations WHERE operation_id=$1 AND state='blocked')")
        .bind(&request.binding.operation_id).fetch_one(&mut *connection).await?;
    ensure!(
        status != "blocked" || replayed_stop,
        "checkpoint is blocked"
    );
    sqlx::query("INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,original_attempts) VALUES($1,$2,$3,$4,$5) ON CONFLICT(operation_id) DO NOTHING")
        .bind(&request.binding.operation_id).bind(request.binding.page_id).bind(&request.binding.scope.run_generation).bind(serde_json::to_value(&request.binding)?).bind(attempts)
        .execute(connection).await?;
    Ok(())
}

async fn reserve(
    connection: &mut PgConnection,
    request: &PageRequest,
    now_ms: i64,
    mode: Mode<'_>,
    policy: &RetryPolicy,
) -> Result<ReservationDecision> {
    register(connection, request).await?;
    let (state, cycle, attempts, due, outcome) = lock_request(connection, request).await?;
    if let Some(outcome) =
        outcome.filter(|_| state != "deferred" || due.is_some_and(|at| at > now_ms))
    {
        return Ok(ReservationDecision::Settled {
            outcome: serde_json::from_value(outcome)?,
        });
    }
    if let Some(reservation) = unfinished(connection, request).await? {
        return Ok(ReservationDecision::Reserved {
            reservation: Box::new(reservation),
        });
    }
    if now_ms >= request.binding.scope.deadline_ms {
        return exhausted(connection, request).await;
    }
    let legacy: Option<String> = sqlx::query_scalar("SELECT last_error FROM scrape_pages WHERE id=$1 AND attempts>0 AND NOT EXISTS(SELECT 1 FROM durable_page_attempts WHERE operation_id=$2)")
        .bind(request.binding.page_id).bind(&request.binding.operation_id).fetch_optional(&mut *connection).await?.flatten();
    if !matches!(mode, Mode::Offline)
        && legacy.is_some_and(|detail| {
            !crate::durable::page::transient_detail(&detail)
                && !matches!(mode, Mode::Reviewed(permit) if permit.permits(request, &detail))
        })
    {
        let outcome = PageOutcome::Rejected {
            detail: "legacy failure is not classified transient; original checkpoint preserved"
                .to_owned(),
        };
        sqlx::query(
            "UPDATE durable_page_operations SET state='rejected',outcome=$2 WHERE operation_id=$1",
        )
        .bind(&request.binding.operation_id)
        .bind(serde_json::to_value(&outcome)?)
        .execute(connection)
        .await?;
        return Ok(ReservationDecision::Settled { outcome });
    }
    ensure!(
        (1..=86_400_000).contains(
            &request
                .binding
                .scope
                .deadline_ms
                .checked_sub(now_ms)
                .context("durable deadline overflow")?
        ),
        "durable run deadline exceeds 24 hours"
    );
    ensure!(
        attempts <= policy.attempts(),
        "durable cycle exceeds configured retry attempts"
    );
    let cycle = if attempts == policy.attempts() {
        cycle.checked_add(1).context("durable cycle overflow")?
    } else {
        cycle
    };
    let ordinal = if attempts == policy.attempts() {
        1
    } else {
        attempts
            .checked_add(1)
            .context("durable attempt overflow")?
    };
    let attempt_id = crate::durable::page::attempt_identity(request, cycle, ordinal)?;
    let total_attempts: i32 = sqlx::query_scalar("UPDATE scrape_pages SET attempts=attempts+1,status='pending' WHERE id=$1 AND (status IN ('pending','failed') OR ($2 AND status='succeeded')) RETURNING attempts")
        .bind(request.binding.page_id).bind(matches!(mode, Mode::Offline)).fetch_one(&mut *connection).await.context("reserving durable page attempt")?;
    sqlx::query("INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts) VALUES($1,$2,$3,$4,$5)")
        .bind(&attempt_id).bind(&request.binding.operation_id).bind(cycle).bind(ordinal).bind(total_attempts).execute(&mut *connection).await?;
    sqlx::query("UPDATE durable_page_operations SET state='active',cycle=$2,cycle_attempts=$3,retry_at_ms=NULL,outcome=NULL WHERE operation_id=$1")
        .bind(&request.binding.operation_id).bind(cycle).bind(ordinal).execute(connection).await?;
    Ok(ReservationDecision::Reserved {
        reservation: Box::new(PageReservation {
            request: request.clone(),
            attempt_id,
            cycle,
            ordinal,
            total_attempts,
        }),
    })
}

async fn unfinished(
    connection: &mut PgConnection,
    request: &PageRequest,
) -> Result<Option<PageReservation>> {
    let row: Option<(String,i32,i32,i32)> = sqlx::query_as("SELECT attempt_id,cycle,ordinal,total_attempts FROM durable_page_attempts WHERE operation_id=$1 AND state IN ('reserved','captured','parsed') ORDER BY cycle DESC,ordinal DESC LIMIT 1")
        .bind(&request.binding.operation_id).fetch_optional(connection).await?;
    Ok(row.map(
        |(attempt_id, cycle, ordinal, total_attempts)| PageReservation {
            request: request.clone(),
            attempt_id,
            cycle,
            ordinal,
            total_attempts,
        },
    ))
}

async fn exhausted(
    connection: &mut PgConnection,
    request: &PageRequest,
) -> Result<ReservationDecision> {
    let outcome = PageOutcome::Exhausted {
        detail: "durable run deadline reached; cumulative attempts retained".to_owned(),
    };
    sqlx::query("UPDATE durable_page_operations SET state='exhausted',retry_at_ms=NULL,outcome=$2 WHERE operation_id=$1")
        .bind(&request.binding.operation_id).bind(serde_json::to_value(&outcome)?).execute(connection).await?;
    Ok(ReservationDecision::Settled { outcome })
}
