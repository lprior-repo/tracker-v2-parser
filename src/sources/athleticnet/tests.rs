mod edges;

use anyhow::Result;

use crate::model::{PageContext, PageKind, Source, Sport, Target};

use super::{athlete_target, meet_target, parse, seeds, team_target};

const BIO: &str = include_str!("../../../fixtures/athleticnet/bio-28872883.json");
const MEET: &str = include_str!("../../../fixtures/athleticnet/meet-634313.json");
const RESULTS: &str = include_str!("../../../fixtures/athleticnet/results-634313.json");
const TEAM: &str = include_str!("../../../fixtures/athleticnet/team-3204.json");
const CORE: &str = include_str!("../../../fixtures/athleticnet/team-core-3204.json");
const ROSTER: &str = include_str!("../../../fixtures/athleticnet/roster-3204.json");
const NAV: &str = include_str!("../../../fixtures/athleticnet/navigation.json");
const ROOT: &str = include_str!("../../../fixtures/athleticnet/directory-root.json");
const WI: &str = include_str!("../../../fixtures/athleticnet/directory-wi.json");
const RANKINGS: &str = include_str!("../../../fixtures/athleticnet/rankings-public.json");

#[test]
fn given_public_career_when_parsed_then_all_marks_and_evidence_are_retained() -> Result<()> {
    let target = athlete_target("28872883", Some("WI"), Sport::Outdoor)?;
    let page = parse(&target, BIO)?;
    assert_eq!(page.athletes[0].name, "Kingston Penn");
    assert_eq!(page.athletes[0].grade.as_deref(), Some("11"));
    assert_eq!(page.athletes[0].graduation_year, None);
    assert_eq!(page.schools[0].name, "Middleton");
    assert!(page.schools[0].state.is_none());
    assert_eq!(page.performances.len(), 41);
    assert!(page.performances.iter().all(|p| !p.is_relay));
    assert_eq!(page.unassigned_results.len(), 12);
    assert!(page.unassigned_results.iter().all(|p| p.is_relay));
    assert!(
        page.unassigned_results
            .iter()
            .any(|p| p.members.iter().any(|id| id == "athleticnet:28872883"))
    );
    let row = page
        .performances
        .iter()
        .find(|p| p.result_id.as_deref() == Some("258445410"))
        .unwrap();
    assert_eq!((&*row.event, &*row.mark), ("200 Meters", "23.33a"));
    assert_eq!(row.meet_name.as_deref(), Some("Waunakee Frosh/Soph Invite"));
    assert_eq!(row.date.as_deref(), Some("2025-05-01"));
    assert_eq!(row.season, Some(2025));
    assert_eq!(row.grade.as_deref(), Some("10"));
    assert_eq!(row.gender.as_deref(), Some("male"));
    assert_eq!(row.timing.as_deref(), Some("FAT"));
    assert!(page.next.iter().any(|t| t.url.contains("sport=xc")));
    Ok(())
}

#[test]
fn given_no_state_hint_when_bio_parsed_then_state_is_not_invented() -> Result<()> {
    let target = athlete_target("28872883", None, Sport::Outdoor)?;
    let page = parse(&target, BIO)?;
    assert!(page.schools.iter().all(|school| school.state.is_none()));
    assert!(page.athletes[0].state.is_none());
    assert!(page.notes.iter().any(|n| n.contains("no state hint")));
    assert_eq!(page.performances.len() + page.unassigned_results.len(), 53);
    Ok(())
}

#[test]
fn given_meet_capture_when_parsed_then_results_require_transient_token_refresh() -> Result<()> {
    let target = meet_target("634313", Sport::Outdoor)?;
    let page = parse(&target, MEET)?;
    let next = &page.next[0];
    assert_eq!(next.kind, PageKind::Results);
    assert_eq!(next.token_url.as_deref(), Some(target.url.as_str()));
    assert!(
        next.url
            .ends_with("meetId=634313&sport=tf&rawResults=false&showTips=false")
    );
    assert_eq!(next.context.meet_name.as_deref(), Some("Big 8 Conference"));
    assert_eq!(next.context.meet_date.as_deref(), Some("2026-05-15"));
    assert!(next.body.is_none());
    assert!(next.browser_origin.is_some());
    Ok(())
}

