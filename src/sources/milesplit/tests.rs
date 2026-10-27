use anyhow::Result;

use crate::model::{PageContext, PageKind, Source, Sport, Target};

use super::{athlete_target, meet_target, parse, seeds, team_target};

const PROFILE: &str = include_str!("../../../fixtures/milesplit/athlete-oh-15005189.html");
const ROSTER: &str = include_str!("../../../fixtures/milesplit/roster-oh-mason.html");
const RAW: &str = include_str!("../../../fixtures/milesplit/oh_meet_770621_rs1321880_raw.html");
const AL_MEET: &str = include_str!("../../../fixtures/milesplit/meet-al-754110-results.html");

#[test]
fn given_states_when_seeded_then_schools_and_all_sports_are_discovered() -> Result<()> {
    let targets = seeds(&["OH".to_owned(), "NY".to_owned()], 2026)?;
    assert_eq!(targets.len(), 8);
    assert_eq!(
        targets.first().map(|target| target.url.as_str()),
        Some("https://oh.milesplit.com/teams")
    );
    assert!(targets.iter().any(|target| target.url
        == "https://ny.milesplit.com/results?season=cc&level=hs&year=2026&page=1"));
    assert!(
        targets
            .iter()
            .any(|target| target.url.contains("season=indoor"))
    );
    assert!(
        targets
            .iter()
            .any(|target| target.url.contains("season=outdoor"))
    );
    assert!(seeds(&["AK".to_owned()], 2026).is_err());
    assert!(athlete_target("0", None, Sport::Outdoor).is_err());
    assert!(team_target("123/evil", None, Sport::Outdoor, 2026).is_err());
    assert!(meet_target("001", Sport::Outdoor).is_err());
    Ok(())
}

#[test]
fn given_native_team_index_when_parsed_then_school_identity_and_roster_frontier_survive()
-> Result<()> {
    let target = seeds(&["OH".to_owned()], 2026)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing directory seed"))?;
    let page = parse(
        &target,
        include_str!("../../../fixtures/milesplit/teams-oh.html"),
    )?;
    assert_eq!(page.schools.len(), 977);
    let mason = page
        .schools
        .iter()
        .find(|school| school.id == "10002")
        .ok_or_else(|| anyhow::anyhow!("missing Mason"))?;
    assert_eq!(mason.name, "Mason");
    assert_eq!(mason.state.as_deref(), Some("OH"));
    assert!(
        page.next
            .iter()
            .any(|target| target.url == "https://oh.milesplit.com/teams/10002-mason")
    );
    Ok(())
}

#[test]
fn given_colorado_directory_with_unnamed_team_when_parsed_then_all_teams_are_queued() -> Result<()>
{
    let target = seeds(&["CO".to_owned()], 2026)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing Colorado directory seed"))?;
    let page = parse(
        &target,
        include_str!("../../../fixtures/milesplit/teams-co.html"),
    )?;
    assert_eq!(page.schools.len(), 429);
    assert_eq!(page.next.len(), 430);
    assert!(page.schools.iter().all(|school| !school.name.is_empty()));
    let unnamed = page
        .next
        .iter()
        .find(|target| target.url == "https://co.milesplit.com/teams/29997")
        .ok_or_else(|| anyhow::anyhow!("missing unnamed team target"))?;
    assert_eq!(unnamed.kind, PageKind::Team);
    assert_eq!(unnamed.context.school_id.as_deref(), Some("29997"));
    assert_eq!(unnamed.context.school_name, None);
    assert_eq!(unnamed.context.state.as_deref(), Some("CO"));
    assert!(page.notes.iter().any(|note| note.contains("29997")));
    let named = page
        .schools
        .iter()
        .find(|school| school.id == "14818")
        .ok_or_else(|| anyhow::anyhow!("missing Abraham Lincoln High School"))?;
    assert_eq!(named.name, "Abraham Lincoln High School");
    assert_eq!(named.city.as_deref(), Some("Denver"));
    assert!(page.next.iter().any(|target| {
        target.url == "https://co.milesplit.com/teams/14818-abraham-lincoln-high-school"
    }));
    Ok(())
}

