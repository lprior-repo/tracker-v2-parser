#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use std::collections::BTreeMap;
use track_scrapper_v2::{
    database::Database,
    model::{Coach, PageContext, PageKind, ParsedPage, School, Source, Sport, Target},
};

fn coach(name: Option<&str>, email: Option<&str>, sport: Option<Sport>) -> Coach {
    Coach {
        school_id: "school-1".into(),
        name: name.map(str::to_owned),
        role: "Head Coach".into(),
        email: email.map(str::to_owned),
        sport,
        review_notes: Vec::new(),
    }
}

fn contact_target(url: &str) -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        url.into(),
        PageContext {
            school_id: Some("school-1".into()),
            school_name: Some("Example High School".into()),
            state: Some("WI".into()),
            ..PageContext::default()
        },
    )
}

fn page(coaches: Vec<Coach>, office: Option<&str>) -> ParsedPage {
    let mut athlete = records::athlete("11");
    athlete.school_id = Some("school-1".into());
    ParsedPage {
        schools: vec![School {
            id: "school-1".into(),
            name: "Example High School".into(),
            state: Some("WI".into()),
            public_email: office.map(str::to_owned),
            ..School::default()
        }],
        athletes: vec![athlete],
        coaches,
        ..ParsedPage::default()
    }
}

async fn export_row(test: &support::Scenario, filename: &str) -> Result<BTreeMap<String, String>> {
    let output = test.directory().join(filename);
    support::assert_success(
        &test
            .cli(&[
                "export",
                "--output",
                output.to_str().context("UTF-8 output")?,
            ])
            .await?,
    );
    let mut workbook = open_workbook_auto(&output)?;
    let sheet = workbook.worksheet_range("Athletes")?;
    assert_eq!(sheet.height(), 2);
    let mut rows = sheet.rows();
    let headers = rows.next().context("athlete headers")?;
    let values = rows.next().context("athlete row")?;
    let fields = headers
        .iter()
        .zip(values)
        .map(|(header, value)| (header.to_string(), value.to_string()))
        .collect();
    let issues = workbook.worksheet_range("Issues")?;
    assert!(issues.rows().skip(1).any(|row| row.iter().any(|cell| {
        cell.to_string().contains("missing public coach email")
            || cell.to_string().contains("missing published coach name")
            || cell.to_string().contains("Earlier contact source claim")
            || cell.to_string().contains("uncertain affiliation")
            || cell
                .to_string()
                .contains("Current tenure is not confirmed by source")
    })));
    Ok(fields)
}

