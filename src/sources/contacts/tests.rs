use anyhow::{Context, Result};

use crate::model::{PageContext, PageKind, School, Source, Sport, Target};

use super::{parse, targets};

const CAPTURE: &str = include_str!("../../../fixtures/contacts/miramonte-staff.html");
const SCOPED: &str = include_str!("../../../fixtures/contacts/scoped-school.html");

fn target(name: &str, url: &str) -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        url.to_owned(),
        PageContext {
            school_id: Some("school-123".to_owned()),
            school_name: Some(name.to_owned()),
            state: Some("CA".to_owned()),
            season: Some(2026),
            subject_id: Some("contacts-depth:0".to_owned()),
            ..PageContext::default()
        },
    )
}

#[test]
fn given_public_sidearm_capture_when_parsed_then_row_local_coaching_mailboxes_survive() -> Result<()>
{
    let target = target(
        "Miramonte High School",
        "https://gomats.org/staff-directory",
    );
    let page = parse(&target, CAPTURE)?;
    assert_eq!(page.coaches.len(), 3);
    let brian = page
        .coaches
        .iter()
        .find(|coach| coach.email == "bhenderson@auhsdschools.org")
        .context("missing Brian")?;
    assert_eq!(brian.name.as_deref(), Some("Brian Henderson"));
    assert_eq!(brian.sport, Some(Sport::CrossCountry));
    assert_eq!(brian.role, "Head Coach — Cross Country");
    let robert = page
        .coaches
        .iter()
        .find(|coach| coach.email == "caljumper259@gmail.com")
        .context("missing Robert")?;
    assert_eq!(robert.name.as_deref(), Some("Robert Kennedy"));
    assert_eq!(robert.sport, None);
    assert_eq!(robert.role, "Head Coach — Track & Field");
    assert!(
        page.coaches
            .iter()
            .any(|coach| coach.email == "dharris@auhsdschools.org"
                && coach.role == "Head Coach — Unified Sports - Track")
    );
    assert!(
        page.coaches
            .iter()
            .all(|coach| coach.school_id == "school-123")
    );
    assert!(page.schools.is_empty());
    Ok(())
}

#[test]
fn given_staff_cards_when_parsed_then_only_published_track_and_xc_coaches_are_retained()
-> Result<()> {
    let target = target(
        "Example High School",
        "https://school.example.edu/athletics/staff",
    );
    let page = parse(&target, SCOPED)?;
    let actual = page
        .coaches
        .iter()
        .map(|coach| (coach.name.as_deref(), coach.email.as_str(), coach.sport))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                Some("Ada Runner"),
                "ada.runner@school.example.edu",
                Some(Sport::Indoor)
            ),
            (
                Some("Ben Walker"),
                "ben.walker@school.example.edu",
                Some(Sport::CrossCountry)
            ),
            (
                Some("Chris Thrower"),
                "chris.thrower@school.example.edu",
                Some(Sport::Outdoor)
            ),
        ]
    );
    Ok(())
}

#[test]
fn given_school_and_district_jsonld_when_parsed_then_only_school_postal_address_is_associated()
-> Result<()> {
    let target = target(
        "Example High School",
        "https://school.example.edu/athletics/staff",
    );
    let page = parse(&target, SCOPED)?;
    let [school] = page.schools.as_slice() else {
        anyhow::bail!("expected one school address");
    };
    assert_eq!(school.id, "school-123");
    assert_eq!(school.name, "Example High School");
    assert_eq!(school.address.as_deref(), Some("123 School Road"));
    assert_eq!(school.city.as_deref(), Some("Example City"));
    assert_eq!(school.state.as_deref(), Some("CA"));
    assert_eq!(school.postal_code.as_deref(), Some("90210"));
    assert_eq!(
        school.website.as_deref(),
        Some("https://school.example.edu/")
    );
    assert_eq!(
        school.public_email.as_deref(),
        Some("office@school.example.edu")
    );
    Ok(())
}

#[test]
fn given_different_school_or_state_when_parsed_then_address_is_not_misattributed() -> Result<()> {
    let other_school = target("Different High School", "https://school.example.edu/");
    let mut other_state = target("Example High School", "https://school.example.edu/");
    other_state.context.state = Some("NV".to_owned());
    let school_page = parse(&other_school, SCOPED)?;
    let state_page = parse(&other_state, SCOPED)?;
    assert!(school_page.schools.is_empty());
    assert!(
        state_page
            .schools
            .iter()
            .all(|school| school.address.is_none())
    );
    assert!(
        school_page
            .notes
            .iter()
            .any(|note| note.contains("postal address"))
    );
    Ok(())
}

