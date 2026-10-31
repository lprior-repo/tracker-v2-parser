mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, School, Source, Sport, Target},
    sources,
};

const STMA: &str = include_str!("../fixtures/contacts/minnesota-stma-coach-tables.html");
const WAYZATA: &str = include_str!("../fixtures/contacts/minnesota-wayzata-coach-tables.html");

fn target(name: &str, url: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        url.into(),
        PageContext {
            school_id: Some("school-1".into()),
            school_name: Some(name.into()),
            state: Some("MN".into()),
            season: Some(2026),
            ..PageContext::default()
        },
    )
}

#[test]
fn given_published_activity_coach_contact_headers_when_parsed_then_cells_remain_row_local()
-> Result<()> {
    let page = sources::parse(
        &target(
            "St. Michael-Albertville High School",
            "https://hs.stma.k12.mn.us/athletics-activities/athletics-coaches",
        ),
        STMA,
    )?;
    let claims = page
        .coaches
        .iter()
        .map(|c| (c.name.as_deref(), c.email.as_deref(), c.sport))
        .collect::<Vec<_>>();
    assert_eq!(
        claims,
        vec![
            (
                Some("Chase Cayo"),
                Some("chaseca@mystma.org"),
                Some(Sport::CrossCountry)
            ),
            (Some("Heather Strait"), None, Some(Sport::CrossCountry)),
            (Some("Chase Cayo"), Some("chaseca@mystma.org"), None),
            (Some("Lillian Johnson"), Some("lillianj@mystma.org"), None),
        ]
    );
    assert!(
        page.coaches
            .iter()
            .all(|c| c.school_id == "school-1" && c.role.starts_with("Coach — "))
    );
    Ok(())
}

#[test]
fn given_published_head_coach_accordion_labels_when_parsed_then_only_track_and_xc_rows_survive()
-> Result<()> {
    let page = sources::parse(
        &target(
            "Wayzata High School",
            "https://www.wayzataathletics.com/coaches-directory/",
        ),
        WAYZATA,
    )?;
    let claims = page
        .coaches
        .iter()
        .map(|c| (c.name.as_deref(), c.email.as_deref(), c.sport))
        .collect::<Vec<_>>();
    assert_eq!(
        claims,
        vec![
            (
                Some("Mark Popp"),
                Some("mark.popp@wayzataschools.org"),
                Some(Sport::CrossCountry)
            ),
            (
                Some("Kalsey Beach"),
                Some("kalsey.beach@wayzataschools.org"),
                Some(Sport::CrossCountry)
            ),
            (
                Some("Aaron Berndt"),
                Some("aaron.berndt@wayzataschools.org"),
                None
            ),
            (
                Some("Alicia Pierskalla"),
                Some("alicia.perskalla@wayzataschools.org"),
                None
            ),
        ]
    );
    assert!(
        page.coaches
            .iter()
            .all(|c| c.role.starts_with("Head Coach — "))
    );
    Ok(())
}

fn table(headers: &str, rows: &str) -> String {
    format!(
        "<main><h1>Example High School</h1><table><thead><tr>{headers}</tr></thead><tbody>{rows}</tbody></table></main>"
    )
}

#[test]
fn given_published_contact_directory_anchors_when_discovered_then_both_minnesota_url_forms_are_queued()
-> Result<()> {
    let target = target("Example High School", "https://school.edu/");
    let body = "<main><h1>Example High School</h1><a href='/athletics-coaches'>Athletics</a><a href='/coaches-directory/'>Coaches Directory</a></main>";
    let page = sources::parse(&target, body)?;
    assert_eq!(
        page.next
            .iter()
            .map(|target| target.url.as_str())
            .collect::<Vec<_>>(),
        vec![
            "https://school.edu/athletics-coaches",
            "https://school.edu/coaches-directory/"
        ]
    );
    assert!(
        page.next
            .iter()
            .all(|target| target.context.school_id.as_deref() == Some("school-1"))
    );
    Ok(())
}

