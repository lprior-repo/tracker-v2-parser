use anyhow::{Context, Result};

use super::{Scheduling, select_scheduled, select_scheduled_with_extension};
use crate::{
    config::Config,
    database::{Database, Job},
    durable::{
        model::{DeadlineExtensionAuthorization, page_config_digest},
        pacing::test_support::with_database,
        page::{
            self, FailureClass, FailureRecord, PageOutcome, PageScope, ReservationDecision,
            RetryPolicy,
        },
    },
    model::{PageContext, PageKind, Source, Target},
};

fn config() -> Result<Config> {
    Config::parse(
        "states=['WI']\n[acquisition]\nmode='public_results'\ncollect_contacts=true\n[sources.athleticnet]\nenabled=false",
    )
}

fn target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Team,
        "https://wi.milesplit.com/teams/731-wisconsin".into(),
        PageContext {
            state: Some("WI".into()),
            school_id: Some("731".into()),
            ..Default::default()
        },
    )
}

fn scope(config: &Config, deadline_ms: i64) -> Result<PageScope> {
    Ok(PageScope {
        run_generation: "authorized-deadline-extension-schools".into(),
        config_sha256: page_config_digest(config)?,
        deadline_ms,
        cooldown_ms: 60_000,
        inherited_retry_campaigns: Vec::new(),
    })
}

async fn snapshot(database: &Database) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(d) ORDER BY operation_id) FROM durable_page_operations d),'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a))")
        .fetch_one(database.pool())
        .await?)
}