#[test]
fn given_published_contact_links_when_parsed_then_origin_platform_and_link_budgets_apply()
-> Result<()> {
    let target = target(
        "Example High School",
        "https://school.example.edu/athletics/staff",
    );
    let page = parse(&target, SCOPED)?;
    assert_eq!(
        page.next
            .iter()
            .map(|next| next.url.as_str())
            .collect::<Vec<_>>(),
        vec![
            "https://school.example.edu/staff",
            "https://school.example.edu/contact",
            "https://school.bigteams.com/staff",
        ]
    );
    assert!(
        page.next
            .iter()
            .all(|next| next.source == Source::AthleticNet
                && next.kind == PageKind::Contacts
                && next.context.subject_id.as_deref() == Some("contacts-depth:1")
                && next.context.school_id == target.context.school_id)
    );
    Ok(())
}

#[test]
fn given_many_staff_links_when_crawled_then_twenty_links_and_depth_two_are_hard_limits()
-> Result<()> {
    let body = format!(
        "<main><h1>School</h1>{}</main>",
        (0..30)
            .map(|number| format!("<a href='/staff/{number}'>Staff</a>"))
            .collect::<String>()
    );
    let mut target = target("School", "https://school.example.edu/");
    target.context.subject_id = Some("contacts-depth:1".to_owned());
    let page = parse(&target, &body)?;
    let next = page.next.first().context("missing first staff page")?;
    let end = parse(next, &body)?;
    assert_eq!(page.next.len(), 20);
    assert_eq!(next.context.subject_id.as_deref(), Some("contacts-depth:2"));
    assert!(end.next.is_empty());
    Ok(())
}

#[test]
fn given_unclear_staff_association_when_parsed_then_names_and_mailboxes_are_not_guessed()
-> Result<()> {
    let body = "<main><h1>School</h1><div class='staff-card'>Track Coach <a href='mailto:coach@school.edu'>Coach</a><a href='mailto:teacher@school.edu'>Teacher</a></div><p>Track Registrar registrar@school.edu</p><p>Cross Country Coach <a href='mailto:xc@school.edu'>Contact</a></p><footer><p>Track Coach footer@school.edu</p></footer></main>";
    let page = parse(&target("School", "https://school.edu/"), body)?;
    assert_eq!(page.coaches.len(), 1);
    let coach = page.coaches.first().context("missing coach")?;
    assert_eq!(coach.email, "xc@school.edu");
    assert_eq!(coach.name, None);
    assert_eq!(coach.sport, Some(Sport::CrossCountry));
    Ok(())
}

#[test]
fn given_local_sport_headings_when_parsed_then_section_context_never_becomes_global() -> Result<()>
{
    let body = "<main><h1>School</h1><h2>Cross Country</h2><p>Head Coach xc@school.edu</p><h2>Basketball</h2><p>Head Coach hoops@school.edu</p></main>";
    let page = parse(&target("School", "https://school.edu/"), body)?;
    assert_eq!(page.coaches.len(), 1);
    assert_eq!(
        page.coaches.first().map(|coach| coach.email.as_str()),
        Some("xc@school.edu")
    );
    Ok(())
}

#[test]
fn given_explicit_school_address_when_parsed_then_district_footer_is_ignored() -> Result<()> {
    let body = "<main><h1>School</h1><address>School: 123 Main Street, Example City, CA 90210</address></main><footer><address>999 District Road, Other City, CA 90001</address></footer>";
    let page = parse(&target("School", "https://school.edu/"), body)?;
    let school = page.schools.first().context("missing school address")?;
    assert_eq!(page.schools.len(), 1);
    assert_eq!(school.address.as_deref(), Some("123 Main Street"));
    assert_eq!(school.city.as_deref(), Some("Example City"));
    assert_eq!(school.postal_code.as_deref(), Some("90210"));
    Ok(())
}

