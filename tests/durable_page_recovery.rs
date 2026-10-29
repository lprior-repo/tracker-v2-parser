#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    durable::page::{self, FailureClass, FailureRecord, PageOutcome, ReservationDecision},
};

#[tokio::test]
async fn transient_bursts_defer_durably_preserve_counts_and_allow_the_next_school() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = page_support::config()?;
            let scope = page_support::scope(&config)?;
            let first = page_support::target("101");
            let second = page_support::target("102");
            let request = page_support::request(&database, &first, &scope).await?;
            database
                .enqueue(std::slice::from_ref(&second), false)
                .await?;
            let mut now = chrono::Utc::now().timestamp_millis();
            for ordinal in 1..=3 {
                let reservation = page_support::reserve(&database, &request, now).await?;
                assert_eq!(reservation.cycle, 1);
                assert_eq!(reservation.ordinal, ordinal);
                let before = page_support::snapshots(&database).await?;
                let replay = page_support::reserve(&database, &request, now).await?;
                assert_eq!(replay.attempt_id, reservation.attempt_id);
                assert_eq!(page_support::snapshots(&database).await?, before);
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
                let PageOutcome::Deferred { retry_at_ms, cycle } = outcome else {
                    anyhow::bail!("expected durable deferral")
                };
                assert_eq!(cycle, 1);
                assert_eq!(
                    page::select(&database, &config, &scope, now)
                        .await?
                        .context("independent school")?
                        .target
                        .url,
                    second.url
                );
                let before = page_support::snapshots(&database).await?;
                assert!(matches!(
                    page::reserve(&database, &request, now).await?,
                    ReservationDecision::Settled {
                        outcome: PageOutcome::Deferred { .. }
                    }
                ));
                assert_eq!(page_support::snapshots(&database).await?, before);
                now = retry_at_ms;
            }
            let reserved = page_support::reserve(&database, &request, now).await?;
            assert_eq!(
                (reserved.cycle, reserved.ordinal, reserved.total_attempts),
                (2, 1, 4)
            );
            let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM scrape_pages WHERE id=$1")
                .bind(request.binding.page_id)
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(attempts, 4);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
                    .fetch_one(&test.pool)
                    .await?,
                4
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn legacy_dns_failure_enters_an_audited_cycle_without_resetting_original_attempts()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let config = page_support::config()?;
        let scope = page_support::scope(&config)?;
        let target = page_support::target("201");
        let request = page_support::request(&database,&target,&scope).await?;
        sqlx::query("UPDATE scrape_pages SET attempts=3,status='failed',last_error='resolving source host: failed to lookup address information: nodename nor servname provided',notes=ARRAY['original evidence'] WHERE id=$1")
            .bind(request.binding.page_id).execute(&test.pool).await?;
        assert!(database.next_profile_coverage_job(None,3,&config.states,&config.graduation_years,false).await?.is_none());
        database.check_profile_coverage_exhausted(track_scrapper_v2::model::Source::MileSplit,3,&config.states,&config.graduation_years,false).await.expect_err("legacy barrier preserved");
        assert_eq!(page::select(&database,&config,&scope,chrono::Utc::now().timestamp_millis()).await?.context("durable legacy recovery")?.binding.page_id,request.binding.page_id);
        let reserved = page_support::reserve(&database,&request,chrono::Utc::now().timestamp_millis()).await?;
        assert_eq!(reserved.total_attempts,4);
        assert_eq!(sqlx::query_scalar::<_,i32>("SELECT original_attempts FROM durable_page_operations").fetch_one(&test.pool).await?,3);
        assert_eq!(sqlx::query_scalar::<_,Vec<String>>("SELECT notes FROM scrape_pages").fetch_one(&test.pool).await?,vec!["original evidence"]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn malformed_legacy_page_and_expired_cycle_do_not_starve_other_school_work() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let config = page_support::config()?;
        let scope = page_support::scope(&config)?;
        let request = page_support::request(&database,&page_support::target("301"),&scope).await?;
        database.enqueue(&[page_support::target("302")],false).await?;
        sqlx::query("UPDATE scrape_pages SET attempts=3,status='failed',last_error='invalid required athlete identity',notes=ARRAY['original'] WHERE id=$1")
            .bind(request.binding.page_id).execute(&test.pool).await?;
        let original: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE id=$1").bind(request.binding.page_id).fetch_one(&test.pool).await?;
        assert!(matches!(page::reserve(&database,&request,chrono::Utc::now().timestamp_millis()).await?,ReservationDecision::Settled {outcome:PageOutcome::Rejected {..}}));
        assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT to_jsonb(p) FROM scrape_pages p WHERE id=$1").bind(request.binding.page_id).fetch_one(&test.pool).await?,original);
        let next=page::select(&database,&config,&scope,chrono::Utc::now().timestamp_millis()).await?.context("other school")?;
        let reservation=page_support::reserve(&database,&next,chrono::Utc::now().timestamp_millis()).await?;
        let outcome=database.durable_fail(&reservation,FailureRecord {class:FailureClass::Transient,detail:"connection timed out",capture:None,stopped_origin:None,observed_at_ms:scope.deadline_ms}).await?;
        assert!(matches!(outcome,PageOutcome::Exhausted {..}));
        let before=page_support::snapshots(&database).await?;
        assert!(matches!(page::reserve(&database,&next,scope.deadline_ms).await?,ReservationDecision::Settled {outcome:PageOutcome::Exhausted {..}}));
        assert_eq!(page_support::snapshots(&database).await?,before);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn another_generation_cannot_hijack_an_unfinished_page_even_after_the_old_deadline()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let original =
                page_support::request(&database, &page_support::target("401"), &scope).await?;
            let reserved =
                page_support::reserve(&database, &original, chrono::Utc::now().timestamp_millis())
                    .await?;
            let mut next_scope = scope.clone();
            next_scope.run_generation = "new-explicit-generation".into();
            next_scope.deadline_ms = scope
                .deadline_ms
                .checked_add(3_600_000)
                .context("next deadline")?;
            let next = page::request(
                track_scrapper_v2::database::Job {
                    id: original.binding.page_id,
                    target: original.target.clone(),
                },
                &next_scope,
            )?;
            let before = page_support::snapshots(&database).await?;
            let error = page::reserve(&database, &next, scope.deadline_ms)
                .await
                .expect_err("incomplete prior effect requires explicit handoff");
            assert!(format!("{error:#}").contains("explicit handoff required"));
            assert_eq!(page_support::snapshots(&database).await?, before);
            let replayed = page_support::reserve(&database, &original, scope.deadline_ms).await?;
            assert_eq!(replayed.attempt_id, reserved.attempt_id);
            assert_eq!(page_support::snapshots(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn due_earlier_contact_retries_rotate_behind_untouched_profiles_and_later_retry_rounds()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        use track_scrapper_v2::{config::Config,model::{Athlete,Capture,PageKind,ParsedPage,School}};
        let database=Database::connect(&test.url).await?;
        let config=Config::parse("states=['OH']\ngraduation_years=[2027]\nseason=2026\n[acquisition]\nmode='public_results'\nprofile_coverage_only=true\ncollect_contacts=true\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\nenabled=true\ntransport='http'\n")?;
        let scope=page_support::scope(&config)?;
        let first_team=page_support::target("501");
        let mut contact=first_team.clone();
        contact.kind=PageKind::Contacts;
        contact.url="https://earlier.example.org/".into();
        contact.context.subject_id=Some("contacts-depth:0".into());
        let second_team=page_support::target("502");
        let mut profile=page_support::profile();
        profile.context.school_id=Some("502".into());
        let first_page=ParsedPage {schools:vec![School {id:"501".into(),name:"Earlier School".into(),state:Some("OH".into()),..School::default()}],next:vec![contact.clone()],..ParsedPage::default()};
        let second_page=ParsedPage {schools:vec![School {id:"502".into(),name:"Later School".into(),state:Some("OH".into()),..School::default()}],athletes:vec![Athlete {id:"15005189".into(),name:"Brice Fuller".into(),state:Some("OH".into()),school_id:Some("502".into()),graduation_year:Some(2027),profile_url:Some(profile.url.clone()),..Athlete::default()}],next:vec![profile.clone()],..ParsedPage::default()};
        for (target,parsed) in [(&first_team,&first_page),(&second_team,&second_page)] {
            database.persist(None,target,&Capture {url:target.url.clone(),status:200,content_type:"application/json".into(),body:serde_json::to_vec(parsed)?},parsed).await?;
        }
        let mut now=chrono::Utc::now().timestamp_millis();
        let first=page::select(&database,&config,&scope,now).await?.context("first school contact")?;
        assert_eq!(first.target.url,contact.url);
        let reserved=page_support::reserve(&database,&first,now).await?;
        now=defer_round(&database,&reserved,now).await?;
        assert_eq!(database.next_profile_coverage_job(None,3,&config.states,&config.graduation_years,true).await?.context("unchanged legacy school ordering")?.target.url,contact.url);
        let mut seen=std::collections::BTreeMap::<String,i32>::new();
        seen.insert(contact.url.clone(),1);
        for expected in [&profile.url,&contact.url,&profile.url,&contact.url,&profile.url,&contact.url,&profile.url] {
            let selected=page::select(&database,&config,&scope,now).await?.context("fair eligible page")?;
            assert_eq!(&selected.target.url,expected,"scope-local retry rounds must rotate before school ordering");
            let reservation=page_support::reserve(&database,&selected,now).await?;
            let attempts=seen.entry(expected.clone()).or_default();
            *attempts+=1;
            assert_eq!((reservation.cycle,reservation.ordinal),((*attempts-1)/3+1,(*attempts-1)%3+1));
            assert_eq!(reservation.total_attempts,*attempts);
            now=defer_round(&database,&reservation,now).await?;
        }
        assert_eq!(seen.get(&contact.url),Some(&4));
        assert_eq!(seen.get(&profile.url),Some(&4));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_attempts").fetch_one(&test.pool).await?,8);
        database.close().await;
        Ok(())
    })).await
}

async fn defer_round(
    database: &Database,
    reservation: &page::PageReservation,
    now: i64,
) -> Result<i64> {
    match database
        .durable_fail(
            reservation,
            FailureRecord {
                class: FailureClass::Transient,
                detail: "DNS resolution failed",
                capture: None,
                stopped_origin: None,
                observed_at_ms: now,
            },
        )
        .await?
    {
        PageOutcome::Deferred { retry_at_ms, .. } => Ok(retry_at_ms),
        outcome => anyhow::bail!("expected retry round, got {outcome:?}"),
    }
}
