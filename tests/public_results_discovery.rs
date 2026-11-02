#[path = "public_results_discovery/support.rs"]
mod discovery;
#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::Result;
use discovery::{PUBLIC, config, done, register, selected, snapshot, target};
use track_scrapper_v2::{
    database::{Database, JobClass},
    durable::page,
    model::{PageKind, Source, Sport},
    sources,
};

#[tokio::test]
async fn given_registered_profiles_when_discovery_falls_back_then_articles_aliases_and_unknown_tabs_stay_unchanged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = config("OH", PUBLIC, "milesplit")?;
            let scope = page_support::scope(&config)?;
            let base = register(&database, "8001", Some(2027)).await?;
            let unknown = register(&database, "8002", None).await?;
            let outside = register(&database, "8003", Some(2028)).await?;
            let mut covered = Vec::new();
            for original in [&base, &unknown] {
                done(&database, original).await?;
                let mut alias = original.clone();
                alias.context.sport = Some(Sport::Indoor);
                covered.push(alias);
            }
            for original in [&base, &unknown, &outside] {
                for suffix in ["stats", "progression", "articles", "photos", "videos"] {
                    let mut followup = original.clone();
                    followup.url.push_str(&format!("/{suffix}"));
                    if original.url == base.url && matches!(suffix, "stats" | "progression") {
                        done(&database, &followup).await?;
                        followup.context.sport = Some(Sport::Indoor);
                    }
                    covered.push(followup);
                }
            }
            database.enqueue(&covered, false).await?;
            let before = snapshot(&database).await?;
            assert!(
                page::select(
                    &database,
                    &config,
                    &scope,
                    chrono::Utc::now().timestamp_millis()
                )
                .await?
                .is_none()
            );
            assert_eq!(snapshot(&database).await?, before);
            assert!(
                database
                    .next_job(Some(Source::MileSplit), 3, &["OH".into()])
                    .await?
                    .is_some()
            );
            let legacy = discovery::config("OH", "", "milesplit")?;
            let legacy_scope = page_support::scope(&legacy)?;
            assert!(
                page::select(
                    &database,
                    &legacy,
                    &legacy_scope,
                    chrono::Utc::now().timestamp_millis()
                )
                .await?
                .is_some()
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_known_and_unknown_registered_profiles_when_selected_then_bases_and_only_known_followups_run()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = config("OH", PUBLIC, "milesplit")?;
            let scope = page_support::scope(&config)?;
            let known = register(&database, "8011", Some(2027)).await?;
            let unknown = register(&database, "8012", None).await?;
            let outside = register(&database, "8013", Some(2028)).await?;
            let mut queued = vec![outside];
            for base in [&known, &unknown] {
                queued.push(base.clone());
                for suffix in ["stats", "progression"] {
                    let mut tab = base.clone();
                    tab.url.push_str(&format!("/{suffix}"));
                    queued.push(tab);
                }
            }
            database.enqueue(&queued, false).await?;
            for expected in [
                &known.url,
                &format!("{}/stats", known.url),
                &format!("{}/progression", known.url),
                &unknown.url,
            ] {
                let selected = selected(&database, &config, &scope).await?;
                assert_eq!(&selected.target.url, expected);
                done(&database, &selected.target).await?;
            }
            let before = snapshot(&database).await?;
            assert!(
                page::select(
                    &database,
                    &config,
                    &scope,
                    chrono::Utc::now().timestamp_millis()
                )
                .await?
                .is_none()
            );
            assert_eq!(snapshot(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_blank_roster_name_when_base_is_unreserved_then_public_discovery_reads_it_and_specialized_tabs_continue()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let config=config("OH",PUBLIC,"milesplit")?;let scope=page_support::scope(&config)?;
        let roster=target(PageKind::Roster,"https://oh.milesplit.com/teams/10002-mason/roster",Some("10002"),None);
        let html="<html><head><link rel='canonical' href='https://oh.milesplit.com/teams/10002-mason/roster'></head><body><div class='jumbotron-content'><h1>Mason</h1></div><div id='rosterDataset'><li class='athlete-row'><a href='https://oh.milesplit.com/athletes/15005189-brice-fuller'></a><span class='column-grad-year'>2027</span></li></div></body></html>";
        let parsed=sources::parse(&roster,html)?;
        assert!(parsed.athletes.is_empty());assert_eq!(parsed.next.len(),1);
        discovery::replay(&database,test.directory(),&scope,roster,html).await?;
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM athlete_sources").fetch_one(database.pool()).await?,0);
        let base=selected(&database,&config,&scope).await?;
        assert_eq!(base.target.context.athlete_id.as_deref(),Some("15005189"));
        let first=page_support::reserve(&database,&base,chrono::Utc::now().timestamp_millis()).await?;
        assert_eq!(page_support::reserve(&database,&base,chrono::Utc::now().timestamp_millis()).await?.attempt_id,first.attempt_id);
        let published_2028=std::fs::read_to_string(support::fixture("milesplit/athlete-oh-15005189.html"))?;
        assert!(published_2028.contains("Class of 2028"));
        let html=published_2028.replace("Class of 2028","Class of 2027");
        discovery::replay(&database,test.directory(),&scope,base.target,&html).await?;
        assert_eq!(sqlx::query_scalar::<_,Option<i32>>("SELECT graduation_year FROM athlete_sources WHERE source_id='15005189'").fetch_one(database.pool()).await?,Some(2027));
        assert!(selected(&database,&config,&scope).await?.target.url.ends_with("/stats"));
        database.close().await;Ok(())
    })).await
}