#[test]
fn given_missing_or_blocked_content_when_parsed_then_unknown_is_not_reported_as_absence()
-> Result<()> {
    let target = target("School", "https://school.edu/");
    let page = parse(
        &target,
        "<title>Access Denied</title><h1>Please sign in</h1>",
    )?;
    assert!(page.coaches.is_empty() && page.schools.is_empty());
    assert_eq!(page.notes.len(), 3);
    assert!(
        page.notes
            .iter()
            .all(|note| note.contains("unknown or incomplete"))
    );
    assert!(parse(&target, "not html").is_err());
    assert!(
        parse(
            &target,
            "<script type='application/ld+json'>{broken}</script>"
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn given_provider_website_when_scheduling_then_no_paths_or_identity_are_invented() -> Result<()> {
    let mut school = School {
        id: "school-123".to_owned(),
        name: "School".to_owned(),
        ..School::default()
    };
    assert!(targets(Source::MileSplit, &school, 2026)?.is_empty());
    school.website = Some("https://school.edu/actual/home".to_owned());
    let jobs = targets(Source::MileSplit, &school, 2026)?;
    assert_eq!(jobs.len(), 1);
    let job = jobs.first().context("missing official-page job")?;
    assert_eq!(job.url, "https://school.edu/actual/home");
    assert_eq!(job.source, Source::MileSplit);
    assert_eq!(job.context.season, Some(2026));
    school.website = Some("mailto:office@school.edu".to_owned());
    assert!(targets(Source::MileSplit, &school, 2026).is_err());
    Ok(())
}

#[test]
fn given_incomplete_identity_when_parsed_then_school_association_is_rejected() {
    let mut target = target("School", "https://school.edu/");
    target.context.school_id = None;
    assert!(parse(&target, SCOPED).is_err());
    target.context.school_id = Some("school-123".to_owned());
    target.context.school_name = None;
    assert!(parse(&target, SCOPED).is_err());
    target.context.school_name = Some("School".to_owned());
    target.context.subject_id = Some("contacts-depth:invalid".to_owned());
    assert!(parse(&target, SCOPED).is_err());
}

#[test]
fn given_school_identified_microdata_when_parsed_then_only_published_components_survive()
-> Result<()> {
    let body = "<main><h1>School</h1><address>School <span itemprop='streetAddress'>12 School Lane</span><span itemprop='addressLocality'>Example City</span><span itemprop='addressRegion'>CA</span><span itemprop='postalCode'>90210-1234</span></address></main>";
    let page = parse(&target("School", "https://school.edu/"), body)?;
    let school = page.schools.first().context("missing structured address")?;
    assert_eq!(school.address.as_deref(), Some("12 School Lane"));
    assert_eq!(school.city.as_deref(), Some("Example City"));
    assert_eq!(school.postal_code.as_deref(), Some("90210-1234"));
    assert_eq!(school.website, None);
    Ok(())
}

#[test]
fn given_combined_published_appointment_when_parsed_then_all_explicit_sports_are_preserved()
-> Result<()> {
    let body = "<main><h1>School</h1><p>Teacher and Indoor / Outdoor Track and Cross Country Coach coach@school.edu</p></main>";
    let page = parse(&target("School", "https://school.edu/"), body)?;
    assert_eq!(
        page.coaches
            .iter()
            .map(|coach| coach.sport)
            .collect::<Vec<_>>(),
        vec![
            Some(Sport::CrossCountry),
            Some(Sport::Indoor),
            Some(Sport::Outdoor)
        ]
    );
    assert!(
        page.coaches
            .iter()
            .all(|coach| coach.email == "coach@school.edu" && coach.name.is_none())
    );
    Ok(())
}

#[test]
fn given_office_only_capture_when_parsed_then_school_mailbox_is_not_a_coach() -> Result<()> {
    let body = "<main><h1>Example High School</h1><p>Main Office: <a href='mailto:office@school.edu'>Email us</a></p><p>Math teacher teacher@school.edu</p></main><footer><p>District school office: district@school.edu</p></footer>";
    let page = parse(&target("Example High School", "https://school.edu/"), body)?;
    let school = page
        .schools
        .first()
        .context("missing school office update")?;
    assert_eq!(page.schools.len(), 1);
    assert_eq!(school.id, "school-123");
    assert_eq!(school.name, "Example High School");
    assert_eq!(school.public_email.as_deref(), Some("office@school.edu"));
    assert_eq!(school.address, None);
    assert_eq!(school.website, None);
    assert!(page.coaches.is_empty());
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("postal address"))
    );
    Ok(())
}

#[test]
fn given_ambiguous_or_unassociated_offices_when_parsed_then_mailboxes_remain_unknown() -> Result<()>
{
    let ambiguous = "<main><h1>Example High School</h1><p>Main Office: first@school.edu</p><p>Athletics Office: second@school.edu</p></main>";
    let unrelated =
        "<main><h1>District Administration</h1><p>Main Office: district@school.edu</p></main>";
    let target = target("Example High School", "https://school.edu/");
    let ambiguous_page = parse(&target, ambiguous)?;
    let unrelated_page = parse(&target, unrelated)?;
    assert!(ambiguous_page.schools.is_empty());
    assert!(unrelated_page.schools.is_empty());
    assert!(
        ambiguous_page
            .notes
            .iter()
            .any(|note| note.contains("office mailbox"))
    );
    Ok(())
}
