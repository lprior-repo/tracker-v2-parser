#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    durable::page::{self, FailureClass, FailureRecord, PageOutcome, ReservationDecision},
    model::{PageKind, Sport},
};

#[tokio::test]
async fn given_three_owned_pages_when_selecting_again_then_no_page_is_selected_twice_and_replay_keeps_the_attempt()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = page_support::config()?;
            let scope = page_support::scope(&config)?;
            database
                .enqueue(
                    &[
                        page_support::target("701"),
                        page_support::target("702"),
                        page_support::target("703"),
                    ],
                    false,
                )
                .await?;
            let now = chrono::Utc::now().timestamp_millis();
            let mut reservations = Vec::new();
            let mut owned = Vec::new();
            for _ in 0..3 {
                let request = page::select_excluding(&database, &config, &scope, now, &owned)
                    .await?
                    .context("independent page")?;
                assert!(
                    !reservations
                        .iter()
                        .any(|r: &page::PageReservation| r.request.binding.page_id
                            == request.binding.page_id)
                );
                owned.push(request.binding.page_id);
                reservations.push(page_support::reserve(&database, &request, now).await?);
            }
            assert!(
                page::select_excluding(&database, &config, &scope, now, &owned)
                    .await?
                    .is_none()
            );
            let first = &reservations[0];
            assert_eq!(
                page::select(&database, &config, &scope, now)
                    .await?
                    .context("interrupted owner can resume")?
                    .binding,
                first.request.binding
            );
            let replay = page_support::reserve(&database, &first.request, now).await?;
            assert_eq!(replay.attempt_id, first.attempt_id);
            assert_eq!(
                sqlx::query_scalar::<_, Vec<i32>>(
                    "SELECT array_agg(attempts ORDER BY id) FROM scrape_pages"
                )
                .fetch_one(&test.pool)
                .await?,
                vec![1, 1, 1]
            );
            let PageOutcome::Deferred { retry_at_ms, .. } = database
                .durable_fail(
                    first,
                    FailureRecord {
                        class: FailureClass::Transient,
                        detail: "connection timed out",
                        capture: None,
                        stopped_origin: None,
                        observed_at_ms: now,
                    },
                )
                .await?
            else {
                anyhow::bail!("deferred outcome")
            };
            assert!(
                page::select_excluding(&database, &config, &scope, now, &owned)
                    .await?
                    .is_none()
            );
            assert_eq!(
                page::select_excluding(&database, &config, &scope, retry_at_ms, &owned)
                    .await?
                    .context("due retry")?
                    .binding,
                first.request.binding
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_an_owned_team_when_pipeline_selects_then_its_alias_and_roster_wait_for_that_parent()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = page_support::config()?;
            let scope = page_support::scope(&config)?;
            let team = page_support::target("711");
            let mut alias = team.clone();
            alias.url.push_str("-published");
            let mut roster = team.clone();
            roster.kind = PageKind::Roster;
            roster.url.push_str("/roster");
            let other = page_support::target("712");
            database
                .enqueue(&[team.clone(), alias, roster, other.clone()], false)
                .await?;
            let now = chrono::Utc::now().timestamp_millis();
            let first = page::select(&database, &config, &scope, now)
                .await?
                .context("team")?;
            assert_eq!(first.target.url, team.url);
            page_support::reserve(&database, &first, now).await?;
            let next =
                page::select_excluding(&database, &config, &scope, now, &[first.binding.page_id])
                    .await?
                    .context("independent school")?;
            assert_eq!(next.target.url, other.url);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
                    .fetch_one(&test.pool)
                    .await?,
                1
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_an_owned_athlete_when_pipeline_selects_then_only_a_different_athlete_can_run_in_parallel()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let config=page_support::config()?;
        let scope=page_support::scope(&config)?;
        let school:i64=sqlx::query_scalar("INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit','721','Pipeline school','OH','https://oh.milesplit.com/teams/721') RETURNING id").fetch_one(&test.pool).await?;
        let mut profiles=Vec::new();
        for id in ["15005189","15005190"] {
            let athlete:i64=sqlx::query_scalar("INSERT INTO athletes(name,graduation_year,school_id,state) VALUES('Pipeline athlete',2027,$1,'OH') RETURNING id").bind(school).fetch_one(&test.pool).await?;
            let mut target=page_support::profile(); target.url=format!("https://oh.milesplit.com/athletes/{id}-runner"); target.context.athlete_id=Some(id.into()); target.context.school_id=Some("721".into());
            sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,school_id,graduation_year) VALUES($1,'milesplit',$2,$3,$4,2027)").bind(athlete).bind(id).bind(&target.url).bind(school).execute(&test.pool).await?;
            profiles.push(target);
        }
        let mut alias=profiles[0].clone(); alias.context.sport=Some(Sport::Outdoor);
        let mut stats=profiles[0].clone(); stats.url.push_str("/stats");
        database.enqueue(&[profiles[0].clone(),alias,stats,profiles[1].clone()],false).await?;
        let now=chrono::Utc::now().timestamp_millis();
        let first=page::select(&database,&config,&scope,now).await?.context("first athlete")?;
        assert_eq!(first.target.context.athlete_id,profiles[0].context.athlete_id);
        page_support::reserve(&database,&first,now).await?;
        let next=page::select_excluding(&database,&config,&scope,now,&[first.binding.page_id]).await?.context("second athlete")?;
        assert_eq!(next.target.context.athlete_id,profiles[1].context.athlete_id);
        assert!(matches!(page::reserve(&database,&first,now).await?,ReservationDecision::Reserved{..}));
        database.close().await; Ok(())
    })).await
}