#[tokio::test]
async fn given_new_discovery_when_parent_aliases_succeeded_then_new_school_roster_directory_and_meet_remain_eligible()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let config=config("OH",PUBLIC,"milesplit")?;let scope=page_support::scope(&config)?;
        let team=target(PageKind::Team,"https://oh.milesplit.com/teams/9001-school",Some("9001"),None);
        let mut roster=team.clone();roster.kind=PageKind::Roster;roster.url.push_str("/roster");
        done(&database,&team).await?;done(&database,&roster).await?;
        let mut alias=team.clone();alias.context.sport=Some(Sport::Indoor);alias.url="https://oh.milesplit.com/teams/9001-published-alias".into();
        let mut alias_roster=roster.clone();alias_roster.context.sport=Some(Sport::Indoor);alias_roster.url="https://oh.milesplit.com/teams/9001-published-alias/roster".into();
        let fresh=target(PageKind::Team,"https://oh.milesplit.com/teams/9002-new",Some("9002"),None);
        let mut fresh_roster=fresh.clone();fresh_roster.kind=PageKind::Roster;fresh_roster.url.push_str("/roster");
        let http_school=target(PageKind::Team,"http://oh.milesplit.com/teams/9004-new",Some("9004"),None);
        let directory=target(PageKind::Directory,"https://oh.milesplit.com/teams",None,None);
        let meet=target(PageKind::Meet,"https://oh.milesplit.com/meets/9003/results",None,None);
        database.enqueue(&[alias.clone(),alias_roster.clone(),fresh.clone(),fresh_roster.clone(),directory.clone(),meet.clone(),http_school.clone()],false).await?;
        let before:Vec<serde_json::Value>=sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE status='pending' AND cursor->'context'->>'school_id'='9001' ORDER BY id").fetch_all(database.pool()).await?;
        for expected in [&fresh,&fresh_roster] {let job=selected(&database,&config,&scope).await?;assert_eq!(job.target.url,expected.url);done(&database,&job.target).await?;}
        for _ in 0..3 {let job=selected(&database,&config,&scope).await?;assert!(job.target.url==directory.url||job.target.url==meet.url||job.target.url==http_school.url);done(&database,&job.target).await?;}
        assert!(page::select(&database,&config,&scope,chrono::Utc::now().timestamp_millis()).await?.is_none());
        assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT to_jsonb(p) FROM scrape_pages p WHERE status='pending' AND cursor->'context'->>'school_id'='9001' ORDER BY id").fetch_all(database.pool()).await?,before);
        database.close().await;Ok(())
    })).await
}