#[test]
fn given_whole_meet_when_parsed_then_relay_team_ids_never_become_people() -> Result<()> {
    let metadata = parse(&meet_target("634313", Sport::Outdoor)?, MEET)?;
    let page = parse(&metadata.next[0], RESULTS)?;
    assert_eq!(page.performances.len(), 686);
    assert!(page.performances.iter().all(|p| !p.is_relay));
    assert_eq!(page.unassigned_results.len(), 72);
    assert!(page.unassigned_results.iter().all(|p| p.is_relay));
    assert_eq!(
        page.unassigned_results
            .iter()
            .map(|p| p.members.len())
            .sum::<usize>(),
        288
    );
    let relay = page
        .unassigned_results
        .iter()
        .find(|p| p.result_id.as_deref() == Some("292352545"))
        .unwrap();
    assert_eq!(relay.mark, "42.41a");
    assert!(relay.members.iter().any(|id| id == "athleticnet:26935452"));
    assert_eq!(relay.raw["row"]["AthleteID"], 32305456);
    assert_eq!(page.athletes.len(), 516);
    assert_eq!(page.schools.len(), 11);
    assert!(!page.athletes.iter().any(|a| a.id == "32305456"));
    let row = page
        .performances
        .iter()
        .find(|p| p.result_id.as_deref() == Some("292277081"))
        .unwrap();
    assert_eq!(row.athlete_id, "28872883");
    assert_eq!(row.mark, "10.41a");
    assert_eq!(row.meet_name.as_deref(), Some("Big 8 Conference"));
    assert!(page.schools.iter().all(|s| s.state.is_none()));
    Ok(())
}

#[test]
fn given_school_capture_when_parsed_then_real_postal_address_survives() -> Result<()> {
    let target = team_target("3204", None, Sport::Outdoor, 2026)?;
    let page = parse(&target, TEAM)?;
    let school = &page.schools[0];
    assert_eq!(school.name, "Middleton");
    assert_eq!(school.address.as_deref(), Some("2100 Bristol St"));
    assert_eq!(school.city.as_deref(), Some("Middleton"));
    assert_eq!(school.state.as_deref(), Some("WI"));
    assert_eq!(school.postal_code.as_deref(), Some("53562-2699"));
    assert!(page.coaches.is_empty());
    assert!(page.next[0].url.contains("TeamHome/GetTeamCore"));
    Ok(())
}

#[test]
fn given_live_school_core_when_roster_parsed_then_public_athletes_are_discovered() -> Result<()> {
    let target = team_target("3204", Some("WI"), Sport::Outdoor, 2026)?;
    let core = parse(&target, CORE)?;
    let roster = &core.next[0];
    let page = parse(roster, ROSTER)?;
    assert_eq!(roster.kind, PageKind::Roster);
    assert_eq!(roster.token_url.as_deref(), Some(target.url.as_str()));
    assert_eq!(
        roster.url,
        "https://www.athletic.net/api/v1/TeamHome/GetAthletes?seasonId=2026"
    );
    assert_eq!(page.athletes.len(), 208);
    assert_eq!(page.athletes[0].id, "25401378");
    assert_eq!(page.athletes[0].name, "Abby Arnold");
    assert_eq!(page.athletes[0].gender.as_deref(), Some("female"));
    assert_eq!(page.athletes[0].school_id.as_deref(), Some("3204"));
    assert_eq!(page.next.len(), 208);
    assert!(page.next[0].url.contains("athleteId=25401378&sport=tf"));
    Ok(())
}

