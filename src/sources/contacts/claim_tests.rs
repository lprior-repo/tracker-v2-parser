use anyhow::{Context, Result};
use regex::Regex;
use scraper::Html;

use crate::model::{Coach, PageContext, PageKind, Source, Sport, Target};

use super::{email, parse, staff};

const WIAA: &str = include_str!("../../../fixtures/contacts/wiaa-abbotsford-coaches.html");
const FINALSITE: &str = include_str!("../../../fixtures/contacts/finalsite-community-coaches.html");
const NULLABLE: &str = include_str!("../../../fixtures/contacts/nullable-coaches.html");
const NAMED: &str = include_str!("../../../fixtures/contacts/named-no-email.html");
const WAYZATA: &str = include_str!("../../../fixtures/contacts/wayzata-administration.html");

const REVIEW: &str = include_str!("../../../fixtures/contacts/staff-review.html");
const REGRESSIONS: &str = include_str!("../../../fixtures/contacts/staff-review-regressions.html");

#[test]
fn given_duplicate_claims_with_conflicting_affiliation_when_reordered_then_review_notes_are_unioned()
-> Result<()> {
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        REGRESSIONS,
    )?;
    let document = Html::parse_document(REGRESSIONS);
    let cards = super::selector(".staff-card")?;
    let reversed = document
        .select(&cards)
        .map(|card| card.html())
        .collect::<Vec<_>>();
    let body = format!(
        "<title>Example High School</title><main><h1>Staff Directory</h1>{}</main>",
        reversed.into_iter().rev().collect::<String>()
    );
    let replay = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        &body,
    )?;
    let duplicate = page
        .coaches
        .iter()
        .filter(|coach| coach.name.as_deref() == Some("Sam Duplicate"))
        .collect::<Vec<_>>();
    assert_eq!(duplicate.len(), 1);
    let coach = duplicate[0];
    assert_eq!(coach.email.as_deref(), Some("sam@school.example.edu"));
    assert_eq!(coach.review_notes.len(), 2);
    assert!(
        coach
            .review_notes
            .iter()
            .any(|note| note.contains("School affiliation unconfirmed"))
    );
    let reversed_coach = replay
        .coaches
        .iter()
        .find(|candidate| candidate.name == coach.name && candidate.email == coach.email)
        .context("missing reversed duplicate")?;
    assert_eq!(
        replay
            .coaches
            .iter()
            .filter(|candidate| candidate.name == coach.name && candidate.email == coach.email)
            .count(),
        1
    );
    assert_eq!(reversed_coach.review_notes, coach.review_notes);
    Ok(())
}

#[test]
fn given_running_programs_when_parsed_then_generic_roles_refine_but_explicit_duties_remain_authoritative()
-> Result<()> {
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        REGRESSIONS,
    )?;
    let actual = page
        .coaches
        .iter()
        .filter(|coach| coach.name.as_deref() != Some("Sam Duplicate"))
        .map(|coach| (coach.name.as_deref(), coach.role.as_str(), coach.sport))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (Some("Inez Indoor"), "Head Track Coach", Some(Sport::Indoor)),
            (
                Some("Owen Outdoor"),
                "Head Track Coach",
                Some(Sport::Outdoor)
            ),
            (
                Some("Xena Country"),
                "Head Coach — Cross Country",
                Some(Sport::CrossCountry)
            ),
            (
                Some("Drew Running"),
                "Head Outdoor Track Coach",
                Some(Sport::Outdoor)
            ),
            (
                Some("Iris Specific"),
                "Indoor Track Coach",
                Some(Sport::Indoor)
            ),
            (
                Some("Dana Disciplines"),
                "Outdoor Track and Cross Country Coach",
                Some(Sport::CrossCountry)
            ),
            (
                Some("Dana Disciplines"),
                "Outdoor Track and Cross Country Coach",
                Some(Sport::Outdoor)
            ),
            (
                Some("Blair Seasons"),
                "Indoor and Outdoor Track Coach",
                Some(Sport::Indoor)
            ),
            (
                Some("Blair Seasons"),
                "Indoor and Outdoor Track Coach",
                Some(Sport::Outdoor)
            ),
            (
                Some("Cora Specific"),
                "Cross Country Coach",
                Some(Sport::CrossCountry)
            ),
            (Some("Gina Generic"), "Track Coach", None),
        ]
    );
    Ok(())
}

