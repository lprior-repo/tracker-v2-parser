#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json, to_value};
use track_scrapper_v2::{
    database::Database,
    durable::page::{self, PageOutcome, ReservationDecision},
    model::{PageContext, PageKind, Source, Target},
};

#[tokio::test]
async fn given_a_committed_implicit_get_capture_when_explicit_get_replays_then_the_receipt_is_unchanged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let target = page_support::profile();
            let request = page::bind_target(&database, target.clone(), &scope).await?;
            let blocking = track_scrapper_v2::durable::blocking::BlockingStages::new();
            let root = test.directory().join("artifacts");
            let before = page::offline_replay(
                &database,
                track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                &scope,
                target.clone(),
                page_support::capture()?,
            )
            .await?;
            let PageOutcome::Completed {
                page_id,
                receipt_sha256,
            } = &before
            else {
                bail!("initial fixture replay did not complete: {before:?}");
            };
            assert_eq!(*page_id, request.binding.page_id());
            let receipts = receipts(&database).await?;
            let [(stored_digest, _)] = receipts.as_slice() else {
                bail!("expected one committed receipt, got {receipts:?}");
            };
            assert_eq!(stored_digest, receipt_sha256);
            let actual = page::offline_replay(
                &database,
                track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                &scope,
                explicit_get(target),
                page_support::capture()?,
            )
            .await?;
            assert_eq!(actual, before);
            assert_eq!(self::receipts(&database).await?, receipts);
            Ok(())
        })
    })
    .await
}

async fn receipts(database: &Database) -> Result<Vec<(String, Value)>> {
    Ok(sqlx::query_as(
        "SELECT receipt_sha256,to_jsonb(r) FROM durable_page_receipts r ORDER BY operation_id",
    )
    .fetch_all(database.pool())
    .await?)
}

#[tokio::test]
async fn given_an_implicit_get_reservation_when_explicit_get_rebinds_then_the_same_attempt_is_retained()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let now = scope.deadline_ms - 30_000;
            let target = page_support::profile();
            let retained = page::bind_target(&database, target.clone(), &scope).await?;
            let before = reserve(&database, &retained, now).await?;
            let rebound =
                page::bind_target(&database, explicit_get(target.clone()), &scope).await?;
            let after = reserve(&database, &rebound, now).await?;
            assert_eq!(rebound.binding, retained.binding);
            assert_eq!(to_value(rebound.target)?, to_value(target)?);
            assert_eq!(after.attempt_id, before.attempt_id);
            assert_eq!(
                (after.cycle, after.ordinal, after.total_attempts),
                (1, 1, 1)
            );
            let attempts: Vec<String> =
                sqlx::query_scalar("SELECT attempt_id FROM durable_page_attempts")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(attempts, [before.attempt_id]);
            Ok(())
        })
    })
    .await
}

async fn reserve(
    database: &Database,
    request: &page::PageRequest,
    now: i64,
) -> Result<Box<page::PageReservation>> {
    match database.durable_reserve_offline(request, now).await? {
        ReservationDecision::Reserved { reservation } => Ok(reservation),
        ReservationDecision::Settled { outcome } => {
            bail!("expected a reservation, got {outcome:?}")
        }
    }
}

fn explicit_get(mut target: Target) -> Target {
    target.method = Some("GET".into());
    target
}

#[tokio::test]
async fn given_a_contact_checkpoint_when_discovery_context_changes_then_the_retained_alias_binding_survives()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let contact = Target::get(
                Source::MileSplit,
                PageKind::Contacts,
                "https://school.example.edu/staff".into(),
                PageContext {
                    school_id: Some("731".into()),
                    school_name: Some("Retained School".into()),
                    ..PageContext::default()
                },
            );
            let retained = page::bind_target(&database, contact.clone(), &scope).await?;
            let mut rediscovered = contact.clone();
            rediscovered.context.school_name = Some("Rediscovered School".into());
            let actual = page::bind_target(&database, rediscovered, &scope).await?;
            assert_eq!(actual.binding, retained.binding);
            let rows: Vec<(i64, Value)> = sqlx::query_as("SELECT id,cursor FROM scrape_pages")
                .fetch_all(&test.pool)
                .await?;
            assert_eq!(rows, [(actual.binding.page_id(), to_value(&contact)?)]);
            assert_eq!(to_value(actual.target)?, to_value(contact)?);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_a_retained_safe_checkpoint_when_incoming_transport_metadata_is_invalid_then_binding_rejects_it()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let target = page_support::profile();
            let retained = page::bind_target(&database, target.clone(), &scope).await?;
            let mut invalid = target.clone();
            invalid.browser_origin = Some("https://wi.milesplit.com/".into());
            let error = page::bind_target(&database, invalid, &scope)
                .await
                .err()
                .context("invalid browser context must be rejected")?;
            assert!(format!("{error:#}").contains("browser context and request origin differ"));
            let rows: Vec<(i64, Value)> = sqlx::query_as("SELECT id,cursor FROM scrape_pages")
                .fetch_all(&test.pool)
                .await?;
            assert_eq!(rows, [(retained.binding.page_id(), to_value(target)?)]);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_distinct_method_body_or_context_when_bound_then_each_request_keeps_its_own_checkpoint()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let get = page_support::profile();
            let mut post = get.clone();
            post.method = Some("POST".into());
            let mut post_body = post.clone();
            post_body.body = Some(json!({"selection": 2}));
            let mut distinct_context = get.clone();
            distinct_context.context.season = Some(2026);
            let mut ids = BTreeSet::new();
            for target in [get, post, post_body, distinct_context] {
                let request = page::bind_target(&database, target.clone(), &scope).await?;
                assert!(ids.insert(request.binding.page_id()));
                assert_eq!(to_value(request.target)?, to_value(target)?);
            }
            let actual: Vec<i64> = sqlx::query_scalar("SELECT id FROM scrape_pages ORDER BY id")
                .fetch_all(&test.pool)
                .await?;
            assert_eq!(actual, ids.into_iter().collect::<Vec<_>>());
            Ok(())
        })
    })
    .await
}