#[test]
fn given_students_or_ambiguous_headers_when_parsed_then_no_coaching_claim_is_invented() -> Result<()>
{
    for headers in [
        "<th>Sport</th><th>Student</th><th>Contact</th>",
        "<th>Sport</th><th>Coach</th><th>Coach</th>",
        "<th>Coach</th><th>Contact</th>",
        "<th>Sport</th><th>Name</th><th>Email</th>",
    ] {
        let body = table(
            headers,
            "<tr><td>Track</td><td>Ada Student</td><td>ada@school.edu</td></tr>",
        );
        assert!(
            sources::parse(
                &target("Example High School", "https://school.edu/coaches"),
                &body
            )?
            .coaches
            .is_empty()
        );
    }
    Ok(())
}

#[test]
fn given_row_local_sports_hidden_rows_and_malformed_mailboxes_when_parsed_then_no_cross_row_inference_occurs()
-> Result<()> {
    let body = table(
        "<th>Contact</th><th>Sport</th><th>Coach</th>",
        concat!(
            "<tr><td>wrong@school.edu</td><td>Basketball</td><td>Other Sport</td></tr>",
            "<tr><td>not-a-mailbox</td><td>Track and Field</td><td>Ada Runner</td></tr>",
            "<tr hidden><td>hidden@school.edu</td><td>Cross Country</td><td>Hidden Person</td></tr>",
            "<tr><td>one@school.edu two@school.edu</td><td>Cross Country</td><td>Ambiguous Mail</td></tr>",
            "<tr><td>coach@school.edu</td><td>Track</td><td>To be determined</td></tr>",
            "<tr><td colspan='2'>merged@school.edu</td><td>Cross Country</td><td>Bad Layout</td></tr>"
        ),
    );
    let page = sources::parse(
        &target("Example High School", "https://school.edu/coaches"),
        &body,
    )?;
    assert_eq!(page.coaches.len(), 2);
    assert_eq!(page.coaches[0].name.as_deref(), Some("Ada Runner"));
    assert_eq!(page.coaches[1].name.as_deref(), Some("Ambiguous Mail"));
    assert!(page.coaches.iter().all(|c| c.email.is_none()));
    Ok(())
}

#[test]
fn given_unrelated_or_duplicate_accordion_labels_when_parsed_then_no_headerless_coach_claim_is_invented()
-> Result<()> {
    for labels in [
        "<div id='label'>Student contacts</div>",
        "<div id='label'>Head Coaches</div><div id='label'>Head Coaches</div>",
    ] {
        let body = format!(
            "<main><h1>Example High School</h1>{labels}<div role='region' aria-labelledby='label'><table><tr><td>Track</td><td>Ada Runner</td><td>ada@school.edu</td></tr></table></div></main>"
        );
        assert!(
            sources::parse(
                &target("Example High School", "https://school.edu/coaches"),
                &body
            )?
            .coaches
            .is_empty()
        );
    }
    Ok(())
}

#[test]
fn given_skiing_or_former_coach_rows_when_parsed_then_they_are_not_current_running_coaches()
-> Result<()> {
    let body = table(
        "<th>Sport</th><th>Coach</th><th>Email</th>",
        concat!(
            "<tr><td>Cross Country Skiing</td><td>Nordic Person</td><td>nordic@school.edu</td></tr>",
            "<tr><td>XC Skiing</td><td>Ski Person</td><td>ski@school.edu</td></tr>",
            "<tr><td>Cross Country-Skiing</td><td>Hyphen Person</td><td>hyphen@school.edu</td></tr>",
            "<tr><td>Cross Country/Skiing</td><td>Slash Person</td><td>slash@school.edu</td></tr>",
            "<tr><td>Nordic/XC</td><td>Winter Person</td><td>winter@school.edu</td></tr>",
            "<tr><td>Former Track Coach</td><td>Old Person</td><td>old@school.edu</td></tr>",
            "<tr><td>Retired Cross Country</td><td>Retired Person</td><td>retired@school.edu</td></tr>"
        ),
    );
    assert!(
        sources::parse(
            &target("Example High School", "https://school.edu/coaches"),
            &body
        )?
        .coaches
        .is_empty()
    );
    Ok(())
}

