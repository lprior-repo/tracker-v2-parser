mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    model::{School, Source, Sport},
    sources::wiaa::{self, DirectoryMatch},
};

fn school() -> School {
    School {
        id: "provider-school-123".into(),
        name: "Example".into(),
        state: Some("WI".into()),
        city: Some("Example City".into()),
        ..School::default()
    }
}

fn row(id: &str, name: &str, level: &str, city: &str) -> String {
    format!(
        "<tr><td></td><td><a class='schoolLink' href='/Directory/School/GetDirectorySchool?orgID={id}'><h5>{name}</h5></a></td><td>{level}</td><td>Public</td><td>{city}</td></tr>"
    )
}

fn listing(rows: &str) -> String {
    format!("<table id='tblSchools'><tbody>{rows}</tbody></table>")
}

fn detail() -> String {
    let labels = [
        ("Level", "High School"),
        ("Address", "123 School Road"),
        ("City", "Example City"),
        ("State", "WI"),
        ("Zip", "54405-9730"),
    ]
    .map(|(label, value)| {
        format!("<div class='entryrowPadding'><span>{label}</span><h5>{value}</h5></div>")
    })
    .join("");
    format!(
        "<html><title>Example School</title><main id='SchoolItem'><label class='JumboMain'>Example</label><div id='schoolInfo'>{labels}</div><input name='orgID' value='42'><table id='tblCoachList'><tbody><tr><td></td><td>Boys Track and Field</td><td><label class='formLabelsText'>Ada Runner</label></td><td>Head Coach</td><td><a href='mailto:ada@example.edu'>ada@example.edu</a></td></tr><tr><td></td><td>Girls Cross Country</td><td><label class='formLabelsText'>Ben Walker</label></td><td>Head Coach</td><td><a href='mailto:ben@example.edu'>ben@example.edu</a></td></tr><tr><td></td><td>Football</td><td><label class='formLabelsText'>Wrong Coach</label></td><td>Head Coach</td><td><a href='mailto:football@example.edu'>football@example.edu</a></td></tr></tbody></table></main></html>"
    )
}

#[test]
fn given_published_wiaa_rows_when_matched_then_only_exact_wisconsin_high_school_identity_is_used()
-> Result<()> {
    let body = listing(
        &[
            row("42", "Example", "High School", "Example City"),
            row("43", "Example Middle", "Middle School", "Example City"),
            row("44", "Example", "High School", "Other City"),
        ]
        .join(""),
    );
    let entries = wiaa::parse_listing(&body)?;
    assert_eq!(entries.len(), 2);
    let DirectoryMatch::Matched(entry) = wiaa::match_school(&school(), &entries) else {
        panic!("exact match required")
    };
    let target = wiaa::contact_target(Source::MileSplit, &school(), entry, 2026)?;
    assert_eq!(
        target.url,
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=42"
    );
    assert_eq!(
        target.context.school_id.as_deref(),
        Some("provider-school-123")
    );
    let mut missing = school();
    missing.city = None;
    assert_eq!(
        wiaa::match_school(&missing, &entries),
        DirectoryMatch::Uncorroborated
    );
    missing = school();
    missing.state = Some("MN".into());
    assert_eq!(
        wiaa::match_school(&missing, &entries),
        DirectoryMatch::Uncorroborated
    );
    missing = school();
    missing.name = "Example Similar".into();
    assert_eq!(
        wiaa::match_school(&missing, &entries),
        DirectoryMatch::Missing
    );
    Ok(())
}

#[test]
fn given_duplicate_school_names_in_one_city_when_matching_then_ambiguity_is_retained() -> Result<()>
{
    let entries = wiaa::parse_listing(&listing(
        &[
            row("42", "Example", "High School", "Example City"),
            row("44", "Example", "High School", "Example City"),
        ]
        .join(""),
    ))?;
    assert_eq!(
        wiaa::match_school(&school(), &entries),
        DirectoryMatch::Ambiguous
    );
    assert!(
        wiaa::parse_listing(&listing(&row(
            "0",
            "Example",
            "High School",
            "Example City"
        )))
        .is_err()
    );
    assert!(wiaa::parse_listing("<title>Verify you are human</title>").is_err());
    Ok(())
}

#[test]
fn given_published_wiaa_school_when_parsed_then_postal_address_and_track_xc_coaches_share_original_school_identity()
-> Result<()> {
    let entries = wiaa::parse_listing(&listing(&row(
        "42",
        "Example",
        "High School",
        "Example City",
    )))?;
    let target = wiaa::contact_target(Source::MileSplit, &school(), &entries[0], 2026)?;
    let page = wiaa::parse(&target, &detail())?;
    assert_eq!(page.schools.len(), 1);
    let school = &page.schools[0];
    assert_eq!(school.id, "provider-school-123");
    assert_eq!(school.address.as_deref(), Some("123 School Road"));
    assert_eq!(school.postal_code.as_deref(), Some("54405-9730"));
    assert_eq!(page.coaches.len(), 2);
    assert!(page.next.is_empty());
    assert!(page.coaches.iter().all(|coach| {
        coach
            .review_notes
            .iter()
            .all(|note| !note.starts_with("School affiliation unconfirmed:"))
    }));
    assert!(
        page.coaches
            .iter()
            .any(|coach| coach.email.as_deref() == Some("ada@example.edu")
                && coach.sport.is_none())
    );
    assert!(
        page.coaches
            .iter()
            .any(|coach| coach.email.as_deref() == Some("ben@example.edu")
                && coach.sport == Some(Sport::CrossCountry))
    );
    assert!(
        page.coaches
            .iter()
            .all(|coach| coach.school_id == "provider-school-123")
    );
    assert!(
        wiaa::parse(
            &target,
            &detail().replace("class='JumboMain'>Example", "class='JumboMain'>Other")
        )
        .is_err()
    );
    assert!(wiaa::parse(&target, &detail().replace("value='42'", "value='43'")).is_err());
    assert!(wiaa::parse(&target, &detail().replace("<h5>WI</h5>", "<h5>MN</h5>")).is_err());
    Ok(())
}