#[test]
fn given_contradictory_running_programs_when_parsed_then_unsupported_labels_are_reviewed_not_added()
-> Result<()> {
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        REGRESSIONS,
    )?;
    for (name, label, sport) in [
        ("Drew Running", "Cross Country", Some(Sport::Outdoor)),
        ("Iris Specific", "Outdoor Track", Some(Sport::Indoor)),
        ("Cora Specific", "Indoor Track", Some(Sport::CrossCountry)),
        ("Gina Generic", "Cross Country", None),
    ] {
        let rows = page
            .coaches
            .iter()
            .filter(|coach| coach.name.as_deref() == Some(name))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "{name}");
        assert_eq!(rows[0].sport, sport, "{name}");
        assert!(
            rows[0].review_notes.iter().any(|note| {
                note.contains("Program context requires review")
                    && note.contains(label)
                    && note.contains(rows[0].role.as_str())
            }),
            "{name}"
        );
    }
    assert!(
        page.coaches
            .iter()
            .filter(|coach| matches!(
                coach.name.as_deref(),
                Some(
                    "Inez Indoor"
                        | "Owen Outdoor"
                        | "Xena Country"
                        | "Dana Disciplines"
                        | "Blair Seasons"
                )
            ))
            .all(|coach| !coach
                .review_notes
                .iter()
                .any(|note| note.contains("Program context requires review")))
    );
    Ok(())
}

#[test]
fn given_conflicting_page_headings_when_parsed_then_matching_identity_does_not_hide_ambiguity()
-> Result<()> {
    for body in [
        REVIEW.replace(
            "<h1>Example High School</h1>",
            "<h1>Different High School</h1>",
        ),
        REVIEW.replace(
            "<title>Example High School</title>",
            "<title>District Athletics</title>",
        ),
        REVIEW.replace(
            "<title>Example High School</title>",
            "<title>Unknown Organization</title>",
        ),
    ] {
        let page = parse(
            &target(
                "Example High School",
                "WI",
                "https://school.example.edu/staff",
            ),
            &body,
        )?;
        assert_eq!(page.coaches.len(), 4);
        assert!(page.coaches.iter().all(|coach| {
            coach
                .review_notes
                .iter()
                .any(|note| note.contains("School affiliation unconfirmed"))
        }));
    }
    let neutral = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        REGRESSIONS,
    )?;
    assert!(
        neutral
            .coaches
            .iter()
            .filter(|coach| coach.name.as_deref() != Some("Sam Duplicate"))
            .all(|coach| !coach
                .review_notes
                .iter()
                .any(|note| note.contains("School affiliation unconfirmed")))
    );
    Ok(())
}

#[test]
fn given_district_or_unmatched_school_when_parsed_then_retained_claims_require_affiliation_review()
-> Result<()> {
    for name in ["Arbitrary High School", "Community High School"] {
        let page = parse(
            &target(
                name,
                "WI",
                "https://www.communityisd.org/athletics/coaches-directory",
            ),
            FINALSITE,
        )?;
        assert_eq!(page.coaches.len(), 1);
        let coach = &page.coaches[0];
        assert_eq!(coach.name.as_deref(), Some("Daniel Childress"));
        assert_eq!(coach.school_id, "published-source-context");
        assert!(
            coach
                .review_notes
                .iter()
                .any(|note| note.contains("School affiliation unconfirmed"))
        );
        assert!(
            coach
                .review_notes
                .iter()
                .any(|note| note.contains("Current tenure unconfirmed"))
        );
    }
    Ok(())
}

