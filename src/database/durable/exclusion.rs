use anyhow::{Context, Result, ensure};

use super::{Database, lock_request};
use crate::{
    acquisition::ScopeExclusion,
    durable::page::{PageOutcome, PageRequest, PageReservation, ReservationDecision},
};

impl Database {
    pub(crate) async fn durable_exclude(
        &self,
        request: &PageRequest,
        exclusion: &ScopeExclusion,
    ) -> Result<ReservationDecision> {
        exclude(self, request, exclusion, None).await
    }

    pub(crate) async fn durable_exclude_reserved(
        &self,
        reservation: &PageReservation,
        exclusion: &ScopeExclusion,
    ) -> Result<PageOutcome> {
        match exclude(self, &reservation.request, exclusion, Some(reservation)).await? {
            ReservationDecision::Settled { outcome } => Ok(outcome),
            ReservationDecision::Reserved { .. } => {
                anyhow::bail!("excluded reservation remains unfinished")
            }
        }
    }
}

async fn exclude(
    database: &Database,
    request: &PageRequest,
    exclusion: &ScopeExclusion,
    inspected: Option<&PageReservation>,
) -> Result<ReservationDecision> {
    crate::durable::page::validate_request(request)?;
    exclusion.check(request)?;
    let mut transaction = database.pool.begin().await?;
    super::reservation::register(&mut transaction, request, false).await?;
    let (state, _, _, _, saved) = lock_request(&mut transaction, request).await?;
    if matches!(
        state.as_str(),
        "completed" | "blocked" | "rejected" | "exhausted"
    ) {
        let saved = saved.context("settled durable operation is missing its outcome")?;
        return super::super::finish(
            transaction,
            Ok(ReservationDecision::Settled {
                outcome: serde_json::from_value(saved)?,
            }),
        )
        .await;
    }
    let status: String = sqlx::query_scalar("SELECT status FROM scrape_pages WHERE id=$1")
        .bind(request.binding.page_id)
        .fetch_one(&mut *transaction)
        .await?;
    ensure!(
        status != "succeeded",
        "succeeded checkpoint cannot acquire a new exclusion outcome"
    );
    if let Some(reservation) = super::reservation::unfinished(&mut transaction, request).await? {
        let Some(inspected) = inspected else {
            return super::super::finish(
                transaction,
                Ok(ReservationDecision::Reserved {
                    reservation: Box::new(reservation),
                }),
            )
            .await;
        };
        ensure!(
            serde_json::to_value(&reservation)? == serde_json::to_value(inspected)?,
            "inspected exclusion reservation differs from unfinished attempt"
        );
    } else {
        ensure!(
            inspected.is_none(),
            "inspected exclusion reservation is no longer unfinished"
        );
    }
    let outcome = PageOutcome::Rejected {
        detail: exclusion.reason().to_owned(),
    };
    let value = serde_json::to_value(&outcome)?;
    sqlx::query("UPDATE durable_page_attempts SET state='failed',outcome=$2 WHERE operation_id=$1 AND outcome IS NULL AND state IN ('reserved','captured','parsed')")
            .bind(&request.binding.operation_id).bind(&value).execute(&mut *transaction).await?;
    sqlx::query("UPDATE durable_page_operations SET state='rejected',outcome=$2,retry_at_ms=NULL WHERE operation_id=$1")
            .bind(&request.binding.operation_id).bind(value).execute(&mut *transaction).await?;
    super::super::finish(transaction, Ok(ReservationDecision::Settled { outcome })).await
}