#[tokio::test]
async fn given_complete_official_contacts_when_specialized_queue_is_empty_then_website_does_not_fall_through()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            discovery::official(&database).await?;
            let config = config("WI", PUBLIC, "milesplit")?;
            let scope = page_support::scope(&config)?;
            let before = snapshot(&database).await?;
            assert!(
                page::select(
                    &database,
                    &config,
                    &scope,
                    chrono::Utc::now().timestamp_millis()
                )
                .await?
                .is_none()
            );
            assert_eq!(snapshot(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_athleticnet_public_results_when_generic_queue_runs_then_its_athlete_jobs_are_unchanged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = config("OH", PUBLIC, "athleticnet")?;
            let scope = page_support::scope(&config)?;
            let mut athlete = target(
                PageKind::Athlete,
                "https://www.athletic.net/athlete/28872883",
                None,
                Some("28872883"),
            );
            athlete.source = Source::AthleticNet;
            database
                .enqueue(std::slice::from_ref(&athlete), false)
                .await?;
            assert_eq!(
                selected(&database, &config, &scope).await?.target.url,
                athlete.url
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_preserved_source_stop_when_restricted_discovery_runs_then_it_remains_blocked_without_counter_changes()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let config=config("OH",PUBLIC,"milesplit")?;let scope=page_support::scope(&config)?;
        let stop=target(PageKind::Athlete,"https://oh.milesplit.com/athletes/9901-stop",None,Some("9901"));
        database.enqueue(std::slice::from_ref(&stop),false).await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=2,last_error='HTTP403 source stop'").execute(database.pool()).await?;
        let before=snapshot(&database).await?;
        assert!(page::select(&database,&config,&scope,chrono::Utc::now().timestamp_millis()).await.is_err());
        assert_eq!(snapshot(&database).await?,before);
        database.close().await;Ok(())
    })).await
}

fn query_meet(id: &str, state: &str) -> track_scrapper_v2::model::Target {
    let mut page = target(
        PageKind::Meet,
        &format!(
            "https://{state_lower}.milesplit.com/meets/{id}-meet",
            state_lower = state.to_lowercase()
        ),
        None,
        None,
    );
    page.context.state = Some(state.to_owned());
    page
}