#[tokio::test]
async fn given_null_school_context_when_stats_is_owned_then_same_athlete_progression_waits()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let config=page_support::config()?;
        let scope=page_support::scope(&config)?;
        let school:i64=sqlx::query_scalar("INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit','731','Source-linked school','OH','https://oh.milesplit.com/teams/731') RETURNING id").fetch_one(&test.pool).await?;
        let mut profiles=Vec::new();
        for id in ["15005189","15005190"] {
            let athlete:i64=sqlx::query_scalar("INSERT INTO athletes(name,graduation_year,school_id,state) VALUES('Null-context athlete',2027,$1,'OH') RETURNING id").bind(school).fetch_one(&test.pool).await?;
            let mut target=page_support::profile();
            target.url=format!("https://oh.milesplit.com/athletes/{id}-runner");
            target.context.athlete_id=Some(id.into());
            target.context.school_id=None;
            sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,school_id,graduation_year) VALUES($1,'milesplit',$2,$3,$4,2027)").bind(athlete).bind(id).bind(&target.url).bind(school).execute(&test.pool).await?;
            profiles.push(target);
        }
        let mut stats=profiles[0].clone();stats.url.push_str("/stats");
        let mut progression=profiles[0].clone();progression.url.push_str("/progression");
        database.enqueue(&[profiles[0].clone(),stats.clone(),progression,profiles[1].clone()],false).await?;
        sqlx::query("UPDATE scrape_pages SET status='succeeded',attempts=1,last_success=clock_timestamp() WHERE url=$1").bind(&profiles[0].url).execute(&test.pool).await?;
        let now=chrono::Utc::now().timestamp_millis();
        let first=page::select(&database,&config,&scope,now).await?.context("published stats")?;
        assert_eq!(first.target.url,stats.url);
        assert!(first.target.context.school_id.is_none());
        let reserved=page_support::reserve(&database,&first,now).await?;
        let next=page::select_excluding(&database,&config,&scope,now,&[first.binding.page_id]).await?.context("independent athlete")?;
        assert_eq!(next.target.url,profiles[1].url);
        assert_ne!(next.target.context.athlete_id,first.target.context.athlete_id);
        assert_eq!(page::select(&database,&config,&scope,now).await?.context("interrupted stats can resume")?.binding,first.binding);
        assert_eq!(page_support::reserve(&database,&first,now).await?.attempt_id,reserved.attempt_id);
        database.close().await;
        Ok(())
    })).await
}