#[test]
fn given_ski_coach_cards_when_parsed_then_separator_variants_are_not_running_roles() -> Result<()> {
    for role in [
        "Head Cross Country-Skiing Coach",
        "Head Cross Country/Skiing Coach",
        "Head Nordic/XC Coach",
    ] {
        let body = format!(
            "<main><h1>Example High School</h1><div class='staff-card'><span class='staff-name'>Winter Person</span><span class='staff-title'>{role}</span><a href='mailto:winter@school.edu'>winter@school.edu</a></div></main>"
        );
        assert!(
            sources::parse(
                &target("Example High School", "https://school.edu/coaches"),
                &body
            )?
            .coaches
            .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
async fn given_observed_minnesota_school_when_coach_tables_persist_twice_then_claims_and_source_checkpoint_are_idempotent()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target(
                "Wayzata High School",
                "https://www.wayzataathletics.com/coaches-directory/",
            );
            let capture = Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "text/html".into(),
                body: WAYZATA.as_bytes().to_vec(),
            };
            database
                .persist(
                    None,
                    &target,
                    &capture,
                    &ParsedPage {
                        schools: vec![School {
                            id: "school-1".into(),
                            name: "Wayzata High School".into(),
                            state: Some("MN".into()),
                            ..School::default()
                        }],
                        ..ParsedPage::default()
                    },
                )
                .await?;
            let parsed = sources::parse(&target, WAYZATA)?;
            assert_eq!(parsed.coaches.len(), 4);
            for _ in 0..2 {
                database.persist(None, &target, &capture, &parsed).await?;
            }
            let rows: Vec<(String, String, String, serde_json::Value)> =
                sqlx::query_as("SELECT name,email,source_url,raw FROM contacts ORDER BY name")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(rows.len(), 4);
            assert_eq!(
                (&rows[0].0, &rows[0].1),
                (
                    &"Aaron Berndt".to_owned(),
                    &"aaron.berndt@wayzataschools.org".to_owned()
                )
            );
            assert!(
                rows.iter()
                    .all(|r| r.2 == target.url && r.3["checkpoint_url"] == target.url)
            );
            let checkpoint: (i64, String) =
                sqlx::query_as("SELECT count(*),max(status) FROM scrape_pages WHERE url=$1")
                    .bind(&target.url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(checkpoint, (1, "succeeded".into()));
            let annotation = rows[0].3["review_notes"]
                .as_array()
                .context("tenure review annotation")?;
            assert!(annotation.iter().any(|n| {
                n.as_str()
                    .is_some_and(|s| s.starts_with("Current tenure unconfirmed:"))
            }));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[test]
fn given_explicit_running_and_skiing_dual_duties_when_parsed_then_running_claims_are_retained()
-> Result<()> {
    for program in [
        "Track and Field / Nordic Skiing",
        "Cross Country Running / Nordic Skiing",
        "XC Running and Nordic Skiing",
    ] {
        let body = table(
            "<th>Sport</th><th>Coach</th><th>Email</th>",
            &format!("<tr><td>{program}</td><td>Ada Runner</td><td>ada@school.edu</td></tr>"),
        );
        let page = sources::parse(
            &target("Example High School", "https://school.edu/coaches"),
            &body,
        )?;
        assert_eq!(page.coaches.len(), 1, "{program}");
        let card = format!(
            "<main><h1>Example High School</h1><div class='staff-card'><span class='staff-name'>Ada Runner</span><span class='staff-title'>Head {program} Coach</span><a href='mailto:ada@school.edu'>ada@school.edu</a></div></main>"
        );
        assert_eq!(
            sources::parse(
                &target("Example High School", "https://school.edu/coaches"),
                &card
            )?
            .coaches
            .len(),
            1,
            "{program}"
        );
    }
    Ok(())
}