async fn durable_sequence(
    database: &Database,
    config: &track_scrapper_v2::config::Config,
    expected: &[track_scrapper_v2::model::Target],
) -> Result<Vec<(i64, serde_json::Value)>> {
    let scope = page_support::scope(config)?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut observed = Vec::with_capacity(expected.len());
    for target in expected {
        let job = database
            .next_durable_page(config, &scope, now)
            .await?
            .ok_or_else(|| anyhow::anyhow!("expected queued public discovery page"))?;
        assert_eq!(
            serde_json::to_value(&job.target)?,
            serde_json::to_value(target)?
        );
        let stored: serde_json::Value =
            sqlx::query_scalar("SELECT cursor FROM scrape_pages WHERE id=$1")
                .bind(job.id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(stored, serde_json::to_value(target)?);
        sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE id=$1")
            .bind(job.id)
            .execute(database.pool())
            .await?;
        observed.push((job.id, stored));
    }
    assert!(
        database
            .next_durable_page(config, &scope, now)
            .await?
            .is_none()
    );
    sqlx::query("UPDATE scrape_pages SET status='pending'")
        .execute(database.pool())
        .await?;
    Ok(observed)
}

async fn legacy_sequence(
    database: &Database,
    config: &track_scrapper_v2::config::Config,
    expected: &[track_scrapper_v2::model::Target],
) -> Result<Vec<(i64, serde_json::Value)>> {
    let mut observed = Vec::with_capacity(expected.len());
    for target in expected {
        let job = database
            .next_job_for_cohort(
                Some(Source::MileSplit),
                config.limits.max_attempts,
                JobClass::Profile,
                &config.states,
                &config.graduation_years,
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("expected queued legacy selection"))?;
        assert_eq!(
            serde_json::to_value(&job.target)?,
            serde_json::to_value(target)?
        );
        let stored: serde_json::Value =
            sqlx::query_scalar("SELECT cursor FROM scrape_pages WHERE id=$1")
                .bind(job.id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(stored, serde_json::to_value(target)?);
        sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE id=$1")
            .bind(job.id)
            .execute(database.pool())
            .await?;
        observed.push((job.id, stored));
    }
    assert!(
        database
            .next_job_for_cohort(
                Some(Source::MileSplit),
                config.limits.max_attempts,
                JobClass::Profile,
                &config.states,
                &config.graduation_years,
            )
            .await?
            .is_none()
    );
    Ok(observed)
}

fn query_config(states: &str) -> Result<track_scrapper_v2::config::Config> {
    track_scrapper_v2::config::Config::parse(&format!(
        "states={states}\ngraduation_years=[2027]\nseason=2026\n{PUBLIC}\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\nenabled=true\ntransport='http'\n"
    ))
}

#[tokio::test]
async fn given_multiple_state_filters_when_durable_fallback_repeats_then_order_and_cursors_match_legacy_selection()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = query_config("['OH','WI']")?;
            let expected = (0..8)
                .map(|index| {
                    query_meet(
                        &format!("repeated-{index}"),
                        if index % 2 == 0 { "OH" } else { "WI" },
                    )
                })
                .collect::<Vec<_>>();
            database.enqueue(&expected, false).await?;
            let optimized = durable_sequence(&database, &config, &expected).await?;
            let legacy = legacy_sequence(&database, &config, &expected).await?;
            assert_eq!(optimized, legacy);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_multiple_configured_states_when_durable_fallback_runs_then_only_matching_rows_keep_database_order()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = query_config("['OH','WI']")?;
            let queued = vec![
                query_meet("multi-oh", "OH"),
                query_meet("multi-wi", "WI"),
                query_meet("multi-ia", "IA"),
            ];
            database.enqueue(&queued, false).await?;
            let optimized = durable_sequence(&database, &config, &queued[..2]).await?;
            let legacy = legacy_sequence(&database, &config, &queued[..2]).await?;
            assert_eq!(optimized, legacy);
            let remaining: i64 =
                sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE status='pending'")
                    .fetch_one(database.pool())
                    .await?;
            database.close().await;
            assert_eq!(remaining, 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_all_configured_states_when_durable_fallback_runs_then_each_state_is_returned_in_id_order()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = query_config("['OH','WI','IA']")?;
            let expected = vec![
                query_meet("all-oh", "OH"),
                query_meet("all-wi", "WI"),
                query_meet("all-ia", "IA"),
            ];
            database.enqueue(&expected, false).await?;
            let optimized = durable_sequence(&database, &config, &expected).await?;
            let legacy = legacy_sequence(&database, &config, &expected).await?;
            assert_eq!(optimized, legacy);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_no_eligible_rows_when_durable_fallback_runs_then_optimized_and_legacy_apis_return_none()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = query_config("['IA']")?;
            let scope = page_support::scope(&config)?;
            assert!(
                database
                    .next_durable_page(&config, &scope, chrono::Utc::now().timestamp_millis())
                    .await?
                    .is_none()
            );
            assert!(
                database
                    .next_job_for_cohort(
                        Some(Source::MileSplit),
                        config.limits.max_attempts,
                        JobClass::Profile,
                        &config.states,
                        &config.graduation_years,
                    )
                    .await?
                    .is_none()
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_a_registered_profile_outside_the_selected_cohort_when_public_fallback_runs_then_the_profile_stays_pending_and_unattempted()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = config("OH", PUBLIC, "milesplit")?;
            let scope = page_support::scope(&config)?;
            let registered = register(&database, "queue-policy-2028", Some(2028)).await?;
            database
                .enqueue(std::slice::from_ref(&registered), false)
                .await?;
            let before = snapshot(&database).await?;
            assert!(
                database
                    .next_durable_page(&config, &scope, chrono::Utc::now().timestamp_millis())
                    .await?
                    .is_none()
            );
            let after = snapshot(&database).await?;
            let state: (String, i32) =
                sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE cursor=$1")
                    .bind(serde_json::to_value(&registered)?)
                    .fetch_one(database.pool())
                    .await?;
            database.close().await;
            assert_eq!(after, before);
            assert_eq!(state, ("pending".into(), 0));
            Ok(())
        })
    })
    .await
}