#[test]
fn given_published_letter_select_when_discovered_then_only_finite_declared_post_requests_are_produced()
-> Result<()> {
    let options = ('A'..='Z')
        .map(|letter| format!("<option value='{letter}'>{letter}</option>"))
        .collect::<String>();
    let body = format!(
        "<select id='Letter'>{options}</select><script>var url='/Directory/School/DirectoryLetter?LetterBtn=';</script>"
    );
    let requests = wiaa::listing_requests(&body)?;
    assert_eq!(requests.len(), 26);
    assert_eq!(
        requests[0],
        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=A"
    );
    assert_eq!(
        requests[25],
        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=Z"
    );
    assert!(wiaa::listing_requests(&body.replace("value='Z'", "value='ZZ'")).is_err());
    Ok(())
}

#[test]
fn given_foreign_staff_outside_valid_wiaa_table_when_parsed_then_only_table_coaches_are_assigned()
-> Result<()> {
    let entries = wiaa::parse_listing(&listing(&row(
        "42",
        "Example",
        "High School",
        "Example City",
    )))?;
    let target = wiaa::contact_target(Source::MileSplit, &school(), &entries[0], 2026)?;
    let foreign = "<article class='staff-card'><span class='staff-name'>Eve Foreign</span><span class='staff-school'>Other School</span><span class='staff-title'>Head Track Coach</span><a href='mailto:foreign@other.example'>Published email</a></article>";
    let body = detail().replace("</main>", &format!("{foreign}</main>"));
    let page = wiaa::parse(&target, &body)?;
    assert_eq!(page.coaches.len(), 2);
    assert!(
        page.coaches
            .iter()
            .all(|coach| coach.name.as_deref() != Some("Eve Foreign")
                && coach.email.as_deref() != Some("foreign@other.example"))
    );
    assert!(page.coaches.iter().all(|coach| {
        coach
            .review_notes
            .iter()
            .any(|note| note.starts_with("Current tenure unconfirmed:"))
    }));
    let no_table = wiaa::parse(
        &target,
        &body.replace("id='tblCoachList'", "id='unrelated-table'"),
    )?;
    assert!(no_table.coaches.is_empty());
    assert!(no_table.notes.iter().any(|note| {
        note.starts_with("No unambiguous public track/XC coaching mailbox extracted;")
    }));
    assert_eq!(
        no_table.schools[0].address.as_deref(),
        Some("123 School Road")
    );
    assert!(
        wiaa::parse(
            &target,
            &body.replace("</main>", "<table id='tblCoachList'></table></main>")
        )
        .is_err()
    );
    let error = wiaa::parse(
        &target,
        &body.replace(
            "<main id='SchoolItem'>",
            "<main id='SchoolItem'><h1>Verify you are human</h1>",
        ),
    )
    .expect_err("original full-page challenge remains blocking");
    assert!(
        error
            .to_string()
            .contains("Official school website access challenge")
    );
    Ok(())
}

#[tokio::test]
async fn given_wiaa_capture_when_replayed_twice_through_cli_then_school_and_coaches_persist_once()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let input = test.directory().join("published-wiaa-school.html");
        std::fs::write(&input, detail())?;
        let args = ["scrape", "--source", "milesplit", "--kind", "contacts", "--url", "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=42", "--input", input.to_str().context("UTF8 capture path")?, "--school-id", "provider-school-123", "--school-name", "Example", "--state", "WI", "--season", "2026", "--subject-id", "contacts-depth:2"];
        support::assert_success(&test.cli(&args).await?);
        support::assert_success(&test.cli(&args).await?);
        let schools: Vec<(String, String, String, String)> = sqlx::query_as("SELECT source_id,address,city,postal_code FROM schools ORDER BY id").fetch_all(&test.pool).await?;
        assert_eq!(schools, vec![("provider-school-123".into(),"123 School Road".into(),"Example City".into(),"54405-9730".into())]);
        let coaches: Vec<(String, String, String)> = sqlx::query_as("SELECT s.source_id,c.name,c.email FROM contacts c JOIN schools s ON s.id=c.school_id WHERE c.kind='coach' ORDER BY c.email").fetch_all(&test.pool).await?;
        assert_eq!(coaches, vec![("provider-school-123".into(),"Ada Runner".into(),"ada@example.edu".into()),("provider-school-123".into(),"Ben Walker".into(),"ben@example.edu".into())]);
        let checkpoint: (String,i32) = sqlx::query_as("SELECT status,attempts FROM scrape_pages").fetch_one(&test.pool).await?;
        assert_eq!(checkpoint,("succeeded".into(),1));
        Ok(())
    })).await
}