#[test]
fn given_domestic_and_foreign_locations_when_parsed_then_country_is_respected() {
    assert_eq!(
        super::html::location("Mason, OH, USA"),
        (Some("Mason".to_owned()), Some("OH".to_owned()))
    );
    assert_eq!(
        super::html::location("OH, USA"),
        (None, Some("OH".to_owned()))
    );
    assert_eq!(
        super::html::postal("Mason, OH 45040, USA").as_deref(),
        Some("45040")
    );
    assert_eq!(super::html::location("London, ON, CAN").1, None);
}

#[test]
fn given_profile_gender_sources_when_parsed_then_progression_takes_precedence() -> Result<()> {
    let target = athlete_target("15005189", Some("OH"), Sport::Outdoor)?;
    let female = PROFILE.replace("var gender = \"m\"", "var gender = \"f\"");
    let page = parse(&target, &female)?;
    assert_eq!(
        page.athletes
            .first()
            .and_then(|athlete| athlete.gender.as_deref()),
        Some("female")
    );
    let schema_only = PROFILE.replace("var gender = \"m\"", "var gender = \"unknown\"");
    let page = parse(&target, &schema_only)?;
    assert_eq!(
        page.athletes
            .first()
            .and_then(|athlete| athlete.gender.as_deref()),
        Some("female")
    );
    Ok(())
}

#[test]
fn given_native_roster_when_parsed_then_athletes_and_public_school_details_survive() -> Result<()> {
    let mut target = team_target("10002", Some("OH"), Sport::Outdoor, 2026)?;
    target.kind = PageKind::Roster;
    target.url = "https://oh.milesplit.com/teams/10002-mason/roster".to_owned();
    let page = parse(&target, ROSTER)?;
    assert_eq!(page.athletes.len(), 319);
    let first = page
        .athletes
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing roster athlete"))?;
    assert_eq!(
        (&first.id, &first.name),
        (&"16846610".to_owned(), &"Inioluwa Adesanya".to_owned())
    );
    assert_eq!(first.gender.as_deref(), Some("female"));
    assert_eq!(first.graduation_year, Some(2029));
    assert_eq!(first.grade, None);
    assert_eq!(first.state.as_deref(), Some("OH"));
    let school = page
        .schools
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing school"))?;
    assert_eq!(
        school.address.as_deref(),
        Some("6100 S Mason-Montgomerry Rd")
    );
    assert_eq!(school.website.as_deref(), Some("http://www.runmason.com/"));
    assert_eq!(page.next.len(), 319);
    Ok(())
}

#[test]
fn given_native_school_profile_when_parsed_then_its_actual_roster_link_is_followed() -> Result<()> {
    let target = team_target("10002", Some("OH"), Sport::Outdoor, 2026)?;
    let page = parse(
        &target,
        include_str!("../../../fixtures/milesplit/team-oh-10002-mason.html"),
    )?;
    assert_eq!(page.next.len(), 1);
    assert_eq!(
        page.next.first().map(|target| target.url.as_str()),
        Some("https://oh.milesplit.com/teams/10002-mason/roster")
    );
    Ok(())
}

#[test]
fn given_native_profile_when_parsed_then_public_prs_cover_three_sports_without_unmasking()
-> Result<()> {
    let target = athlete_target("15005189", Some("OH"), Sport::Outdoor)?;
    let page = parse(&target, PROFILE)?;
    let athlete = page
        .athletes
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing athlete"))?;
    assert_eq!(athlete.name, "Brice Fuller");
    assert_eq!(athlete.school_id.as_deref(), Some("9573"));
    assert_eq!(athlete.graduation_year, Some(2028));
    assert_eq!(athlete.gender.as_deref(), Some("male"));
    assert!(athlete.same_as.is_empty());
    assert!(page.performances.iter().any(|pr| pr.sport == Sport::Outdoor
        && pr.event == "1600m"
        && pr.mark == "4:12.69"
        && pr.date.as_deref() == Some("2026-06-04")));
    assert!(
        page.performances
            .iter()
            .any(|pr| pr.sport == Sport::Indoor && pr.event == "3000m" && pr.mark == "8:26.83")
    );
    assert!(
        page.performances
            .iter()
            .any(|pr| pr.sport == Sport::CrossCountry && pr.event == "5K" && pr.mark == "14:49.06")
    );
    assert!(
        page.performances
            .iter()
            .all(|pr| pr.grade.is_none() && pr.is_personal_record)
    );
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("masked athlete historical results"))
    );
    Ok(())
}

