use super::{RetryPolicy, test_support::*};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::with_database,
        page::{self, PageOutcome, PageScope, ReservationDecision},
    },
};
use anyhow::{Context, Result};

pub(super) fn configured() -> Result<crate::config::Config> {
    crate::config::Config::parse(
        "states=['IA']\n[limits]\nmax_attempts=3\n[acquisition]\nmode='public_results'\n[sources.athleticnet]\nenabled=false",
    )
}

pub(super) fn source_target() -> crate::model::Target {
    crate::model::Target::get(
        crate::model::Source::MileSplit,
        crate::model::PageKind::Athlete,
        "https://ia.milesplit.com/athletes/1-runner".into(),
        crate::model::PageContext {
            state: Some("IA".into()),
            athlete_id: Some("1".into()),
            ..Default::default()
        },
    )
}

pub(super) fn bound(
    config: &crate::config::Config,
    now: i64,
    generation: &str,
    parents: &[&str],
) -> Result<PageScope> {
    let mut value = scope(config, now)?;
    value.run_generation = generation.into();
    value.inherited_retry_campaigns = parents.iter().map(|value| (*value).to_owned()).collect();
    Ok(value)
}

pub(super) async fn fail(
    database: &Database,
    config: &crate::config::Config,
    scope: &PageScope,
    now: i64,
) -> Result<()> {
    let selected = page::select(database, config, scope, now)
        .await?
        .context("eligible page")?;
    let ReservationDecision::Reserved { reservation } =
        page::reserve_configured(database, config, &selected, now).await?
    else {
        anyhow::bail!("reservation required")
    };
    database
        .durable_fail_with_policy(
            &reservation,
            failure(now),
            &RetryPolicy::configured(config, &selected)?,
        )
        .await?;
    Ok(())
}

