use anyhow::{Context, Result};
use serde_json::Value;

use super::{Scheduling, select_scheduled};
use crate::{
    config::Config,
    database::Database,
    durable::{
        model::page_config_digest,
        pacing::test_support::with_database,
        page::{self, FailureClass, FailureRecord, PageOutcome, PageScope, ReservationDecision},
    },
    model::{PageContext, PageKind, Source, Target},
};

#[test]
fn configured_reservation_intervals_drive_public_selection_turns() -> Result<()> {
    for interval in [1, 2, 8, 128] {
        let config = Config::parse(&format!(
            "states=['MI']\n[limits]\npublic_results_every={interval}"
        ))?;
        assert!(matches!(
            Scheduling::for_turn(&config, interval - 1),
            Scheduling::PublicBulkTurn
        ));
        if interval > 1 {
            assert!(matches!(
                Scheduling::for_turn(&config, 0),
                Scheduling::Resume
            ));
        }
    }
    Ok(())
}

fn config(focused: bool) -> Result<Config> {
    Config::parse(&format!(
        "states=['WI']\ngraduation_years=[2027]\n[acquisition]\nmode='public_results'\ncollect_contacts=true\nprofile_coverage_only={focused}\n[sources.athleticnet]\nenabled=false\n"
    ))
}

fn target(kind: PageKind, url: &str) -> Target {
    Target::get(
        Source::MileSplit,
        kind,
        url.into(),
        PageContext {
            state: Some("WI".into()),
            school_id: Some("101".into()),
            ..Default::default()
        },
    )
}

fn scope(config: &Config, now: i64) -> Result<PageScope> {
    Ok(PageScope {
        run_generation: "bounded-school-sweep".into(),
        config_sha256: page_config_digest(config)?,
        deadline_ms: now + 3_600_000,
        cooldown_ms: 60_000,
    })
}

async fn snapshot(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(d) ORDER BY operation_id) FROM durable_page_operations d),'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a))").fetch_one(database.pool()).await?)
}

#[tokio::test]
async fn focused_workflow_sweep_skips_due_deferred_pages_without_destroying_retry_state()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config(true)?;
            let now = chrono::Utc::now().timestamp_millis();
            let scope = scope(&config, now)?;
            let team = target(PageKind::Team, "https://wi.milesplit.com/teams/101-school");
            database.enqueue(std::slice::from_ref(&team), false).await?;
            let request = page::select(&database, &config, &scope, now)
                .await?
                .context("initial team")?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &request, now).await?
            else {
                anyhow::bail!("team reservation absent");
            };
            let PageOutcome::Deferred { retry_at_ms, .. } = database
                .durable_fail(
                    &reservation,
                    FailureRecord {
                        class: FailureClass::Transient,
                        detail: "DNS lookup failed",
                        capture: None,
                        stopped_origin: None,
                        observed_at_ms: now,
                    },
                )
                .await?
            else {
                anyhow::bail!("deferred team absent");
            };
            let before = snapshot(&database).await?;
            for at in [now, retry_at_ms, retry_at_ms + 100_000] {
                assert!(
                    select_scheduled(
                        &database,
                        &config,
                        &scope,
                        at,
                        &[],
                        Scheduling::for_turn(&config, 0)
                    )
                    .await?
                    .is_none()
                );
            }
            assert_eq!(
                database.durable_due(&scope.run_generation).await?,
                Some(retry_at_ms)
            );
            assert_eq!(
                page::select(&database, &config, &scope, retry_at_ms)
                    .await?
                    .context("saved retry remains eligible")?
                    .binding,
                request.binding
            );
            assert_eq!(snapshot(&database).await?, before);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn public_results_reserves_each_eighth_turn_for_bulk_even_when_contacts_are_ready()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config(false)?;
            let now = chrono::Utc::now().timestamp_millis();
            let scope = scope(&config, now)?;
            let team = target(PageKind::Team, "https://wi.milesplit.com/teams/101-school");
            let contact = target(PageKind::Contacts, "https://school.example.edu/staff");
            let meet = target(PageKind::Meet, "https://wi.milesplit.com/meets/900-meet");
            database
                .enqueue(&[team.clone(), contact.clone(), meet.clone()], false)
                .await?;
            sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE url=$1")
                .bind(&team.url)
                .execute(pool)
                .await?;
            let before = snapshot(&database).await?;
            for turn in 0..16 {
                let selected = select_scheduled(
                    &database,
                    &config,
                    &scope,
                    now,
                    &[],
                    Scheduling::for_turn(&config, turn),
                )
                .await?
                .context("scheduled page")?;
                assert_eq!(
                    &selected.target.url,
                    if turn % 8 == 7 {
                        &meet.url
                    } else {
                        &contact.url
                    }
                );
            }
            assert_eq!(snapshot(&database).await?, before);
            let roster = target(
                PageKind::Roster,
                "https://wi.milesplit.com/teams/101-school/roster",
            );
            let mut unknown = target(
                PageKind::Athlete,
                "https://wi.milesplit.com/athletes/999-unknown",
            );
            unknown.context.athlete_id = Some("999".into());
            database
                .enqueue(&[roster.clone(), unknown.clone()], false)
                .await?;
            assert_eq!(
                select_scheduled(
                    &database,
                    &config,
                    &scope,
                    now,
                    &[],
                    Scheduling::for_turn(&config, 7)
                )
                .await?
                .context("bulk precedes roster and unknown profile")?
                .target
                .url,
                meet.url
            );
            assert_eq!(
                select_scheduled(
                    &database,
                    &config,
                    &scope,
                    now,
                    &[],
                    Scheduling::for_turn(&config, 6)
                )
                .await?
                .context("normal turn preserves roster priority")?
                .target
                .url,
                roster.url
            );
            sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE url=ANY($1)")
                .bind(vec![roster.url, unknown.url])
                .execute(pool)
                .await?;
            sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE url=$1")
                .bind(&meet.url)
                .execute(pool)
                .await?;
            assert_eq!(
                select_scheduled(
                    &database,
                    &config,
                    &scope,
                    now,
                    &[],
                    Scheduling::for_turn(&config, 7)
                )
                .await?
                .context("empty bulk turn falls back")?
                .target
                .url,
                contact.url
            );
            Ok(())
        })
    })
    .await
}
