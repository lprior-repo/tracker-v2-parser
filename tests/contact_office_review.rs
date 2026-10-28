use anyhow::{Context, Result};
use track_scrapper_v2::{
    model::{PageContext, PageKind, ParsedPage, Source, Target},
    sources::contacts,
};

fn parse(body: &str) -> Result<ParsedPage> {
    let target = Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        "https://school.example.edu/contact".to_owned(),
        PageContext {
            school_id: Some("school-review-5".to_owned()),
            school_name: Some("Example High School".to_owned()),
            ..PageContext::default()
        },
    );
    contacts::parse(&target, body)
}

fn assert_office_unknown(page: &ParsedPage) {
    assert!(
        page.schools
            .iter()
            .all(|school| school.public_email.is_none())
    );
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("office mailbox"))
    );
}

#[test]
fn given_conflicting_school_headings_when_office_parsed_then_email_is_unknown() -> Result<()> {
    for heading in [
        "<main><h1>Other High School</h1></main>",
        "<header><h1>Other High School</h1></header>",
    ] {
        let when = parse(&format!(
            "<title>Example High School</title>{heading}<main><p>Main Office: <a href='mailto:other@school.edu'>Email office</a></p></main>"
        ))?;
        assert_office_unknown(&when);
    }
    Ok(())
}

#[test]
fn given_matching_heading_and_conflicting_title_when_office_parsed_then_email_is_unknown()
-> Result<()> {
    let given = "<title>Other High School</title><main><h1>Example High School</h1><p>Main Office: other@school.edu</p></main>";
    let when = parse(given)?;
    assert_office_unknown(&when);
    Ok(())
}

#[test]
fn given_overlapping_school_names_when_office_parsed_then_other_school_email_is_unknown()
-> Result<()> {
    let given = "<title>Example High School East</title><main><h1>Example High School East</h1><p>Example High School East Main Office: east@school.edu</p></main>";
    let when = parse(given)?;
    assert_office_unknown(&when);
    Ok(())
}

#[test]
fn given_matching_page_with_other_school_office_when_parsed_then_email_is_unknown() -> Result<()> {
    let given = "<main><h1>Example High School</h1><p>Example High School East Main Office: east@school.edu</p></main>";
    let when = parse(given)?;
    assert_office_unknown(&when);
    Ok(())
}

#[test]
fn given_hidden_office_links_when_parsed_then_stale_mailboxes_are_unknown() -> Result<()> {
    [
        "<a hidden href='mailto:stale@school.edu'>Email office</a>",
        "<span hidden><a href='mailto:stale@school.edu'>Email office</a></span>",
        "<a aria-hidden='true' href='mailto:stale@school.edu'>Email office</a>",
        "<a style='display: none' href='mailto:stale@school.edu'>Email office</a>",
        "<span style='visibility: hidden'><a href='mailto:stale@school.edu'>Email office</a></span>",
    ]
    .into_iter()
    .try_for_each(|given| -> Result<()> {
        let when = parse(&format!(
            "<main><h1>Example High School</h1><p>Main Office: {given}</p></main>"
        ))?;
        assert_office_unknown(&when);
        Ok(())
    })
}

#[test]
fn given_visible_and_hidden_office_emails_when_parsed_then_visible_email_survives() -> Result<()> {
    [
        "<a hidden href='mailto:stale@school.edu'>stale@school.edu</a>",
        "<span aria-hidden='true'>stale@school.edu</span>",
        "<span style='DISPLAY : none'>stale@school.edu</span>",
        "<span style='visibility: hidden'><a href='mailto:stale@school.edu'>stale@school.edu</a></span>",
    ]
    .into_iter()
    .try_for_each(|given| -> Result<()> {
        let when = parse(&format!(
            "<main><h1>Example High School</h1><p>Main Office: <a href='mailto:office@school.edu'>Email office</a>{given}</p></main>"
        ))?;
        assert_eq!(
            when.schools.first().context("missing visible office")?.public_email.as_deref(),
            Some("office@school.edu")
        );
        assert!(!when.notes.iter().any(|note| note.contains("office mailbox")));
        Ok(())
    })
}

#[test]
fn given_hidden_office_block_when_parsed_then_stale_email_is_unknown() -> Result<()> {
    let given = "<main><h1>Example High School</h1><p hidden>Main Office: <a href='mailto:stale@school.edu'>Email office</a></p></main>";
    let when = parse(given)?;
    assert_office_unknown(&when);
    Ok(())
}

#[test]
fn given_unambiguous_school_scoped_office_when_parsed_then_public_email_is_accepted() -> Result<()>
{
    let given = "<title>Example High School Athletics</title><main><h1>Example High School</h1><p>Contact the athletics office: <a href='Mailto:OFFICE@school.edu?subject=Question'>Email office</a></p></main>";
    let when = parse(given)?;
    let then = when.schools.first().context("missing scoped office")?;
    assert_eq!(then.id, "school-review-5");
    assert_eq!(then.public_email.as_deref(), Some("office@school.edu"));
    assert!(when.coaches.is_empty());
    Ok(())
}

#[test]
fn given_explicit_school_named_office_without_local_page_when_parsed_then_email_is_accepted()
-> Result<()> {
    let given = "<main><h1>School contacts</h1><p>Example High School Main Office: office@school.edu</p></main>";
    let when = parse(given)?;
    assert_eq!(
        when.schools
            .first()
            .context("missing explicitly named office")?
            .public_email
            .as_deref(),
        Some("office@school.edu")
    );
    Ok(())
}

#[test]
fn given_visible_office_and_hidden_stale_block_when_parsed_then_visible_email_survives()
-> Result<()> {
    let given = "<main><h1>Example High School</h1><p>Main Office: office@school.edu</p><p hidden>Main Office: stale@school.edu</p></main>";
    let when = parse(given)?;
    assert_eq!(
        when.schools
            .first()
            .context("missing visible office")?
            .public_email
            .as_deref(),
        Some("office@school.edu")
    );
    assert!(
        !when
            .notes
            .iter()
            .any(|note| note.contains("office mailbox"))
    );
    Ok(())
}

#[test]
fn given_visible_school_heading_and_hidden_conflict_when_parsed_then_office_is_accepted()
-> Result<()> {
    let given = "<main><h1>Example High School</h1><h1 style='display:none'>Other High School</h1><p>Main Office: office@school.edu</p></main>";
    let when = parse(given)?;
    assert_eq!(
        when.schools
            .first()
            .context("missing visible school office")?
            .public_email
            .as_deref(),
        Some("office@school.edu")
    );
    Ok(())
}

#[test]
fn given_multiple_visible_office_mailboxes_when_parsed_then_email_remains_unknown() -> Result<()> {
    let given = "<main><h1>Example High School</h1><p>Main Office: first@school.edu second@school.edu</p></main>";
    let when = parse(given)?;
    assert_office_unknown(&when);
    Ok(())
}