#[tokio::test]
async fn given_named_coach_without_email_when_saved_twice_and_cli_exported_then_name_and_review_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = contact_target("https://school.example.edu/staff");
            let page = page(
                vec![coach(Some("Ada Runner"), None, Some(Sport::CrossCountry))],
                None,
            );
            records::persist(&database, &target, &page).await?;
            let before: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(
                "SELECT id,name,email,source_key FROM contacts WHERE kind='coach' ORDER BY id",
            )
            .fetch_all(&test.pool)
            .await?;
            records::persist(&database, &target, &page).await?;
            let after: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(
                "SELECT id,name,email,source_key FROM contacts WHERE kind='coach' ORDER BY id",
            )
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(after, before);
            assert_eq!(after.len(), 1);
            assert_eq!(
                (&after[0].1, &after[0].2),
                (&"Ada Runner".to_owned(), &None)
            );
            let records = database.athlete_records().await?;
            assert_eq!(records[0].coaches[0].email, None);
            assert_eq!(records[0].coaches[0].sport, Some(Sport::CrossCountry));
            let row = export_row(test, "nullable.xlsx").await?;
            assert_eq!(row["coach_names"], "Ada Runner");
            assert_eq!(row["coach_emails"], "");
            assert_eq!(row["needs_review"], "true");
            assert!(row["review_notes"].contains("missing public coach email"));
            assert!(!row["review_notes"].contains("missing published coach name"));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_distinct_names_and_sports_when_replayed_then_no_email_and_shared_mailbox_claims_do_not_collapse()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = contact_target("https://school.example.edu/staff");
            let page = page(
                vec![
                    coach(Some("Ada Runner"), None, Some(Sport::Outdoor)),
                    coach(Some("Ben Walker"), None, Some(Sport::Outdoor)),
                    coach(
                        Some("Chris Thrower"),
                        Some("staff@school.example.edu"),
                        Some(Sport::Outdoor),
                    ),
                    coach(
                        Some("Dina Jumper"),
                        Some("staff@school.example.edu"),
                        Some(Sport::Outdoor),
                    ),
                    coach(
                        Some("Chris Thrower"),
                        Some("staff@school.example.edu"),
                        Some(Sport::CrossCountry),
                    ),
                ],
                Some("staff@school.example.edu"),
            );
            records::persist(&database, &target, &page).await?;
            let before: Vec<(i64, String, String)> =
                sqlx::query_as("SELECT id,kind,source_key FROM contacts ORDER BY id")
                    .fetch_all(&test.pool)
                    .await?;
            records::persist(&database, &target, &page).await?;
            let after: Vec<(i64, String, String)> =
                sqlx::query_as("SELECT id,kind,source_key FROM contacts ORDER BY id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(after, before);
            assert_eq!(after.len(), 6);
            let claims: Vec<(String, Option<String>, String)> = sqlx::query_as(
                "SELECT name,email,sport FROM contacts WHERE kind='coach' ORDER BY name,sport",
            )
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(
                claims,
                vec![
                    ("Ada Runner".into(), None, "outdoor".into()),
                    ("Ben Walker".into(), None, "outdoor".into()),
                    (
                        "Chris Thrower".into(),
                        Some("staff@school.example.edu".into()),
                        "outdoor".into()
                    ),
                    (
                        "Chris Thrower".into(),
                        Some("staff@school.example.edu".into()),
                        "xc".into()
                    ),
                    (
                        "Dina Jumper".into(),
                        Some("staff@school.example.edu".into()),
                        "outdoor".into()
                    ),
                ]
            );
            let row = export_row(test, "partial.xlsx").await?;
            assert_eq!(
                row["coach_names"],
                "Ada Runner; Ben Walker; Chris Thrower; Dina Jumper"
            );
            assert_eq!(row["coach_emails"], "staff@school.example.edu");
            assert_eq!(row["school_public_emails"], "staff@school.example.edu");
            assert_eq!(row["needs_review"], "true");
            assert!(row["review_notes"].contains("missing public coach email"));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_mailbox_without_name_when_cli_exported_then_name_is_blank_and_reviewed_not_inferred()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let page = page(
                vec![
                    coach(Some("Ada Runner"), Some("ada@school.example.edu"), None),
                    coach(
                        None,
                        Some("ben.walker@school.example.edu"),
                        Some(Sport::CrossCountry),
                    ),
                ],
                None,
            );
            records::persist(
                &database,
                &contact_target("https://school.example.edu/staff"),
                &page,
            )
            .await?;
            let row = export_row(test, "missing-name.xlsx").await?;
            assert_eq!(row["coach_names"], "Ada Runner");
            assert_eq!(
                row["coach_emails"],
                "ada@school.example.edu; ben.walker@school.example.edu"
            );
            assert_eq!(row["needs_review"], "true");
            assert!(row["review_notes"].contains("missing published coach name"));
            assert!(!row["review_notes"].contains("missing public coach email"));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_case_variants_or_new_provenance_when_saved_then_normalized_replay_and_distinct_sources_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = contact_target("https://school.example.edu/staff");
            let mut page = page(
                vec![coach(
                    Some("Ada Runner"),
                    Some("ADA@school.example.edu"),
                    None,
                )],
                Some("OFFICE@school.example.edu"),
            );
            records::persist(&database, &target, &page).await?;
            page.coaches[0].email = Some("ada@school.example.edu".into());
            page.schools[0].public_email = Some("office@school.example.edu".into());
            page.schools[0].city = Some("Published City".into());
            records::persist(&database, &target, &page).await?;
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 2);
            let other = contact_target("https://school.example.edu/athletics/staff");
            records::persist(&database, &other, &page).await?;
            let provenance: Vec<(String, String)> =
                sqlx::query_as("SELECT kind,source_url FROM contacts ORDER BY kind,source_url")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(
                provenance,
                vec![
                    ("coach".into(), other.url.clone()),
                    ("coach".into(), target.url.clone()),
                    ("school_office".into(), other.url),
                    ("school_office".into(), target.url),
                ]
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_invalid_or_fabricated_contact_when_saved_then_constraints_reject_and_page_rolls_back()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = contact_target("https://school.example.edu/staff");
        for (name, email, constraint) in [
            (Some("Ada Runner"), Some(""), "contacts_email_check"),
            (Some("Ada Runner"), Some("not-an-email"), "contacts_email_check"),
            (Some("Ada Runner"), Some("ada@@school.example.edu"), "contacts_email_check"),
            (None, None, "contacts_published_identity_check"),
            (Some("  "), None, "contacts_published_identity_check"),
            (Some("\t\r\n"), None, "contacts_published_identity_check"),
        ] {
            let error = records::persist(&database, &target, &page(vec![coach(name, email, None)], None))
                .await.err().context("invalid contact was accepted")?;
            let error = error.downcast_ref::<sqlx::Error>().context("SQLx constraint error")?;
            let error = error.as_database_error().context("database constraint error")?;
            assert_eq!(error.code().as_deref(), Some("23514"));
            assert_eq!(error.constraint(), Some(constraint));
            let counts: (i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM contacts),(SELECT count(*) FROM schools),(SELECT count(*) FROM scrape_pages)",
            ).fetch_one(&test.pool).await?;
            assert_eq!(counts, (0, 0, 0));
        }
        database.close().await;
        Ok(())
    })).await
}