#[test]
fn given_school_local_page_when_parsed_then_affiliation_is_not_assigned_to_another_school()
-> Result<()> {
    let local = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        REVIEW,
    )?;
    assert_eq!(local.coaches.len(), 4);
    assert!(
        local
            .coaches
            .iter()
            .filter(|coach| coach.name.as_deref() != Some("Olivia Other"))
            .all(|coach| !coach
                .review_notes
                .iter()
                .any(|note| note.contains("School affiliation unconfirmed")))
    );
    let other_card = local
        .coaches
        .iter()
        .find(|coach| coach.name.as_deref() == Some("Olivia Other"))
        .context("missing other school claim")?;
    assert!(
        other_card
            .review_notes
            .iter()
            .any(|note| note.contains("School affiliation unconfirmed"))
    );
    let unmatched = parse(
        &target(
            "Unmatched High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        REVIEW,
    )?;
    assert_eq!(claims(&unmatched.coaches), claims(&local.coaches));
    assert!(unmatched.coaches.iter().all(|coach| {
        coach
            .review_notes
            .iter()
            .any(|note| note.contains("School affiliation unconfirmed"))
    }));
    Ok(())
}

#[test]
fn given_conflicting_program_when_parsed_then_only_explicit_multisport_track_role_survives()
-> Result<()> {
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        REVIEW,
    )?;
    assert!(
        !page
            .coaches
            .iter()
            .any(|coach| coach.name.as_deref() == Some("Pat Conflict"))
    );
    let dual = page
        .coaches
        .iter()
        .find(|coach| coach.name.as_deref() == Some("Dana Dual"))
        .context("missing dual coach")?;
    assert_eq!(dual.role, "Head Track and Basketball Coach");
    assert_eq!(dual.sport, None);
    assert_eq!(dual.email.as_deref(), Some("dual@school.example.edu"));
    Ok(())
}

#[test]
fn given_plaintext_chrome_inside_staff_card_when_parsed_then_mailbox_and_fallback_role_ignore_it()
-> Result<()> {
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        REVIEW,
    )?;
    let unknown = page
        .coaches
        .iter()
        .find(|coach| coach.name.as_deref() == Some("No Mailbox"))
        .context("missing unknown mailbox claim")?;
    assert_eq!(unknown.email, None);
    assert_eq!(unknown.sport, Some(Sport::CrossCountry));
    let real = page
        .coaches
        .iter()
        .find(|coach| coach.name.as_deref() == Some("Real Mailbox"))
        .context("missing real mailbox claim")?;
    assert_eq!(real.email.as_deref(), Some("real@school.example.edu"));
    assert_eq!(real.sport, Some(Sport::Outdoor));
    assert!(
        !page
            .coaches
            .iter()
            .any(|coach| coach.name.as_deref() == Some("Nina Unrelated"))
    );
    Ok(())
}

#[test]
fn given_school_identified_card_on_district_page_when_parsed_then_only_that_card_is_affiliated()
-> Result<()> {
    let body = REVIEW
        .replace(
            "<title>Example High School</title>",
            "<title>District Athletics</title>",
        )
        .replace(
            "<h1>Example High School</h1>",
            "<h1>District Athletics</h1>",
        )
        .replace("Different High School", "Example High School");
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        &body,
    )?;
    assert_eq!(page.coaches.len(), 4);
    let local = page
        .coaches
        .iter()
        .find(|coach| coach.name.as_deref() == Some("Olivia Other"))
        .context("missing school-identified claim")?;
    assert!(
        !local
            .review_notes
            .iter()
            .any(|note| note.contains("School affiliation unconfirmed"))
    );
    assert!(
        page.coaches
            .iter()
            .filter(|coach| coach.name != local.name)
            .all(|coach| coach
                .review_notes
                .iter()
                .any(|note| note.contains("School affiliation unconfirmed")))
    );
    Ok(())
}
type Claim<'a> = (Option<&'a str>, &'a str, Option<&'a str>, Option<Sport>);

fn claims(coaches: &[Coach]) -> Vec<Claim<'_>> {
    coaches
        .iter()
        .map(|coach| {
            (
                coach.name.as_deref(),
                coach.role.as_str(),
                coach.email.as_deref(),
                coach.sport,
            )
        })
        .collect()
}

