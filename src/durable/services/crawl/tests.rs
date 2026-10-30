use anyhow::Result;

use super::{BatchProgress, CrawlPhase, CrawlRequest, QueueState, next_phase};
use crate::config::Config;

fn request(phase: CrawlPhase, focused: bool, contacts: bool) -> Result<CrawlRequest> {
    Ok(CrawlRequest {
        generation: "fair-phase-progression".into(),
        config: Config::parse(&format!(
            "states=['WI']\n[acquisition]\nmode='public_results'\nprofile_coverage_only={focused}\ncollect_contacts={contacts}\n[sources.athleticnet]\nenabled=false\n"
        ))?,
        deadline_ms: 120_000,
        cooldown_ms: 15_000,
        batch: 0,
        max_batches: 100,
        phase,
    })
}

#[test]
fn focused_school_sweep_advances_with_saved_deferrals_without_claiming_completion() -> Result<()> {
    let idle = BatchProgress {
        queue: QueueState::NoEligiblePage,
        deferred: 27,
        ..Default::default()
    };
    let first = request(CrawlPhase::SchoolChains, true, true)?;
    assert_eq!(
        next_phase(&first, &idle, Some(60_000)),
        CrawlPhase::ContactRefresh
    );
    assert_eq!(
        next_phase(
            &request(CrawlPhase::SchoolChains, true, false)?,
            &idle,
            Some(60_000)
        ),
        CrawlPhase::PublicResults
    );
    let results = first.next(CrawlPhase::PublicResults)?;
    assert!(!results.config.acquisition.profile_coverage_only());
    assert_eq!(
        next_phase(&results, &idle, Some(60_000)),
        CrawlPhase::PublicResults
    );
    assert_eq!(next_phase(&results, &idle, None), CrawlPhase::Complete);
    assert_eq!(
        next_phase(&first, &BatchProgress::default(), Some(60_000)),
        CrawlPhase::SchoolChains
    );
    Ok(())
}

#[test]
fn broad_school_runs_still_wait_for_deferrals_and_contact_refresh_advances_normally() -> Result<()>
{
    let idle = BatchProgress {
        queue: QueueState::NoEligiblePage,
        ..Default::default()
    };
    let broad = request(CrawlPhase::SchoolChains, false, true)?;
    assert_eq!(
        next_phase(&broad, &idle, Some(60_000)),
        CrawlPhase::SchoolChains
    );
    assert_eq!(next_phase(&broad, &idle, None), CrawlPhase::ContactRefresh);
    assert_eq!(
        next_phase(
            &request(CrawlPhase::ContactRefresh, true, true)?,
            &idle,
            None
        ),
        CrawlPhase::PublicResults
    );
    Ok(())
}
