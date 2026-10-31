mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    config::Config,
    database::Database,
    durable::{
        model::page_config_digest,
        page::{self, FailureClass, FailureRecord, PageOutcome, PageScope, ReservationDecision},
    },
    export::ReviewStatus,
    model::{PageContext, PageKind, Source, Target},
};

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

#[tokio::test]
async fn exhausted_contact_stays_on_review_and_yields_to_independent_results_without_cooldown()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = Config::parse(
                "states=['WI']\ngraduation_years=[2027]\n[limits]\nmax_attempts=3\n[acquisition]\nmode='public_results'\ncollect_contacts=true\nprofile_coverage_only=false\n[sources.athleticnet]\nenabled=false\n[export]\nreview_sheet=true\nlayout='single_sheet'\n",
            )?;
            let mut now = chrono::Utc::now().timestamp_millis();
            let scope = PageScope {
                run_generation: "three-total-attempts-results".into(),
                config_sha256: page_config_digest(&config)?,
                deadline_ms: now + 3_600_000,
                cooldown_ms: 60_000,
            };
            let team = target(PageKind::Team, "https://wi.milesplit.com/teams/101-school");
            let contact = target(PageKind::Contacts, "https://school.example.edu/staff");
            let result = target(PageKind::Meet, "https://wi.milesplit.com/meets/900-meet");
            database.enqueue(&[team.clone(), contact.clone()], false).await?;
            sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE url=$1")
                .bind(&team.url)
                .execute(database.pool())
                .await?;
            let request = page::select(&database, &config, &scope, now)
                .await?
                .context("contact must initially be selected")?;
            assert_eq!(serde_json::to_value(&request.target)?, serde_json::to_value(&contact)?);
            for attempt in 1..=3 {
                let ReservationDecision::Reserved { reservation } =
                    page::reserve(&database, &request, now).await?
                else {
                    anyhow::bail!("budget unexpectedly ended before attempt {attempt}")
                };
                assert_eq!(reservation.total_attempts, attempt);
                let outcome = database
                    .durable_fail(
                        &reservation,
                        FailureRecord {
                            class: FailureClass::Transient,
                            detail: "DNS resolution failed",
                            capture: None,
                            stopped_origin: None,
                            observed_at_ms: now,
                        },
                    )
                    .await?;
                if attempt < 3 {
                    let PageOutcome::Deferred { retry_at_ms, .. } = outcome else {
                        anyhow::bail!("retry before total budget must be deferred")
                    };
                    now = retry_at_ms;
                } else {
                    assert!(matches!(outcome, PageOutcome::Exhausted { .. }));
                }
            }
            assert_eq!(database.durable_due(&scope.run_generation).await?, None);
            let before: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_build_object('operation',to_jsonb(d),'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a)) FROM durable_page_operations d WHERE operation_id=$1",
            )
            .bind(&request.binding.operation_id)
            .fetch_one(database.pool())
            .await?;
            for at in [now, now + scope.cooldown_ms, scope.deadline_ms] {
                assert!(matches!(page::reserve(&database, &request, at).await?, ReservationDecision::Settled { outcome: PageOutcome::Exhausted { .. } }));
            }
            let after: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_build_object('operation',to_jsonb(d),'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a)) FROM durable_page_operations d WHERE operation_id=$1",
            )
            .bind(&request.binding.operation_id)
            .fetch_one(database.pool())
            .await?;
            assert_eq!(after, before);
            database.enqueue(std::slice::from_ref(&result), false).await?;
            assert_eq!(
                page::select(&database, &config, &scope, now)
                    .await?
                    .context("independent result must be selectable immediately")?
                    .target.url,
                result.url
            );
            let mut snapshot = database
                .begin_export_snapshot(&config.states, &config.graduation_years)
                .await?;
            let rows = snapshot.review_records(0, 1000).await?;
            snapshot.finish().await?;
            assert_eq!(rows.len(), 1);
            let row = rows.first().context("exhausted contact Review row")?;
            assert_eq!(row.checkpoint_id, request.binding.page_id);
            assert_eq!(row.url, contact.url);
            assert_eq!(row.status, ReviewStatus::Failed);
            assert_eq!(row.attempts, 3);
            assert_eq!(row.durable_state, "exhausted");
            assert_eq!(row.retry_after, "");
            assert_eq!(row.error, "DNS resolution failed");
            assert_eq!(row.next_action, "Investigate exhausted retry budget; preserve cumulative attempt counters.");
            assert!(!row.ai_guidance.contains("Wait for the automatic retry"));
            assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(database.pool()).await?, 0);
            assert_eq!(database.durable_due(&scope.run_generation).await?, None);
            database.close().await;
            Ok(())
        })
    })
    .await
}