fn target(name: &str, state: &str, url: &str) -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        url.to_owned(),
        PageContext {
            school_id: Some("published-source-context".to_owned()),
            school_name: Some(name.to_owned()),
            state: Some(state.to_owned()),
            subject_id: Some("contacts-depth:0".to_owned()),
            ..PageContext::default()
        },
    )
}

fn wiaa_claims() -> Vec<Claim<'static>> {
    vec![
        (
            Some("JACOB KNAPMILLER"),
            "Head Coach — Boys Track and Field",
            Some("jknapmiller@abbotsford.k12.wi.us"),
            None,
        ),
        (
            Some("Dillon Novak"),
            "Head Coach — Girls Cross Country",
            Some("dnovak@abbotsford.k12.wi.us"),
            Some(Sport::CrossCountry),
        ),
    ]
}

#[test]
fn given_retained_wisconsin_cloudflare_claims_when_parsed_then_exact_published_rows_survive()
-> Result<()> {
    let target = target(
        "Abbotsford",
        "WI",
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=1",
    );
    let page = parse(&target, WIAA)?;
    assert_eq!(claims(&page.coaches), wiaa_claims());
    assert!(page.schools.is_empty());
    Ok(())
}

#[test]
fn given_either_retained_cloudflare_attribute_when_parsed_then_each_decodes_independently()
-> Result<()> {
    let attributes = [
        r#"data-cfemail="[^"]*""#,
        r#"href="/cdn-cgi/l/email-protection#[^"]*""#,
    ];
    for attribute in attributes {
        let body = Regex::new(attribute)?.replace_all(WIAA, "");
        let rows = staff::parse(
            &Html::parse_document(&body),
            "published-source-context",
            "Abbotsford",
        )?;
        assert_eq!(claims(&rows), wiaa_claims());
    }
    Ok(())
}

#[test]
fn given_real_finalsite_cards_when_parsed_then_visible_title_and_name_survive_without_js_execution()
-> Result<()> {
    let rows = staff::parse(
        &Html::parse_document(FINALSITE),
        "published-source-context",
        "Unconfirmed High School",
    )?;
    assert_eq!(
        claims(&rows),
        vec![(
            Some("Daniel Childress"),
            "Head Coach - Track & Field",
            None,
            None
        )]
    );
    Ok(())
}

#[test]
fn given_nullable_shared_and_ambiguous_claims_when_parsed_then_names_and_sports_stay_separate()
-> Result<()> {
    let target = target(
        "Example High School",
        "WI",
        "https://school.example.edu/staff",
    );
    let page = parse(&target, NULLABLE)?;
    assert_eq!(
        claims(&page.coaches),
        vec![
            (
                Some("Ada Unpublished"),
                "Head Outdoor Track Coach",
                None,
                Some(Sport::Outdoor)
            ),
            (
                Some("Ben Unpublished"),
                "Head Outdoor Track Coach",
                None,
                Some(Sport::Outdoor)
            ),
            (
                Some("Cara Shared"),
                "Cross Country Coach",
                Some("xc@school.example.edu"),
                Some(Sport::CrossCountry)
            ),
            (
                Some("Dara Shared"),
                "Cross Country Coach",
                Some("xc@school.example.edu"),
                Some(Sport::CrossCountry)
            ),
            (
                Some("Evan Ambiguous"),
                "Indoor Track Coach",
                None,
                Some(Sport::Indoor)
            ),
            (
                Some("Fiona Current"),
                "Head Cross Country Coach",
                Some("fiona@school.example.edu"),
                Some(Sport::CrossCountry)
            ),
        ]
    );
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("without an unambiguous public mailbox"))
    );
    let replay = staff::parse(
        &Html::parse_document(&format!("{NULLABLE}{NULLABLE}")),
        "published-source-context",
        "Example High School",
    )?;
    assert_eq!(claims(&replay), claims(&page.coaches));
    Ok(())
}

#[test]
fn given_a_named_coach_without_email_when_parsed_then_missing_email_is_not_absence() -> Result<()> {
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        NAMED,
    )?;
    assert_eq!(
        claims(&page.coaches),
        vec![(
            Some("Ada Unpublished"),
            "Head Outdoor Track Coach",
            None,
            Some(Sport::Outdoor)
        )]
    );
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("without an unambiguous public mailbox"))
    );
    Ok(())
}

