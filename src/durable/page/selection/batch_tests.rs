use anyhow::{Context, Result};

use super::{Scheduling, select_scheduled};
use crate::{
    config::Config,
    database::Database,
    durable::{model::page_config_digest, pacing::test_support::with_database, page::PageScope},
    model::{PageContext, PageKind, Source, Target},
};

#[test]
fn discovery_turns_continue_across_small_batches_and_keep_the_first_batch_unchanged() -> Result<()>
{
    for interval in [1, 2, 8, 128] {
        for pages in [1, 2, 4, 8, 2000] {
            let config = Config::parse(&format!(
                "[limits]\nmax_pages={pages}\npublic_results_every={interval}"
            ))?;
            for turn in 0..(interval * 3) {
                let batch = u32::try_from(turn / pages)?;
                let scheduling = Scheduling::for_batch_turn(&config, batch, turn % pages)?;
                assert_eq!(
                    matches!(scheduling, Scheduling::PublicBulkTurn),
                    turn % interval == interval - 1
                );
            }
        }
    }
    let focused = Config::parse(
        "[acquisition]\nmode='public_results'\nprofile_coverage_only=true\n[sources.athleticnet]\nenabled=false",
    )?;
    assert!(matches!(
        Scheduling::for_batch_turn(&focused, 8, 7)?,
        Scheduling::SchoolSweep
    ));
    Ok(())
}

#[tokio::test]
async fn one_page_batches_reach_public_results_despite_ready_contacts() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = Config::parse("states=['WI']\n[limits]\nmax_pages=1\npublic_results_every=8\n[acquisition]\nmode='public_results'\ncollect_contacts=true\n[sources.athleticnet]\nenabled=false")?;
            let now = chrono::Utc::now().timestamp_millis();
            let scope = PageScope {
                run_generation: "small-batch-fairness".into(),
                config_sha256: page_config_digest(&config)?,
                deadline_ms: now + 3_600_000,
                cooldown_ms: 60_000,
            };
            let target = |kind, url: &str| Target::get(Source::MileSplit, kind, url.into(), PageContext {
                state: Some("WI".into()), school_id: Some("101".into()), ..Default::default()
            });
            let team = target(PageKind::Team, "https://wi.milesplit.com/teams/101-school");
            let contact = target(PageKind::Contacts, "https://school.example.edu/staff");
            let meet = target(PageKind::Meet, "https://wi.milesplit.com/meets/900-meet");
            database.enqueue(&[team.clone(), contact.clone(), meet.clone()], false).await?;
            sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE url=$1").bind(&team.url).execute(pool).await?;
            for batch in 0..16 {
                let selected = select_scheduled(&database, &config, &scope, now, &[], Scheduling::for_batch_turn(&config, batch, 0)?).await?.context("selected page")?;
                assert_eq!(selected.target.url, if batch % 8 == 7 { &meet.url } else { &contact.url }.as_str());
            }
            Ok(())
        })
    }).await
}