const EARLIER_CLAIM: &str = "Earlier contact source claim; not confirmed by latest parse";

async fn saved_contacts(test: &support::Scenario) -> Result<Vec<serde_json::Value>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(c) FROM contacts c ORDER BY id")
            .fetch_all(&test.pool)
            .await?,
    )
}

#[tokio::test]
async fn given_changed_and_removed_claims_when_replayed_then_old_evidence_is_flagged_and_current_claims_are_exported()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = contact_target("https://school.example.edu/staff");
        let old = page(vec![
            coach(Some("Ada Runner"), Some("old@school.example.edu"), None),
            coach(Some("Ben Walker"), Some("ben@school.example.edu"), None),
            coach(Some("Chris Thrower"), Some("chris@school.example.edu"), None),
        ], None);
        records::persist(&database, &target, &old).await?;
        let before = saved_contacts(test).await?;
        let current = page(vec![
            coach(Some("Ada Runner"), Some("new@school.example.edu"), None),
            coach(Some("Chris Thrower"), Some("chris@school.example.edu"), None),
        ], None);
        records::persist(&database, &target, &current).await?;
        let after = saved_contacts(test).await?;
        assert_eq!(after.len(), 4);
        for (index, prior) in before.iter().enumerate() {
            assert_eq!(after[index]["id"], prior["id"]);
            assert_eq!(after[index]["source_key"], prior["source_key"]);
            assert_eq!(after[index]["raw"]["name"], prior["raw"]["name"]);
            assert_eq!(after[index]["raw"]["email"], prior["raw"]["email"]);
        }
        assert_eq!(after[0]["raw"]["review_notes"], serde_json::json!([EARLIER_CLAIM]));
        assert_eq!(after[1]["raw"]["review_notes"], serde_json::json!([EARLIER_CLAIM]));
        assert_eq!(after[2]["raw"].get("review_notes"), None);
        assert_eq!(after[3]["raw"].get("review_notes"), None);
        let records = database.athlete_records().await?;
        assert_eq!(records[0].coaches[0].review_notes, vec![EARLIER_CLAIM]);
        assert_eq!(records[0].coaches[3].review_notes, Vec::<String>::new());
        let row = export_row(test, "changed-claims.xlsx").await?;
        assert_eq!(row["coach_names"], "Ada Runner; Ben Walker; Chris Thrower");
        assert_eq!(row["coach_emails"], "ben@school.example.edu; chris@school.example.edu; new@school.example.edu; old@school.example.edu");
        assert_eq!(row["needs_review"], "true");
        assert!(row["review_notes"].contains("Ada Runner <old@school.example.edu>"));
        assert!(row["review_notes"].contains("Ben Walker <ben@school.example.edu>"));
        assert!(row["review_notes"].contains(EARLIER_CLAIM));
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_existing_claim_with_annotations_when_empty_parse_then_reconfirmation_retains_ids_and_refreshes_only_notes()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = contact_target("https://school.example.edu/staff");
            let mut current = page(
                vec![coach(
                    Some("Ada Runner"),
                    Some("ada@school.example.edu"),
                    None,
                )],
                None,
            );
            current.coaches[0].review_notes = vec!["uncertain affiliation".into()];
            records::persist(&database, &target, &current).await?;
            let before = saved_contacts(test).await?;
            let empty = ParsedPage {
                notes: vec!["Ambiguous or incomplete contact parse".into()],
                ..ParsedPage::default()
            };
            records::persist(&database, &target, &empty).await?;
            records::persist(&database, &target, &empty).await?;
            let flagged = saved_contacts(test).await?;
            assert_eq!(flagged.len(), 1);
            assert_eq!(flagged[0]["id"], before[0]["id"]);
            assert_eq!(flagged[0]["source_key"], before[0]["source_key"]);
            assert_eq!(
                flagged[0]["raw"]["review_notes"],
                serde_json::json!(["uncertain affiliation", EARLIER_CLAIM])
            );
            current.coaches[0].review_notes =
                vec!["Current tenure is not confirmed by source".into()];
            records::persist(&database, &target, &current).await?;
            records::persist(&database, &target, &current).await?;
            let confirmed = saved_contacts(test).await?;
            assert_eq!(confirmed.len(), 1);
            assert_eq!(confirmed[0]["id"], before[0]["id"]);
            assert_eq!(confirmed[0]["source_key"], before[0]["source_key"]);
            assert_eq!(
                confirmed[0]["raw"]["review_notes"],
                serde_json::json!(["Current tenure is not confirmed by source"])
            );
            let records = database.athlete_records().await?;
            assert_eq!(
                records[0].coaches[0].review_notes,
                current.coaches[0].review_notes
            );
            let row = export_row(test, "annotated-current.xlsx").await?;
            assert_eq!(row["coach_names"], "Ada Runner");
            assert_eq!(row["coach_emails"], "ada@school.example.edu");
            assert_eq!(row["needs_review"], "true");
            assert!(row["review_notes"].contains("Current tenure is not confirmed by source"));
            assert!(!row["review_notes"].contains(EARLIER_CLAIM));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_multiple_schools_sources_pages_and_offices_when_empty_contact_parse_then_only_exact_source_page_claims_are_flagged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = contact_target("https://www.athletic.net/team/3204/coaches");
            let observed = page(
                vec![coach(
                    Some("Ada Runner"),
                    Some("ada@school.example.edu"),
                    None,
                )],
                Some("office@school.example.edu"),
            );
            records::persist(&database, &target, &observed).await?;
            let other_page = contact_target("https://school.example.edu/other-staff");
            records::persist(&database, &other_page, &observed).await?;
            let mut other_school = observed.clone();
            other_school.athletes.clear();
            other_school.schools[0].id = "school-2".into();
            other_school.coaches[0].school_id = "school-2".into();
            let mut other_target = target.clone();
            other_target.context.school_id = Some("school-2".into());
            records::persist(&database, &other_target, &other_school).await?;
            let mut other_source = observed.clone();
            other_source.athletes.clear();
            other_target = target.clone();
            other_target.source = Source::MileSplit;
            records::persist(&database, &other_target, &other_source).await?;
            let before = saved_contacts(test).await?;
            let mut non_contact = target.clone();
            non_contact.kind = PageKind::Team;
            records::persist(&database, &non_contact, &ParsedPage::default()).await?;
            assert_eq!(saved_contacts(test).await?, before);
            records::persist(&database, &target, &ParsedPage::default()).await?;
            let after = saved_contacts(test).await?;
            assert_eq!(after.len(), before.len());
            let original_school = before
                .iter()
                .find(|row| row["kind"] == "coach")
                .context("original coach")?["school_id"]
                .clone();
            for (prior, updated) in before.iter().zip(&after) {
                if prior["kind"] == "coach"
                    && prior["school_id"] == original_school
                    && prior["source_url"] == target.url
                {
                    assert_eq!(
                        updated["raw"]["review_notes"],
                        serde_json::json!([EARLIER_CLAIM])
                    );
                    assert_eq!(updated["id"], prior["id"]);
                    assert_eq!(updated["source_key"], prior["source_key"]);
                } else {
                    assert_eq!(updated, prior);
                }
            }
            records::persist(&database, &other_page, &ParsedPage::default()).await?;
            let unrelated: Vec<_> = saved_contacts(test)
                .await?
                .into_iter()
                .filter(|row| {
                    row["school_id"] != original_school || row["source_url"] != target.url
                })
                .collect();
            records::persist(&database, &target, &observed).await?;
            let restored = saved_contacts(test).await?;
            let primary = restored
                .iter()
                .find(|row| {
                    row["kind"] == "coach"
                        && row["school_id"] == original_school
                        && row["source_url"] == target.url
                })
                .context("reconfirmed primary claim")?;
            assert_eq!(primary["raw"].get("review_notes"), None);
            let other = restored
                .iter()
                .find(|row| {
                    row["kind"] == "coach"
                        && row["school_id"] == original_school
                        && row["source_url"] == other_page.url
                })
                .context("unrelated page claim")?;
            assert_eq!(
                other["raw"]["review_notes"],
                serde_json::json!([EARLIER_CLAIM])
            );
            for row in unrelated.iter().filter(|row| row["kind"] == "coach") {
                assert_eq!(
                    restored.iter().find(|restored| restored["id"] == row["id"]),
                    Some(row)
                );
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_redirected_claim_when_original_checkpoint_redirect_changes_then_previous_effective_claim_is_retained_and_flagged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = contact_target("https://school.example.edu/staff");
            let observed = page(
                vec![coach(
                    Some("Ada Runner"),
                    Some("ada@school.example.edu"),
                    None,
                )],
                None,
            );
            let capture = |url: &str| track_scrapper_v2::model::Capture {
                url: url.into(),
                status: 200,
                content_type: "text/html".into(),
                body: Vec::new(),
            };
            database
                .persist(
                    None,
                    &target,
                    &capture("https://school.example.edu/staff-2025"),
                    &observed,
                )
                .await?;
            let before = saved_contacts(test).await?;
            database
                .persist(
                    None,
                    &target,
                    &capture("https://school.example.edu/staff-2026"),
                    &observed,
                )
                .await?;
            let after = saved_contacts(test).await?;
            assert_eq!(after.len(), 2);
            assert_eq!(after[0]["id"], before[0]["id"]);
            assert_eq!(after[0]["source_key"], before[0]["source_key"]);
            assert_eq!(after[0]["raw"]["checkpoint_url"], target.url);
            assert_eq!(
                after[0]["raw"]["review_notes"],
                serde_json::json!([EARLIER_CLAIM])
            );
            assert_eq!(after[1]["raw"].get("review_notes"), None);
            database
                .persist(
                    None,
                    &target,
                    &capture("https://school.example.edu/staff-2027"),
                    &ParsedPage::default(),
                )
                .await?;
            let empty = saved_contacts(test).await?;
            assert_eq!(empty.len(), 2);
            assert_eq!(
                empty[0]["raw"]["review_notes"],
                serde_json::json!([EARLIER_CLAIM])
            );
            assert_eq!(
                empty[1]["raw"]["review_notes"],
                serde_json::json!([EARLIER_CLAIM])
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_existing_contacts_when_upsert_or_checkpoint_fails_then_annotations_contacts_and_checkpoint_roll_back()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = contact_target("https://school.example.edu/staff");
        let observed = page(vec![coach(Some("Ada Runner"), Some("old@school.example.edu"), None)], None);
        records::persist(&database, &target, &observed).await?;
        let before = saved_contacts(test).await?;
        let checkpoint: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p")
            .fetch_one(&test.pool).await?;
        let invalid = page(vec![
            coach(Some("Ada Runner"), Some("new@school.example.edu"), None),
            coach(Some("Ben Walker"), Some("invalid-mailbox"), None),
        ], None);
        let error = records::persist(&database, &target, &invalid).await.err().context("invalid contact accepted")?;
        let error = error.downcast_ref::<sqlx::Error>().context("SQLx error")?;
        assert_eq!(error.as_database_error().and_then(|error| error.constraint()), Some("contacts_email_check"));
        assert_eq!(saved_contacts(test).await?, before);
        let failed_checkpoint: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p")
            .fetch_one(&test.pool).await?;
        assert_eq!(failed_checkpoint, checkpoint);
        sqlx::query("ALTER TABLE scrape_pages ADD CONSTRAINT reject_contact_checkpoint CHECK (NOT ('reject-checkpoint' = ANY(notes)))")
            .execute(&test.pool).await?;
        let mut changed = page(vec![coach(Some("Ada Runner"), Some("new@school.example.edu"), None)], None);
        changed.notes = vec!["reject-checkpoint".into()];
        let error = records::persist(&database, &target, &changed).await.err().context("checkpoint accepted")?;
        let error = error.downcast_ref::<sqlx::Error>().context("SQLx error")?;
        assert_eq!(error.as_database_error().and_then(|error| error.constraint()), Some("reject_contact_checkpoint"));
        assert_eq!(saved_contacts(test).await?, before);
        let failed_checkpoint: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p")
            .fetch_one(&test.pool).await?;
        assert_eq!(failed_checkpoint, checkpoint);
        database.close().await;
        Ok(())
    })).await
}