#[test]
fn given_national_router_profile_when_parsed_then_published_school_state_wins() -> Result<()> {
    let target = athlete_target("13966799", Some("OH"), Sport::CrossCountry)?;
    let page = parse(
        &target,
        include_str!("../../../fixtures/milesplit/athlete-www-13966799.html"),
    )?;
    let athlete = page
        .athletes
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing athlete"))?;
    assert_eq!(athlete.name, "Abigail Burt");
    assert_eq!(athlete.state.as_deref(), Some("NY"));
    assert_eq!(athlete.school_id.as_deref(), Some("11187"));
    assert_eq!(athlete.graduation_year, Some(2029));
    Ok(())
}

#[test]
fn given_native_meet_index_when_parsed_then_pagination_and_actual_meets_are_queued() -> Result<()> {
    let target = seeds(&["OH".to_owned()], 2026)?
        .into_iter()
        .find(|target| target.context.sport == Some(Sport::CrossCountry))
        .ok_or_else(|| anyhow::anyhow!("missing XC index"))?;
    let page = parse(
        &target,
        include_str!("../../../fixtures/milesplit/results-oh.html"),
    )?;
    assert_eq!(
        page.next
            .iter()
            .filter(|target| target.kind == PageKind::Meet)
            .count(),
        50
    );
    let next = page
        .next
        .iter()
        .find(|target| target.kind == PageKind::Directory)
        .ok_or_else(|| anyhow::anyhow!("missing next page"))?;
    assert!(next.url.contains("page=2"));
    assert!(next.url.contains("season=cc"));
    assert!(next.url.contains("level=hs"));
    assert!(next.url.contains("year=2026"));
    Ok(())
}

#[test]
fn given_native_meet_inventory_when_parsed_then_only_public_raw_file_is_queued() -> Result<()> {
    let mut target = meet_target("770621", Sport::CrossCountry)?;
    target.url =
        "https://oh.milesplit.com/meets/770621-beaver-eastern-invite-2026/results".to_owned();
    let page = parse(
        &target,
        include_str!("../../../fixtures/milesplit/meet-oh-770621-results.html"),
    )?;
    assert_eq!(page.next.len(), 1);
    assert_eq!(
        page.next.first().map(|target| target.url.as_str()),
        Some("https://oh.milesplit.com/meets/770621/results/1321880/raw")
    );
    assert!(page.next.iter().all(|target| !target.url.contains("/api/")));
    Ok(())
}

#[test]
fn given_published_hytek_meet_when_parsed_then_rows_and_result_inventory_survive() -> Result<()> {
    let mut target = meet_target("754110", Sport::CrossCountry)?;
    target.url =
        "https://al.milesplit.com/meets/754110-northeast-alabama-championship-2026/results"
            .to_owned();
    target.context.state = Some("AL".to_owned());
    let page = parse(&target, AL_MEET)?;

    assert_eq!(page.unassigned_results.len(), 102);
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    let first = page
        .unassigned_results
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing first result"))?;
    assert_eq!(first.event, "Event 1  Girls 5k Run CC");
    assert_eq!(first.mark, "20:48.19");
    assert_eq!(first.raw["name"], "Bruce, Jaxson");
    assert_eq!(first.raw["team"], "Plainview Hi");
    assert!(first.raw["grade"].is_null());
    assert!(first.school_id.is_none());
    let fourth = page
        .unassigned_results
        .get(3)
        .ok_or_else(|| anyhow::anyhow!("missing fourth result"))?;
    assert_eq!(fourth.raw["team"], "Pisgah High School");
    assert_eq!(fourth.mark, "21:46.36");
    let last = page
        .unassigned_results
        .last()
        .ok_or_else(|| anyhow::anyhow!("missing last result"))?;
    assert_eq!(last.raw["name"], "Tucker, Jenna");
    assert_eq!(last.mark, "39:29.84");
    assert_eq!(page.next.len(), 1);
    assert_eq!(
        page.next[0].url,
        "https://al.milesplit.com/meets/754110/results/1330427/raw"
    );
    Ok(())
}