#[test]
fn given_national_navigation_when_filtered_then_real_state_tree_and_school_routes_are_used()
-> Result<()> {
    let seeds = seeds(&["WI".to_owned()], 2026)?;
    let nav = seeds
        .iter()
        .find(|t| t.url.contains("indoor=false"))
        .unwrap();
    let page = parse(nav, NAV)?;
    let wi = page
        .next
        .iter()
        .find(|t| t.url.contains("GetDivChildren"))
        .unwrap();
    let schools = parse(wi, WI)?;
    assert!(wi.url.ends_with("divId=170770"));
    assert_eq!(wi.context.state.as_deref(), Some("WI"));
    assert_eq!(page.next.len(), 3);
    assert!(
        page.next
            .iter()
            .filter(|t| t.method.as_deref() == Some("POST"))
            .count()
            == 2
    );
    assert!(
        schools
            .schools
            .iter()
            .any(|s| s.name == "Argyle" && s.id == "3341")
    );
    assert!(schools.schools.iter().all(|s| s.website.is_none()));
    assert!(schools.next.iter().any(|t| t.url.contains("sport=xc")));
    Ok(())
}

#[test]
fn given_root_null_state_keys_when_parsed_then_published_child_names_resolve_states() -> Result<()>
{
    let target = Target::get(
        Source::AthleticNet,
        PageKind::Directory,
        "https://www.athletic.net/api/v1/SiteHeader/GetDivChildren?sport=tf&divId=168416"
            .to_owned(),
        PageContext {
            subject_id: Some("WI".to_owned()),
            season: Some(2026),
            ..PageContext::default()
        },
    );
    let page = parse(&target, ROOT)?;
    assert_eq!(page.next.len(), 1);
    assert!(page.next[0].url.ends_with("divId=170770"));
    assert_eq!(page.next[0].context.state.as_deref(), Some("WI"));
    Ok(())
}

#[test]
fn given_blurred_rankings_when_parsed_then_masked_population_is_reported_not_inferred() -> Result<()>
{
    let nav = seeds(&["WI".to_owned()], 2026)?
        .into_iter()
        .find(|t| t.url.contains("indoor=false"))
        .unwrap();
    let discovered = parse(&nav, NAV)?;
    let request = discovered
        .next
        .iter()
        .find(|t| t.method.as_deref() == Some("POST"))
        .unwrap();
    let page = parse(request, RANKINGS)?;
    assert_eq!(page.athletes.len(), 5);
    assert!(page.athletes.iter().any(|a| a.id == "28872883"));
    assert!(page.notes.iter().any(|n| n.contains("96 ranking rows")));
    assert!(!page.next.iter().any(|t| t.method.is_some()));
    assert!(page.performances.is_empty());
    Ok(())
}

#[test]
fn given_confirmed_indoor_and_xc_cores_when_parsed_then_published_season_ids_scope_rosters()
-> Result<()> {
    let indoor = include_str!("../../../fixtures/athleticnet/team-core-3204-tfi.json");
    let xc = include_str!("../../../fixtures/athleticnet/team-core-3204-xc.json");
    let indoor_target = team_target("3204", Some("WI"), Sport::Indoor, 2026)?;
    let xc_target = team_target("3204", Some("WI"), Sport::CrossCountry, 2026)?;
    let indoor = parse(&indoor_target, indoor)?;
    let xc = parse(&xc_target, xc)?;
    assert!(indoor_target.url.contains("sport=tfi&year=2026"));
    assert!(xc_target.url.contains("sport=xc&year=2026"));
    assert!(indoor.next[0].url.ends_with("seasonId=12026"));
    assert!(xc.next[0].url.ends_with("seasonId=2026"));
    assert_eq!(
        indoor.next[0].token_url.as_deref(),
        Some(indoor_target.url.as_str())
    );
    assert_eq!(xc.next[0].context.sport, Some(Sport::CrossCountry));
    Ok(())
}