#[tokio::test]
async fn authorized_deadline_extension_replays_immutable_owner_and_preserves_attempt_budget()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config()?;
            let now = chrono::Utc::now().timestamp_millis();
            let original_deadline = now + 1_500;
            let old_scope = scope(&config, original_deadline)?;
            let target = target();
            database
                .enqueue(std::slice::from_ref(&target), false)
                .await?;
            let original =
                select_scheduled(&database, &config, &old_scope, now, &[], Scheduling::Resume)
                    .await?
                    .context("initial page owner")?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &original, now).await?
            else {
                anyhow::bail!("initial reservation missing")
            };
            let PageOutcome::Deferred { retry_at_ms, .. } = database
                .durable_fail(
                    &reservation,
                    FailureRecord {
                        class: FailureClass::Transient,
                        detail: "temporary upstream failure",
                        capture: None,
                        stopped_origin: None,
                        observed_at_ms: now,
                    },
                )
                .await?
            else {
                anyhow::bail!("expected a retryable deferred result")
            };
            let original_row: serde_json::Value = sqlx::query_scalar(
                "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1",
            )
            .bind(&original.binding.operation_id)
            .fetch_one(pool)
            .await?;
            let mut extended_scope = old_scope.clone();
            extended_scope.deadline_ms = now + 60_000;
            let authorization = DeadlineExtensionAuthorization {
                previous_deadline_ms: original_deadline,
                effective_deadline_ms: extended_scope.deadline_ms,
            };
            let resume_at = original_deadline + 1;

            assert!(
                select_scheduled_with_extension(
                    &database,
                    &config,
                    &extended_scope,
                    retry_at_ms - 1,
                    &[],
                    Scheduling::Resume,
                    Some(&authorization),
                )
                .await?
                .is_none()
            );

            let ordinary = select_scheduled(
                &database,
                &config,
                &extended_scope,
                resume_at,
                &[],
                Scheduling::Resume,
            )
            .await?
            .context("ordinary selection")?;
            let before_rejected_identity = snapshot(&database).await?;
            let error = page::reserve(&database, &ordinary, resume_at)
                .await
                .err()
                .context("changed deadline must not steal an operation identity")?;
            assert!(
                error
                    .to_string()
                    .contains("another immutable operation owner")
            );
            assert_eq!(snapshot(&database).await?, before_rejected_identity);

            let wrong = DeadlineExtensionAuthorization {
                previous_deadline_ms: original_deadline - 1,
                effective_deadline_ms: extended_scope.deadline_ms,
            };
            let before_wrong_authority = snapshot(&database).await?;
            let error = select_scheduled_with_extension(
                &database,
                &config,
                &extended_scope,
                resume_at,
                &[],
                Scheduling::Resume,
                Some(&wrong),
            )
            .await
            .err()
            .context("wrong previous deadline must be rejected")?;
            assert!(error.to_string().contains("not compatible"));
            assert_eq!(snapshot(&database).await?, before_wrong_authority);

            let continued = select_scheduled_with_extension(
                &database,
                &config,
                &extended_scope,
                resume_at,
                &[],
                Scheduling::Resume,
                Some(&authorization),
            )
            .await?
            .context("authorized retained page")?;
            assert_eq!(continued.binding, original.binding);
            assert_eq!(
                continued
                    .deadline_extension
                    .as_ref()
                    .map(|value| value.owner_operation_id.as_str()),
                Some(original.binding.operation_id.as_str())
            );
            assert_eq!(
                continued.effective_deadline_ms(),
                extended_scope.deadline_ms
            );
            let ReservationDecision::Reserved {
                reservation: resumed,
            } = page::reserve(&database, &continued, resume_at).await?
            else {
                anyhow::bail!("authorized continuation was not reserved")
            };
            assert_eq!(resumed.attempt_id, page::attempt_identity(&original, 1, 2)?);
            assert_eq!(resumed.total_attempts, 2);
            let PageOutcome::Deferred {
                retry_at_ms: second_retry_at_ms,
                ..
            } = database
                .durable_fail(
                    &resumed,
                    FailureRecord {
                        class: FailureClass::Transient,
                        detail: "second transient failure",
                        capture: None,
                        stopped_origin: None,
                        observed_at_ms: resume_at,
                    },
                )
                .await?
            else {
                anyhow::bail!("extended deadline did not defer the second transient failure")
            };
            assert!(second_retry_at_ms > original_deadline);
            assert!(second_retry_at_ms < extended_scope.deadline_ms);
            let ReservationDecision::Reserved { reservation: last } =
                page::reserve(&database, &continued, second_retry_at_ms).await?
            else {
                anyhow::bail!("third attempt was not reserved from cumulative history")
            };
            assert_eq!(last.total_attempts, 3);
            assert!(matches!(
                database
                    .durable_fail_with_policy(
                        &last,
                        FailureRecord {
                            class: FailureClass::Transient,
                            detail: "attempt budget exhausted",
                            capture: None,
                            stopped_origin: None,
                            observed_at_ms: second_retry_at_ms,
                        },
                        &RetryPolicy::configured(&config, &continued)?,
                    )
                    .await?,
                PageOutcome::Exhausted { .. }
            ));
            let retained_row: serde_json::Value = sqlx::query_scalar(
                "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1",
            )
            .bind(&original.binding.operation_id)
            .fetch_one(pool)
            .await?;
            assert_eq!(retained_row["binding"], original_row["binding"]);
            assert_eq!(
                retained_row["original_attempts"],
                original_row["original_attempts"]
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM durable_page_attempts WHERE operation_id=$1"
                )
                .bind(&original.binding.operation_id)
                .fetch_one(pool)
                .await?,
                3
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn deadline_extension_rejects_another_config_and_preserves_owner() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config()?;
            let now = chrono::Utc::now().timestamp_millis();
            let old_deadline = now + 5_000;
            let old_scope = scope(&config, old_deadline)?;
            let target = target();
            database
                .enqueue(std::slice::from_ref(&target), false)
                .await?;
            let (id, cursor): (i64, serde_json::Value) =
                sqlx::query_as("SELECT id,cursor FROM scrape_pages WHERE cursor=$1")
                    .bind(serde_json::to_value(&target)?)
                    .fetch_one(pool)
                    .await?;
            let job = Job {
                id,
                target: serde_json::from_value(cursor)?,
            };
            let original = page::request(job, &old_scope)?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &original, now).await?
            else {
                anyhow::bail!("initial reservation missing")
            };
            database
                .durable_fail(
                    &reservation,
                    FailureRecord {
                        class: FailureClass::Transient,
                        detail: "temporary upstream failure",
                        capture: None,
                        stopped_origin: None,
                        observed_at_ms: now,
                    },
                )
                .await?;
            let before = snapshot(&database).await?;
            let mut changed = scope(&config, now + 60_000)?;
            changed.config_sha256 = "f".repeat(64);
            let authorization = DeadlineExtensionAuthorization {
                previous_deadline_ms: old_deadline,
                effective_deadline_ms: changed.deadline_ms,
            };
            let error = database
                .durable_request_with_deadline_extension(
                    Job {
                        id: original.binding.page_id,
                        target: target.clone(),
                    },
                    &changed,
                    &authorization,
                )
                .await
                .err()
                .context("configuration change must fail closed")?;
            assert!(error.to_string().contains("not compatible"));
            assert_eq!(snapshot(&database).await?, before);

            let mut wrong_cooldown = scope(&config, now + 60_000)?;
            wrong_cooldown.cooldown_ms = 15_000;
            let error = database
                .durable_request_with_deadline_extension(
                    Job {
                        id: original.binding.page_id,
                        target: target.clone(),
                    },
                    &wrong_cooldown,
                    &DeadlineExtensionAuthorization {
                        previous_deadline_ms: old_deadline,
                        effective_deadline_ms: wrong_cooldown.deadline_ms,
                    },
                )
                .await
                .err()
                .context("cooldown change must fail closed")?;
            assert!(error.to_string().contains("not compatible"));
            assert_eq!(snapshot(&database).await?, before);

            let mut wrong_lineage = scope(&config, now + 60_000)?;
            wrong_lineage.inherited_retry_campaigns = vec!["different-campaign".into()];
            let error = database
                .durable_request_with_deadline_extension(
                    Job {
                        id: original.binding.page_id,
                        target: target.clone(),
                    },
                    &wrong_lineage,
                    &DeadlineExtensionAuthorization {
                        previous_deadline_ms: old_deadline,
                        effective_deadline_ms: wrong_lineage.deadline_ms,
                    },
                )
                .await
                .err()
                .context("retry lineage change must fail closed")?;
            assert!(error.to_string().contains("not compatible"));
            assert_eq!(snapshot(&database).await?, before);

            let downgrade = DeadlineExtensionAuthorization {
                previous_deadline_ms: old_deadline,
                effective_deadline_ms: old_deadline,
            };
            let error = database
                .durable_request_with_deadline_extension(
                    Job {
                        id: original.binding.page_id,
                        target: target.clone(),
                    },
                    &scope(&config, old_deadline)?,
                    &downgrade,
                )
                .await
                .err()
                .context("equal deadline must be rejected")?;
            assert!(
                error
                    .to_string()
                    .contains("must increase the prior deadline")
            );
            assert_eq!(snapshot(&database).await?, before);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn extension_replays_only_its_exact_unfinished_owner() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config()?;
            let now = chrono::Utc::now().timestamp_millis();
            let old_scope = scope(&config, now + 5_000)?;
            let target = target();
            database
                .enqueue(std::slice::from_ref(&target), false)
                .await?;
            let (id, cursor): (i64, serde_json::Value) =
                sqlx::query_as("SELECT id,cursor FROM scrape_pages WHERE cursor=$1")
                    .bind(serde_json::to_value(&target)?)
                    .fetch_one(pool)
                    .await?;
            let original = page::request(
                Job {
                    id,
                    target: serde_json::from_value(cursor)?,
                },
                &old_scope,
            )?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &original, now).await?
            else {
                anyhow::bail!("initial reservation missing")
            };
            let before = snapshot(&database).await?;
            let extended_scope = scope(&config, now + 60_000)?;
            let continued = database
                .durable_request_with_deadline_extension(
                    Job {
                        id,
                        target: target.clone(),
                    },
                    &extended_scope,
                    &DeadlineExtensionAuthorization {
                        previous_deadline_ms: old_scope.deadline_ms,
                        effective_deadline_ms: extended_scope.deadline_ms,
                    },
                )
                .await?;
            assert_eq!(continued.binding, original.binding);
            let ReservationDecision::Reserved {
                reservation: replay,
            } = page::reserve(&database, &continued, now + 1).await?
            else {
                anyhow::bail!("same immutable owner should resume its unfinished attempt")
            };
            assert_eq!(replay.attempt_id, reservation.attempt_id);
            assert_eq!(snapshot(&database).await?, before);

            let mut unrelated_scope = extended_scope;
            unrelated_scope.run_generation = "unrelated-owner-generation".into();
            let unrelated = page::request(Job { id, target }, &unrelated_scope)?;
            let error = page::reserve(&database, &unrelated, now + 1)
                .await
                .err()
                .context("unfinished owner must not be transferred")?;
            assert!(error.to_string().contains("explicit handoff required"));
            assert_eq!(snapshot(&database).await?, before);
            Ok(())
        })
    })
    .await
}