#[test]
fn given_inline_result_with_unconfirmed_canonical_when_parsed_then_inventory_is_preserved()
-> Result<()> {
    let mut target = meet_target("754110", Sport::CrossCountry)?;
    target.url =
        "https://al.milesplit.com/meets/754110-northeast-alabama-championship-2026/results"
            .to_owned();
    let body = AL_MEET.replacen(
        r#"<link rel="canonical" href="https://al.milesplit.com/meets/754110-northeast-alabama-championship-2026/results/1330423/raw" />"#,
        r#"<link rel="canonical" href="https://al.milesplit.com/meets/754110-northeast-alabama-championship-2026/results/1330427/raw" />"#,
        1,
    );
    let page = parse(&target, &body)?;
    assert_eq!(page.unassigned_results.len(), 102);
    assert_eq!(page.next.len(), 2);
    assert_eq!(
        page.next[0].url,
        "https://al.milesplit.com/meets/754110/results/1330423/raw"
    );
    assert_eq!(
        page.next[1].url,
        "https://al.milesplit.com/meets/754110/results/1330427/raw"
    );
    Ok(())
}

#[test]
fn given_published_manual_result_table_when_parsed_then_all_results_are_retained() -> Result<()> {
    let mut target = meet_target("771781", Sport::CrossCountry)?;
    target.url =
        "https://al.milesplit.com/meets/771781-mayhaw-music-festival-5k-2026/results".to_owned();
    let body = include_str!("../../../fixtures/milesplit/meet-al-771781-results.html");
    let page = parse(&target, body)?;
    assert_eq!(page.unassigned_results.len(), 73);
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    assert!(page.next.is_empty());
    let first = &page.unassigned_results[0];
    assert_eq!(first.event, "Girls Varsity 5K");
    assert_eq!(first.mark, "20:51.3");
    assert_eq!(first.raw["name"], "Cara Wallace");
    assert_eq!(first.raw["grade"], "11");
    assert_eq!(first.raw["team"], "Wicksburg");
    assert_eq!(page.unassigned_results[24].mark, "DNF");
    assert_eq!(page.unassigned_results[57].raw["place"], "33n");
    assert_eq!(page.unassigned_results[57].raw["name"], "Grymm McKinight");
    assert_eq!(page.unassigned_results[72].raw["name"], "Jackson Smith");
    assert!(parse(&target, &body.replacen("<td>time\n", "<td>unknown\n", 1)).is_err());
    assert!(
        parse(
            &target,
            "<div id=\"meetResultsBody\"><table><tr><td>Unknown</td></tr></table></div>"
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn given_spreadsheet_results_when_parsed_then_rows_and_inventory_are_preserved() -> Result<()> {
    let mut target = meet_target("776188", Sport::CrossCountry)?;
    target.url =
        "https://al.milesplit.com/meets/776188-center-point-high-school-invite-2026/results"
            .to_owned();
    let body = include_str!("../../../fixtures/milesplit/meet-al-776188-results.html");
    let page = parse(&target, body)?;
    assert_eq!(page.unassigned_results.len(), 37);
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    let first = &page.unassigned_results[0];
    assert_eq!(first.meet_id.as_deref(), Some("776188"));
    assert_eq!(first.event, "Boys 5000 Meter Run");
    assert_eq!(first.mark, "19:13.75");
    assert_eq!(first.raw["place"], "1");
    assert_eq!(first.raw["name"], "Muguia, Kevin");
    assert_eq!(first.raw["team"], "Fultondale");
    assert_eq!(first.raw["points"], "1");
    assert_eq!(page.unassigned_results[5].mark, "21:29:53");
    assert_eq!(page.unassigned_results[18].mark, "25:55:17");
    assert_eq!(page.unassigned_results[36].raw["name"], "James, Carter");
    assert_eq!(page.next.len(), 3);
    assert!(page.next.iter().all(|next| !next.url.contains("1325876")));
    assert!(page.next.iter().any(|next| next.url.contains("1325874")));
    assert!(page.next.iter().any(|next| next.url.contains("1325875")));
    assert!(page.next.iter().any(|next| next.url.contains("1328237")));
    Ok(())
}

#[test]
fn given_spreadsheet_file_aliases_when_parsed_then_published_row_ids_match() -> Result<()> {
    let body = include_str!("../../../fixtures/milesplit/meet-al-776188-results.html");
    let mut root = meet_target("776188", Sport::CrossCountry)?;
    root.url = "https://al.milesplit.com/meets/776188-center-point-high-school-invite-2026/results"
        .to_owned();
    let root_page = parse(&root, body)?;
    let mut file = root.clone();
    file.kind = PageKind::Results;
    file.url = "https://www.milesplit.com/meets/776188/results/1325876/raw".to_owned();
    let file_page = parse(&file, body)?;
    assert_eq!(file_page.unassigned_results.len(), 37);
    assert!(file_page.next.is_empty());
    for (left, right) in root_page
        .unassigned_results
        .iter()
        .zip(&file_page.unassigned_results)
    {
        assert_eq!(left.result_id, right.result_id);
        assert_eq!(left.mark, right.mark);
    }
    file.url = "https://www.milesplit.com/meets/776188/results/1325875/raw".to_owned();
    let other_page = parse(&file, body)?;
    assert_ne!(
        root_page.unassigned_results[0].result_id,
        other_page.unassigned_results[0].result_id
    );
    Ok(())
}

#[test]
fn given_malformed_spreadsheet_when_parsed_then_it_is_not_accepted_as_empty() -> Result<()> {
    let mut target = meet_target("776188", Sport::CrossCountry)?;
    target.url =
        "https://al.milesplit.com/meets/776188-center-point-high-school-invite-2026/results"
            .to_owned();
    let body = include_str!("../../../fixtures/milesplit/meet-al-776188-results.html");
    let bad_header = body.replacen("<td>Time</td>", "<td>Unknown</td>", 1);
    assert_ne!(body, bad_header);
    assert!(parse(&target, &bad_header).is_err());
    let bad_row = body.replacen("<td>19:13.75</td>", "<td></td>", 1);
    assert_ne!(body, bad_row);
    assert!(parse(&target, &bad_row).is_err());
    let bad_scores = body.replacen("<td>(Win / Loss) Record</td>", "<td>Unknown</td>", 1);
    assert_ne!(body, bad_scores);
    assert!(parse(&target, &bad_scores).is_err());
    target.kind = PageKind::Results;
    target.url = "https://al.milesplit.com/meets/776188/results/1325876/raw".to_owned();
    assert!(parse(&target, &bad_header).is_err());
    Ok(())
}

#[test]
fn given_unrecognized_inline_result_when_parsed_then_it_is_not_silently_accepted() -> Result<()> {
    let mut target = meet_target("754110", Sport::CrossCountry)?;
    target.url =
        "https://al.milesplit.com/meets/754110-northeast-alabama-championship-2026/results"
            .to_owned();
    let malformed = AL_MEET.replacen(
        "Name                    Year School                  Finals",
        "Name                    Year School                  Unknown",
        1,
    );
    assert!(parse(&target, &malformed).is_err());
    Ok(())
}

fn raw_target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Results,
        "https://oh.milesplit.com/meets/770621/results/1321880/raw".to_owned(),
        PageContext {
            state: Some("OH".to_owned()),
            sport: Some(Sport::CrossCountry),
            ..PageContext::default()
        },
    )
}

#[test]
fn given_unidentified_raw_rows_when_parsed_then_names_never_become_fake_athletes() -> Result<()> {
    let page = parse(&raw_target(), RAW)?;
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    assert!(page.next.is_empty());
    assert_eq!(page.unassigned_results.len(), 80);
    let first = page
        .unassigned_results
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing unassigned raw row"))?;
    assert_eq!(first.mark, "12:40.6");
    assert_eq!(first.event, "Boys Middle School 3000 Meter");
    assert_eq!(
        first.raw.get("name").and_then(serde_json::Value::as_str),
        Some("Jeydyn Fields")
    );
    assert_eq!(
        first.raw.get("grade").and_then(serde_json::Value::as_str),
        Some("8")
    );
    assert!(page.notes.iter().any(|note| {
        note.starts_with("80 published result rows have no actual athlete profile ID")
    }));
    Ok(())
}

#[test]
fn given_linked_raw_name_when_parsed_then_only_exact_linked_row_is_identified() -> Result<()> {
    let body = RAW.replacen(
        "Jeydyn Fields",
        "<a href='/athletes/123456'>Jeydyn Fields</a>",
        1,
    );
    let page = parse(&raw_target(), &body)?;
    let athlete = page
        .athletes
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing linked athlete"))?;
    assert_eq!(athlete.id, "123456");
    assert_eq!(athlete.name, "Jeydyn Fields");
    assert_eq!(athlete.grade.as_deref(), Some("8"));
    assert_eq!(athlete.state, None);
    assert_eq!(page.performances.len(), 1);
    assert_eq!(page.unassigned_results.len(), 79);
    assert_eq!(
        page.performances.first().map(|pr| pr.mark.as_str()),
        Some("12:40.6")
    );
    assert!(
        page.next
            .iter()
            .all(|target| target.context.state.is_none())
    );
    Ok(())
}

#[test]
fn given_denied_or_unknown_pages_when_parsed_then_no_empty_success_or_auth_fallback() -> Result<()>
{
    let target = athlete_target("15005189", Some("OH"), Sport::Outdoor)?;
    assert!(parse(&target, "<title>Log In - MileSplit</title>").is_err());
    assert!(parse(&target, "<title>Just a moment...</title>").is_err());
    assert!(parse(&target, "<html><title>MileSplit</title></html>").is_err());
    assert!(
        parse(
            &athlete_target("999999", Some("OH"), Sport::Outdoor)?,
            PROFILE
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn given_pro_result_inventory_when_parsed_then_denied_files_are_not_requested() -> Result<()> {
    let target = meet_target("770621", Sport::CrossCountry)?;
    let body = r#"<script>let meetResultFiles = [{"id":1321880,"name":"Results","isMeetPro":1}];</script>"#;
    let page = parse(&target, body)?;
    assert!(page.next.is_empty());
    assert!(page.performances.is_empty());
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("subscription result file 1321880"))
    );
    Ok(())
}

#[test]
fn given_legacy_result_options_when_parsed_then_published_ids_drive_raw_links() -> Result<()> {
    let target = meet_target("770621", Sport::CrossCountry)?;
    let body = r#"<select id="ddResultsPage"><option value="https://oh.milesplit.com/meets/770621-beaver-2026/results/1321880">Boys</option><option value="https://oh.milesplit.com/meets/770621-beaver-2026/results/1321881?x=1">Girls</option></select>"#;
    let page = parse(&target, body)?;
    assert_eq!(page.next.len(), 2);
    assert_eq!(
        page.next.first().map(|target| target.url.as_str()),
        Some("https://oh.milesplit.com/meets/770621/results/1321880/raw")
    );
    assert_eq!(
        page.next.get(1).map(|target| target.url.as_str()),
        Some("https://oh.milesplit.com/meets/770621/results/1321881/raw")
    );
    Ok(())
}

#[test]
fn given_inline_raw_meet_when_parsed_then_result_evidence_is_not_lost() -> Result<()> {
    let mut target = raw_target();
    target.kind = PageKind::Meet;
    target.url = "https://oh.milesplit.com/meets/770621/results".to_owned();
    let page = parse(&target, RAW)?;
    assert!(
        page.notes
            .iter()
            .any(|note| note.starts_with("Read 80 source-published"))
    );
    assert!(page.athletes.is_empty());
    assert_eq!(page.unassigned_results.len(), 80);
    Ok(())
}

#[test]
fn given_a_masked_pr_mark_when_parsed_then_incompleteness_is_explicit_not_zero() -> Result<()> {
    let body = PROFILE.replace(
        "<td class=\"pr-td-mark\">4:12.69</td>",
        "<td class=\"pr-td-mark\"><span class=\"mask\">*****</span></td>",
    );
    let page = parse(
        &athlete_target("15005189", Some("OH"), Sport::Outdoor)?,
        &body,
    )?;
    assert!(
        !page
            .performances
            .iter()
            .any(|pr| pr.event == "1600m" && pr.mark == "4:12.69")
    );
    assert!(
        page.performances
            .iter()
            .all(|pr| pr.mark != "*****" && pr.mark != "0")
    );
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("masked PR outdoor 1600m"))
    );
    Ok(())
}

