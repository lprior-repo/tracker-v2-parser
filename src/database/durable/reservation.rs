use anyhow::{Context, Result, ensure};
use sqlx::PgConnection;

use super::{Database, lock_request};
use crate::durable::page::{
    IndexRefreshPermit, PageOutcome, PageRequest, PageReservation, ReservationDecision, RetryPolicy,
};

impl Database {
    pub(crate) async fn durable_failure_detail(
        &self,
        request: &PageRequest,
    ) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT last_error FROM scrape_pages WHERE id=$1")
            .bind(request.binding.page_id)
            .fetch_one(&self.pool)
            .await
            .context("reading retained page failure")
    }

    pub(crate) async fn durable_acquisition_attempts(&self, request: &PageRequest) -> Result<i64> {
        let mut connection = self.pool.acquire().await?;
        attempts_used(&mut connection, request).await
    }

    pub(crate) async fn durable_reserve_configured(
        &self,
        request: &PageRequest,
        now_ms: i64,
        permit: Option<&crate::acquisition::LegacyFailurePermit>,
        index: Option<&IndexRefreshPermit>,
        policy: &RetryPolicy,
    ) -> Result<ReservationDecision> {
        crate::durable::page::validate_request(request)?;
        policy.check(request)?;
        let mut transaction = self.pool.begin().await?;
        let mode = Mode::Configured {
            reviewed: permit,
            index,
        };
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
    Configured {
        reviewed: Option<&'a crate::acquisition::LegacyFailurePermit>,
        index: Option<&'a IndexRefreshPermit>,
    },
}

pub(super) async fn register(
    connection: &mut PgConnection,
    request: &PageRequest,
    completed_index_replay: bool,
) -> Result<()> {
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
    let other_owner: Option<String> = sqlx::query_scalar("SELECT d.operation_id FROM durable_page_operations d JOIN durable_page_attempts a ON a.operation_id=d.operation_id WHERE d.page_id=$1 AND d.operation_id<>$2 AND a.state IN ('reserved','captured','parsed') AND NOT ($3 AND EXISTS(SELECT 1 FROM durable_page_operations saved WHERE saved.operation_id=$2 AND saved.page_id=$1 AND saved.state='completed' AND saved.outcome IS NOT NULL)) ORDER BY d.created_at LIMIT 1")
        .bind(request.binding.page_id).bind(&request.binding.operation_id).bind(completed_index_replay).fetch_optional(&mut *connection).await?;
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
    let completed_index_replay =
        matches!(mode, Mode::Configured { index: Some(permit), .. } if permit.permits(request));
    register(connection, request, completed_index_replay).await?;
    let (state, cycle, attempts, due, outcome) = lock_request(connection, request).await?;
    if let Some(outcome) = outcome.as_ref().filter(|_| state != "deferred") {
        return Ok(ReservationDecision::Settled {
            outcome: serde_json::from_value(outcome.clone())?,
        });
    }
    if let Some(reservation) = unfinished(connection, request).await? {
        return Ok(ReservationDecision::Reserved {
            reservation: Box::new(reservation),
        });
    }
    let used = attempts_used(connection, request).await?;
    if used >= i64::from(policy.attempts()) {
        let detail: Option<String> =
            sqlx::query_scalar("SELECT last_error FROM scrape_pages WHERE id=$1")
                .bind(request.binding.page_id)
                .fetch_one(&mut *connection)
                .await?;
        let outcome = policy.exhausted(
            used,
            detail
                .as_deref()
                .map_or("retained attempts preserved", |value| value),
        );
        sqlx::query("UPDATE durable_page_operations SET state='exhausted',retry_at_ms=NULL,outcome=$2 WHERE operation_id=$1")
            .bind(&request.binding.operation_id).bind(serde_json::to_value(&outcome)?).execute(connection).await?;
        return Ok(ReservationDecision::Settled { outcome });
    }
    if let Some(outcome) = outcome.filter(|_| due.is_some_and(|at| at > now_ms)) {
        return Ok(ReservationDecision::Settled {
            outcome: serde_json::from_value(outcome)?,
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
                && !matches!(mode, Mode::Configured { reviewed: Some(permit), .. } if permit.permits(request, &detail))
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
    let ordinal = attempts
        .checked_add(1)
        .context("durable attempt overflow")?;
    let attempt_id = crate::durable::page::attempt_identity(request, cycle, ordinal)?;
    let refresh_succeeded = matches!(mode, Mode::Offline)
        || matches!(mode, Mode::Configured { index: Some(permit), .. } if permit.permits(request));
    let total_attempts: i32 = sqlx::query_scalar("UPDATE scrape_pages SET attempts=attempts+1,status='pending' WHERE id=$1 AND (status IN ('pending','failed') OR ($2 AND status='succeeded')) RETURNING attempts")
        .bind(request.binding.page_id).bind(refresh_succeeded).fetch_one(&mut *connection).await.context("reserving durable page attempt")?;
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

pub(super) async fn unfinished(
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

pub(super) async fn attempts_used(
    connection: &mut PgConnection,
    request: &PageRequest,
) -> Result<i64> {
    let generation = request.binding.scope.run_generation.as_str();
    let campaign = ["-schools", "-contacts", "-results", "-complete"]
        .into_iter()
        .find_map(|suffix| generation.strip_suffix(suffix))
        .map_or(generation, |value| value);
    sqlx::query_scalar("SELECT count(*) FROM durable_page_attempts a JOIN durable_page_operations d USING(operation_id) WHERE d.page_id=$1 AND d.run_generation IN ($2,$2||'-schools',$2||'-contacts',$2||'-results',$2||'-complete')")
        .bind(request.binding.page_id).bind(campaign)
        .fetch_one(connection).await.context("counting durable acquisition attempts")
}