pub(super) async fn history(
    database: &Database,
    config: &crate::config::Config,
    now: i64,
    attempts: u32,
) -> Result<()> {
    database.enqueue(&[source_target()], false).await?;
    for index in 0..attempts {
        let phase = ["schools", "contacts", "results"][usize::try_from(index)?];
        fail(
            database,
            config,
            &bound(config, now, &format!("old-mixed-{phase}"), &[])?,
            now + i64::from(index) * 10_000,
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_inherited_exhaustion_when_selected_then_settled_without_attempt_and_no_reselection()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database = Database::durable_test_database(pool.clone());
        let config = configured()?;
        let now = chrono::Utc::now().timestamp_millis();
        history(&database, &config, now, 3).await?;
        for phase in ["schools", "contacts", "results"] {
            let scope = bound(&config, now, &format!("new-iowa-{phase}"), &["old-mixed"])?;
            let request = page::select(&database, &config, &scope, now + 60_000).await?.context("retained failed page")?;
            let outcome = page::reserve_configured(&database, &config, &request, now + 60_000).await?;
            assert!(matches!(outcome, ReservationDecision::Settled { outcome: PageOutcome::Exhausted { detail } } if detail.contains("3/3")));
            assert!(page::select(&database, &config, &scope, now + 60_000).await?.is_none());
            let before = snapshot(&database).await?;
            assert!(matches!(page::reserve_configured(&database, &config, &request, now + 60_000).await?, ReservationDecision::Settled { outcome: PageOutcome::Exhausted { .. } }));
            assert_eq!(snapshot(&database).await?, before);
        }
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_attempts").fetch_one(pool).await?, 3);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?, 0);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_two_ancestor_attempts_when_child_retries_then_only_one_shared_attempt_remains()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = configured()?;
            let now = chrono::Utc::now().timestamp_millis();
            history(&database, &config, now, 2).await?;
            let scope = bound(&config, now, "new-iowa-schools", &["old-mixed"])?;
            let request = page::select(&database, &config, &scope, now + 60_000)
                .await?
                .context("one attempt remaining")?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &config, &request, now + 60_000).await?
            else {
                anyhow::bail!("remaining attempt required")
            };
            assert_eq!(reservation.total_attempts, 3);
            assert_eq!(database.durable_acquisition_attempts(&request).await?, 3);
            let before = snapshot(&database).await?;
            let ReservationDecision::Reserved {
                reservation: replay,
            } = page::reserve_configured(&database, &config, &request, now + 60_000).await?
            else {
                anyhow::bail!("unfinished attempt must replay")
            };
            assert_eq!(replay.attempt_id, reservation.attempt_id);
            assert_eq!(snapshot(&database).await?, before);
            let outcome = database
                .durable_fail_with_policy(
                    &reservation,
                    failure(now + 60_000),
                    &RetryPolicy::configured(&config, &request)?,
                )
                .await?;
            assert!(matches!(outcome, PageOutcome::Exhausted { .. }));
            let next = bound(&config, now, "new-iowa-results", &["old-mixed"])?;
            let next = page::select(&database, &config, &next, now + 120_000)
                .await?
                .context("new phase retains exhausted review")?;
            assert!(matches!(
                page::reserve_configured(&database, &config, &next, now + 120_000).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Exhausted { .. }
                }
            ));
            for (offset, year) in [2028, 2029].into_iter().enumerate() {
                let mut future = config.clone();
                future.graduation_years = vec![year];
                future.validate()?;
                let independent = bound(&future, now, &format!("new-{year}-schools"), &[])?;
                let independent = page::select(&database, &future, &independent, now + 120_000)
                    .await?
                    .context("explicit independent future cohort")?;
                let ReservationDecision::Reserved { reservation } =
                    page::reserve_configured(&database, &future, &independent, now + 120_000)
                        .await?
                else {
                    anyhow::bail!("independent future cohort reservation")
                };
                assert_eq!(reservation.total_attempts, 4 + i32::try_from(offset)?);
                assert_eq!(
                    database.durable_acquisition_attempts(&independent).await?,
                    1
                );
                database
                    .durable_fail_with_policy(
                        &reservation,
                        failure(now + 120_000),
                        &RetryPolicy::configured(&future, &independent)?,
                    )
                    .await?;
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_sibling_campaigns_when_reservations_race_then_one_remaining_attempt_cannot_be_duplicated()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = configured()?;
            let now = chrono::Utc::now().timestamp_millis();
            history(&database, &config, now, 2).await?;
            let a = bound(&config, now, "new-iowa-schools", &["old-mixed"])?;
            let b = bound(&config, now, "new-illinois-results", &["old-mixed"])?;
            let a = page::select(&database, &config, &a, now + 60_000)
                .await?
                .context("sibling A")?;
            let b = page::select(&database, &config, &b, now + 60_000)
                .await?
                .context("sibling B")?;
            let (left, right) = tokio::join!(
                page::reserve_configured(&database, &config, &a, now + 60_000),
                page::reserve_configured(&database, &config, &b, now + 60_000)
            );
            let (owner, denied) = match (left, right) {
                (Ok(ReservationDecision::Reserved { reservation }), Err(error))
                | (Err(error), Ok(ReservationDecision::Reserved { reservation })) => {
                    (reservation, error)
                }
                _ => anyhow::bail!("exactly one sibling must own the last attempt"),
            };
            assert!(
                denied
                    .to_string()
                    .contains("another durable operation owns unfinished work")
            );
            let request = if owner.request.binding == a.binding {
                &b
            } else {
                &a
            };
            let policy = RetryPolicy::configured(&config, &owner.request)?;
            database
                .durable_fail_with_policy(&owner, failure(now + 60_000), &policy)
                .await?;
            assert!(matches!(
                page::reserve_configured(&database, &config, request, now + 120_000).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Exhausted { .. }
                }
            ));
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
                    .fetch_one(pool)
                    .await?,
                3
            );
            Ok(())
        })
    })
    .await
}