#[test]
fn given_malformed_or_oversize_cloudflare_tokens_when_parsed_then_no_mailbox_is_invented()
-> Result<()> {
    let oversized = "00".repeat(256);
    for token in [
        "",
        "0",
        "zz",
        "00ff",
        "006e6f742d612d6d61696c626f78",
        oversized.as_str(),
    ] {
        let body = NAMED.replace(
            "</article>",
            &format!("<a href='/cdn-cgi/l/email-protection#{token}'><span data-cfemail='{token}'>Email</span></a></article>"),
        );
        let page = parse(
            &target(
                "Example High School",
                "WI",
                "https://school.example.edu/staff",
            ),
            &body,
        )?;
        assert_eq!(
            claims(&page.coaches),
            vec![(
                Some("Ada Unpublished"),
                "Head Outdoor Track Coach",
                None,
                Some(Sport::Outdoor)
            )]
        );
    }
    Ok(())
}

#[test]
fn given_hidden_obfuscated_email_when_parsed_then_visible_named_claim_remains_email_unknown()
-> Result<()> {
    let token = "660c0d0807160b0f0a0a03142607040409121500091402480d575448110f481315";
    let body = NAMED.replace("</article>", &format!("<span hidden data-cfemail='{token}'></span><a aria-hidden='true' href='/cdn-cgi/l/email-protection#{token}'>Email</a></article>"));
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        &body,
    )?;
    let coach = page.coaches.first().context("missing visible name")?;
    assert_eq!(page.coaches.len(), 1);
    assert_eq!(coach.name.as_deref(), Some("Ada Unpublished"));
    assert_eq!(coach.email, None);
    Ok(())
}

#[test]
fn given_retained_noncoach_obfuscation_when_decoded_then_mailbox_never_implies_coaching_role()
-> Result<()> {
    let pattern = email::pattern()?;
    assert_eq!(
        email::decode_cfemail(
            "254840424d444b0b554a515140576552445c5f44514456464d4a4a49560b4a5742",
            &pattern
        )
        .as_deref(),
        Some("meghan.potter@wayzataschools.org")
    );
    let page = parse(
        &target(
            "Wayzata High School",
            "MN",
            "https://www.mshsl.org/schools/wayzata-high-school",
        ),
        WAYZATA,
    )?;
    assert!(page.coaches.is_empty());
    assert!(page.schools.is_empty());
    Ok(())
}

#[test]
fn given_conflicting_published_obfuscations_when_parsed_then_a_named_claim_keeps_no_mailbox()
-> Result<()> {
    let href = "375d5c5956475a5e5b5b52457756555558434451584553195c060519405e194244";
    let attribute = "60040e0f16010b200102020f1413060f12044e0b51524e17094e1513";
    let body = NAMED.replace("</article>", &format!("<a href='/cdn-cgi/l/email-protection#{href}'><span data-cfemail='{attribute}'>Email</span></a></article>"));
    let page = parse(
        &target(
            "Example High School",
            "WI",
            "https://school.example.edu/staff",
        ),
        &body,
    )?;
    assert_eq!(
        claims(&page.coaches),
        vec![(
            Some("Ada Unpublished"),
            "Head Outdoor Track Coach",
            None,
            Some(Sport::Outdoor)
        )]
    );
    Ok(())
}

#[test]
fn given_a_maximum_length_encoded_mailbox_when_decoded_then_the_existing_normalizer_sets_the_limit()
-> Result<()> {
    let address = format!("{}@example.edu", "a".repeat(242));
    let encoded = format!(
        "00{}",
        address
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let pattern = email::pattern()?;
    assert_eq!(address.len(), 254);
    assert_eq!(encoded.len(), 510);
    assert_eq!(
        email::decode_cfemail(&encoded, &pattern).as_deref(),
        Some(address.as_str())
    );
    assert_eq!(
        email::decode_cfemail(&format!("{encoded}61"), &pattern),
        None
    );
    Ok(())
}