#[test]
fn given_explicit_person_identity_link_when_parsed_then_only_owned_same_as_is_emitted() -> Result<()>
{
    let body = r#"<link rel="canonical" href="https://oh.milesplit.com/athletes/123"><script type="application/ld+json">{"@type":"Person","url":"https://oh.milesplit.com/athletes/123","sameAs":["https://www.athletic.net/athlete/456/track-and-field"]}</script><h1 id="athleteName">Source Athlete</h1><a href="https://www.athletic.net/athlete/999/track-and-field">Another Athlete</a><table id="prTable"><tbody></tbody></table>"#;
    let page = parse(&athlete_target("123", None, Sport::Outdoor)?, body)?;
    assert_eq!(
        page.athletes
            .first()
            .map(|athlete| athlete.same_as.as_slice()),
        Some(["https://www.athletic.net/athlete/456/track-and-field".to_owned()].as_slice())
    );
    assert!(
        parse(
            &athlete_target("123", None, Sport::Outdoor)?,
            &body.replace(
                "\"url\":\"https://oh.milesplit.com/athletes/123\"",
                "\"url\":\"https://oh.milesplit.com/athletes/124\""
            )
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn given_nonwinning_status_rows_when_parsed_then_statuses_and_row_evidence_survive() -> Result<()> {
    let original = parse(&raw_target(), RAW)?;
    let original_id = original
        .unassigned_results
        .first()
        .and_then(|row| row.result_id.as_ref())
        .ok_or_else(|| anyhow::anyhow!("missing row ID"))?;
    ["    DNS", "    DNF", "     DQ", "    NaN", "    inf"]
        .iter()
        .try_for_each(|replacement| -> Result<()> {
            let page = parse(&raw_target(), &RAW.replacen("12:40.6", replacement, 1))?;
            let row = page
                .unassigned_results
                .first()
                .ok_or_else(|| anyhow::anyhow!("status row was dropped"))?;
            assert_eq!(page.unassigned_results.len(), 80);
            assert_eq!(row.mark, replacement.trim());
            assert_ne!(row.result_id.as_ref(), Some(original_id));
            assert!(
                row.raw
                    .get("published_text")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|line| line.contains(replacement))
            );
            Ok(())
        })?;
    Ok(())
}

#[test]
fn given_raw_row_replacement_at_same_position_when_parsed_then_identity_does_not_overwrite()
-> Result<()> {
    let original = parse(&raw_target(), RAW)?;
    let changed = parse(
        &raw_target(),
        &RAW.replacen("Jeydyn Fields", "Other Runners", 1),
    )?;
    let first = original
        .unassigned_results
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing row"))?;
    let replacement = changed
        .unassigned_results
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing replacement"))?;
    assert_eq!(first.raw["name"], "Jeydyn Fields");
    assert_eq!(replacement.raw["name"], "Other Runners");
    assert_eq!(first.mark, replacement.mark);
    assert_ne!(first.result_id, replacement.result_id);
    Ok(())
}
