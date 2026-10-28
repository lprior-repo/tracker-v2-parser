#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use records::{athlete, performance, persist, target};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Athlete, ParsedPage, School, Source, Sport, UnassignedResult},
};

fn unassigned(id: &str) -> UnassignedResult {
    UnassignedResult {
        result_id: Some(id.to_owned()),
        source_row: Some("published-row-1".to_owned()),
        event: "100 Meters".to_owned(),
        mark: "12.34".to_owned(),
        sport: Sport::Outdoor,
        school_id: None,
        meet_id: Some("meet-1".to_owned()),
        date: Some("2026-05-15".to_owned()),
        is_relay: false,
        members: Vec::new(),
        raw: json!({"published": "retained"}),
    }
}

async fn stored(pool: &sqlx::PgPool, id: &str) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object('event', event, 'mark', raw_mark, 'meet_id', meet_id, \
         'date', meet_date, 'numeric', normalized_numeric, 'unit', normalized_unit, \
         'status', status, 'raw', raw, 'unassigned', athlete_id IS NULL) \
         FROM results WHERE source_result_id=$1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?)
}

async fn result_count(pool: &sqlx::PgPool) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM results")
        .fetch_one(pool)
        .await?)
}

#[tokio::test]
async fn given_unassigned_rich_rows_when_sparse_then_corrected_replayed_then_known_facts_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target(Source::MileSplit, "unassigned-review");
            let rich = unassigned("unassigned-review");
            let mut page = ParsedPage {
                unassigned_results: vec![rich.clone()],
                ..ParsedPage::default()
            };
            persist(&database, &target, &page).await?;
            let original = stored(&test.pool, "unassigned-review").await?;
            assert_eq!(original["numeric"], 12_340_000_000_i64);
            assert_eq!(original["unit"], "ns");
            let mut sparse = rich;
            sparse.event.clear();
            sparse.mark = " ".to_owned();
            sparse.date = None;
            sparse.meet_id = None;
            sparse.source_row = None;
            sparse.raw = json!({});
            page.unassigned_results = vec![sparse];
            persist(&database, &target, &page).await?;
            persist(&database, &target, &page).await?;
            assert_eq!(stored(&test.pool, "unassigned-review").await?, original);
            let corrected = page
                .unassigned_results
                .first_mut()
                .context("unassigned row")?;
            corrected.event = "200 Meters".to_owned();
            corrected.mark = "25.00".to_owned();
            corrected.date = Some("2026-05-16".to_owned());
            persist(&database, &target, &page).await?;
            persist(&database, &target, &page).await?;
            let updated = stored(&test.pool, "unassigned-review").await?;
            assert_eq!(updated["event"], "200 Meters");
            assert_eq!(updated["mark"], "25.00");
            assert_eq!(updated["date"], "2026-05-16");
            assert_eq!(updated["meet_id"], "meet-1");
            assert_eq!(updated["numeric"], 25_000_000_000_i64);
            assert_eq!(updated["raw"]["event"], "200 Meters");
            assert_eq!(updated["raw"]["date"], "2026-05-16");
            assert_eq!(updated["raw"]["raw"]["published"], "retained");
            assert_eq!(result_count(&test.pool).await?, 1);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_recorded_rows_when_status_only_dq_replayed_then_sql_and_raw_agree() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target(Source::AthleticNet, "11");
            let mut page = ParsedPage {
                athletes: vec![athlete("11")],
                performances: vec![performance("12.34")],
                unassigned_results: vec![unassigned("unassigned-dq")],
                ..ParsedPage::default()
            };
            persist(&database, &target, &page).await?;
            assert_eq!(stored(&test.pool, "race-1").await?["status"], "recorded");
            let assigned = page.performances.first_mut().context("assigned row")?;
            assigned.event.clear();
            assigned.mark.clear();
            assigned.reported_status = Some("DQ".to_owned());
            let unassigned = page
                .unassigned_results
                .first_mut()
                .context("unassigned row")?;
            unassigned.event.clear();
            unassigned.mark.clear();
            unassigned.raw = json!({"statusCode": "DQ"});
            persist(&database, &target, &page).await?;
            persist(&database, &target, &page).await?;
            for id in ["race-1", "unassigned-dq"] {
                let result = stored(&test.pool, id).await?;
                assert_eq!(result["event"], "100 Meters");
                assert_eq!(result["mark"], "12.34");
                assert_eq!(result["status"], "DQ");
                assert_eq!(result["numeric"], Value::Null);
                assert_eq!(result["unit"], Value::Null);
                assert_eq!(result["raw"]["event"], "100 Meters");
                assert_eq!(result["raw"]["mark"], "12.34");
            }
            assert_eq!(
                stored(&test.pool, "race-1").await?["raw"]["reported_status"],
                "DQ"
            );
            assert_eq!(
                stored(&test.pool, "unassigned-dq").await?["raw"]["raw"]["statusCode"],
                "DQ"
            );
            let records = database.athlete_records().await?;
            let evidence = records
                .first()
                .and_then(|row| row.performances.first())
                .context("assigned evidence")?;
            assert_eq!(evidence.performance.reported_status.as_deref(), Some("DQ"));
            assert!(track_scrapper_v2::marks::comparable(evidence).is_none());
            assert_dq_replay_correction(&database, &target, &mut page, &test.pool).await?;
            assert_eq!(result_count(&test.pool).await?, 2);
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn assert_dq_replay_correction(
    database: &Database,
    target: &track_scrapper_v2::model::Target,
    page: &mut ParsedPage,
    pool: &sqlx::PgPool,
) -> Result<()> {
    page.performances
        .first_mut()
        .context("assigned row")?
        .reported_status = None;
    page.unassigned_results
        .first_mut()
        .context("unassigned row")?
        .raw = json!({});
    persist(database, target, page).await?;
    assert_eq!(stored(pool, "race-1").await?["status"], "DQ");
    assert_eq!(stored(pool, "unassigned-dq").await?["status"], "DQ");
    page.performances = vec![performance("12.35")];
    let mut corrected = unassigned("unassigned-dq");
    corrected.mark = "12.35".to_owned();
    page.unassigned_results = vec![corrected];
    persist(database, target, page).await?;
    persist(database, target, page).await?;
    for id in ["race-1", "unassigned-dq"] {
        let result = stored(pool, id).await?;
        assert_eq!(result["status"], "recorded");
        assert_eq!(result["numeric"], 12_350_000_000_i64);
        assert_eq!(result["unit"], "ns");
        assert_eq!(result["mark"], "12.35");
        assert!(result["raw"].get("reported_status").is_none());
        assert!(result["raw"]["raw"].get("statusCode").is_none());
    }
    Ok(())
}

#[tokio::test]
async fn given_whitespace_school_and_page_states_when_scoped_then_athletes_and_issues_are_retained()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut target = target(Source::AthleticNet, "11");
            target.context.state = Some(" wi ".to_owned());
            let mut result = unassigned("scoped-result");
            result.school_id = Some("school-1".to_owned());
            let page = ParsedPage {
                schools: vec![School {
                    id: "school-1".to_owned(),
                    name: "Central".to_owned(),
                    state: Some("\t wi \u{a0}".to_owned()),
                    ..School::default()
                }],
                athletes: vec![Athlete {
                    school_id: Some("school-1".to_owned()),
                    ..athlete("11")
                }],
                unassigned_results: vec![result],
                notes: vec!["published incomplete coverage".to_owned()],
                ..ParsedPage::default()
            };
            persist(&database, &target, &page).await?;
            let mut pending = records::target(Source::AthleticNet, "pending-review");
            pending.context.state = Some(" wi ".to_owned());
            database.enqueue(&[pending.clone()], false).await?;
            let (athletes, issues) = database
                .export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            assert_eq!(athletes.len(), 1);
            assert_eq!(
                athletes.first().context("scoped athlete")?.name,
                "Alex Morgan"
            );
            assert_eq!(issues.len(), 3);
            assert!(
                issues
                    .iter()
                    .any(|issue| issue.url == pending.url && issue.detail == "page pending")
            );
            assert!(issues.iter().any(|issue| issue.url == target.url
                && issue.detail == "page note: published incomplete coverage"));
            assert!(
                issues.iter().any(
                    |issue| issue.detail == "unassigned result | 100 Meters | 12.34 | recorded"
                )
            );
            let (excluded, issues) = database
                .export_snapshot(&["OH".to_owned()], &[2027])
                .await?;
            assert!(excluded.is_empty());
            assert!(issues.is_empty());
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_school_scoped_issues_when_page_scope_differs_then_represented_school_is_retained()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut target = target(Source::AthleticNet, "school-scoped-review");
            target.context.state = Some("OH".to_owned());
            let mut result = unassigned("school-scoped-result");
            result.school_id = Some("school-1".to_owned());
            persist(
                &database,
                &target,
                &ParsedPage {
                    schools: vec![School {
                        id: "school-1".to_owned(),
                        name: "Central".to_owned(),
                        state: Some(" wi ".to_owned()),
                        ..School::default()
                    }],
                    unassigned_results: vec![result],
                    notes: vec!["Ohio page".to_owned()],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let mut pending = records::target(Source::AthleticNet, "school-scoped-pending");
            pending.context.school_id = Some("school-1".to_owned());
            database.enqueue(&[pending.clone()], false).await?;
            let (_, issues) = database.export_snapshot(&["WI".to_owned()], &[]).await?;
            assert_eq!(issues.len(), 2);
            assert!(
                issues
                    .iter()
                    .any(|issue| issue.url == pending.url && issue.detail == "page pending")
            );
            assert!(issues.iter().any(|issue| issue.url == target.url
                && issue.detail == "unassigned result | 100 Meters | 12.34 | recorded"));
            assert!(
                !issues
                    .iter()
                    .any(|issue| issue.detail.contains("Ohio page"))
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_acquisition_state_when_factual_state_differs_then_page_issues_use_acquisition_scope()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut target = target(Source::AthleticNet, "acquisition-review");
            target.context.state = Some("OH".to_owned());
            target.context.acquisition_state = Some(" wi ".to_owned());
            persist(
                &database,
                &target,
                &ParsedPage {
                    notes: vec!["acquired from Wisconsin coverage".to_owned()],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let (athletes, issues) = database.export_snapshot(&["WI".to_owned()], &[]).await?;
            assert!(athletes.is_empty());
            assert_eq!(issues.len(), 1);
            assert_eq!(issues.first().context("acquisition issue")?.url, target.url);
            assert_eq!(
                issues.first().context("acquisition issue")?.detail,
                "page note: acquired from Wisconsin coverage"
            );
            let (_, excluded) = database.export_snapshot(&["OH".to_owned()], &[]).await?;
            assert!(excluded.is_empty());
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn school_profile(
    database: &Database,
    source: Source,
    id: &str,
    state: &str,
    links: Vec<String>,
) -> Result<()> {
    let page = ParsedPage {
        schools: vec![School {
            id: "central".to_owned(),
            name: "Central High School".to_owned(),
            state: Some(state.to_owned()),
            ..School::default()
        }],
        athletes: vec![Athlete {
            state: None,
            school_id: Some("central".to_owned()),
            same_as: links,
            ..athlete(id)
        }],
        performances: vec![track_scrapper_v2::model::Performance {
            athlete_id: id.to_owned(),
            ..performance("12.34")
        }],
        ..ParsedPage::default()
    };
    persist(database, &target(source, id), &page).await
}

async fn assert_school_state_conflict(pool: &sqlx::PgPool) -> Result<()> {
    let persisted: Vec<(String, Option<String>, Option<String>, String, bool)> = sqlx::query_as(
        "SELECT p.source,p.state,a.state,s.state,a.needs_review FROM athlete_sources p \
         JOIN athletes a ON a.id=p.athlete_id JOIN schools s ON s.id=p.school_id ORDER BY p.source",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(
        persisted,
        vec![
            ("athleticnet".to_owned(), None, None, "WI".to_owned(), true),
            ("milesplit".to_owned(), None, None, "OH".to_owned(), true),
        ]
    );
    let counts: (i64, i64) =
        sqlx::query_as("SELECT count(*),count(DISTINCT athlete_id) FROM athlete_sources")
            .fetch_one(pool)
            .await?;
    assert_eq!(counts, (2, 2));
    assert_eq!(result_count(pool).await?, 2);
    let incorrect_ownership: i64 = sqlx::query_scalar("SELECT count(*) FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id WHERE r.athlete_id<>p.athlete_id")
        .fetch_one(pool).await?;
    assert_eq!(incorrect_ownership, 0);
    Ok(())
}

#[tokio::test]
async fn given_known_school_states_when_unknown_state_athletes_link_then_conflict_without_guesses()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            school_profile(&database, Source::AthleticNet, "11", "WI", Vec::new()).await?;
            school_profile(
                &database,
                Source::MileSplit,
                "21",
                "OH",
                vec!["https://www.athletic.net/athlete/11".to_owned()],
            )
            .await?;
            school_profile(
                &database,
                Source::MileSplit,
                "21",
                "OH",
                vec!["https://www.athletic.net/athlete/11".to_owned()],
            )
            .await?;
            assert_school_state_conflict(&test.pool).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_reserved_link_when_later_same_name_school_has_different_state_then_identity_separates()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            school_profile(
                &database,
                Source::AthleticNet,
                "11",
                "WI",
                vec!["https://www.milesplit.com/athletes/21".to_owned()],
            )
            .await?;
            let merged: i64 =
                sqlx::query_scalar("SELECT count(DISTINCT athlete_id) FROM athlete_sources")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(merged, 1);
            school_profile(&database, Source::MileSplit, "21", "OH", Vec::new()).await?;
            school_profile(
                &database,
                Source::MileSplit,
                "21",
                "OH",
                vec!["https://www.athletic.net/athlete/11".to_owned()],
            )
            .await?;
            assert_school_state_conflict(&test.pool).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_unknown_school_state_when_linked_then_missing_state_is_not_a_conflict_or_inferred_fact()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            school_profile(
                &database,
                Source::AthleticNet,
                "11",
                "WI",
                vec!["https://www.milesplit.com/athletes/21".to_owned()],
            )
            .await?;
            school_profile(&database, Source::MileSplit, "21", "", Vec::new()).await?;
            let facts: (i64, i64, bool, bool) = sqlx::query_as(
                "SELECT count(*), count(DISTINCT a.id), bool_or(a.needs_review), \
             bool_or(p.state IS NOT NULL OR a.state IS NOT NULL) \
             FROM athlete_sources p JOIN athletes a ON a.id=p.athlete_id",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(facts, (2, 1, false, false));
            let states: Vec<String> =
                sqlx::query_scalar("SELECT state FROM schools ORDER BY source")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(states, vec!["WI".to_owned(), String::new()]);
            assert_eq!(result_count(&test.pool).await?, 2);
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn evidence_values(pool: &sqlx::PgPool) -> Result<(Value, Value)> {
    let assigned = stored(pool, "race-1").await?;
    let unassigned = stored(pool, "evidence-review").await?;
    Ok((
        assigned["raw"]["source_values"].clone(),
        unassigned["raw"]["raw"]["source_values"].clone(),
    ))
}

#[tokio::test]
async fn given_rich_level_evidence_when_sparse_metadata_then_known_level_survives_until_corrected()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = target(Source::MileSplit, "11");
        let rich = json!({"level": "high_school", "specification": "39in", "kind": "career_result", "eventType": {"id": 1, "specification": "39in"}, "context": {"season": 2026, "state": "WI"}, "row": {"html": "published row", "text": "Original row", "attributes": {"data-level": "high_school", "data-event": "100 Meters"}}});
        let mut assigned = performance("12.34");
        assigned.source_values = Some(rich.clone());
        let mut unassigned = unassigned("evidence-review");
        unassigned.raw = json!({"source_values": rich});
        let mut page = ParsedPage {
            athletes: vec![athlete("11")], performances: vec![assigned],
            unassigned_results: vec![unassigned], ..ParsedPage::default()
        };
        persist(&database, &target, &page).await?;
        let sparse = json!({"level": null, "specification": " ", "meet_url": "https://www.milesplit.com/meets/1", "eventType": {"id": null, "specification": " "}, "row": {"html": null, "text": " ", "attributes": {"data-level": null, "data-event": " "}}, "context": {"season": null, "state": " "}});
        page.performances.first_mut().context("assigned evidence")?.source_values = Some(sparse.clone());
        page.unassigned_results.first_mut().context("unassigned evidence")?.raw = json!({"source_values": sparse});
        persist(&database, &target, &page).await?;
        persist(&database, &target, &page).await?;
        let expected = json!({"level": "high_school", "specification": "39in", "kind": "career_result", "meet_url": "https://www.milesplit.com/meets/1", "eventType": {"id": 1, "specification": "39in"}, "context": {"season": 2026, "state": "WI"}, "row": {"html": "published row", "text": "Original row", "attributes": {"data-level": "high_school", "data-event": "100 Meters"}}});
        assert_eq!(evidence_values(&test.pool).await?, (expected.clone(), expected));
        let corrected = json!({"level": "college", "specification": "42in", "eventType": {"id": 2, "specification": "42in"}, "row": {"text": "Corrected row", "attributes": {"data-level": "college"}}, "context": {"season": 2025}});
        page.performances.first_mut().context("assigned evidence")?.source_values = Some(corrected.clone());
        page.unassigned_results.first_mut().context("unassigned evidence")?.raw = json!({"source_values": corrected});
        persist(&database, &target, &page).await?;
        persist(&database, &target, &page).await?;
        let expected = json!({"level": "college", "specification": "42in", "kind": "career_result", "meet_url": "https://www.milesplit.com/meets/1", "eventType": {"id": 2, "specification": "42in"}, "context": {"season": 2025, "state": "WI"}, "row": {"html": "published row", "text": "Corrected row", "attributes": {"data-level": "college", "data-event": "100 Meters"}}});
        assert_eq!(evidence_values(&test.pool).await?, (expected.clone(), expected));
        assert_eq!(result_count(&test.pool).await?, 2);
        database.close().await;
        Ok(())
    })).await
}

async fn assert_partial_correction(pool: &sqlx::PgPool, event: &str) -> Result<()> {
    for id in ["race-1", "partial-review"] {
        let result = stored(pool, id).await?;
        assert_eq!(result["event"], event);
        assert_eq!(result["mark"], "12.35");
        assert_eq!(result["status"], "recorded");
        assert_eq!(result["numeric"], 12_350_000_000_i64);
        assert_eq!(result["unit"], "ns");
        assert_eq!(result["raw"]["event"], event);
        assert_eq!(result["raw"]["mark"], "12.35");
        assert!(result["raw"].get("reported_status").is_none());
        assert!(result["raw"]["raw"].get("statusCode").is_none());
    }
    assert_eq!(result_count(pool).await?, 2);
    Ok(())
}

#[tokio::test]
async fn given_dq_rows_when_mark_only_then_event_only_corrected_then_effective_fields_are_comparable()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target(Source::AthleticNet, "11");
            let mut assigned = performance("12.34");
            assigned.reported_status = Some("DQ".to_owned());
            let mut unassigned = unassigned("partial-review");
            unassigned.raw = json!({"statusCode": "DQ"});
            let mut page = ParsedPage {
                athletes: vec![athlete("11")],
                performances: vec![assigned],
                unassigned_results: vec![unassigned],
                ..ParsedPage::default()
            };
            persist(&database, &target, &page).await?;
            let assigned = page
                .performances
                .first_mut()
                .context("assigned correction")?;
            assigned.event.clear();
            assigned.mark = "12.35".to_owned();
            assigned.reported_status = None;
            let unassigned = page
                .unassigned_results
                .first_mut()
                .context("unassigned correction")?;
            unassigned.event.clear();
            unassigned.mark = "12.35".to_owned();
            unassigned.raw = json!({});
            persist(&database, &target, &page).await?;
            persist(&database, &target, &page).await?;
            assert_partial_correction(&test.pool, "100 Meters").await?;
            let records = database.athlete_records().await?;
            let evidence = records
                .first()
                .and_then(|athlete| athlete.performances.first())
                .context("corrected evidence")?;
            assert_eq!(
                track_scrapper_v2::marks::comparable(evidence)
                    .context("corrected comparable mark")?
                    .value,
                12_350_000_000_i64
            );
            let assigned = page
                .performances
                .first_mut()
                .context("assigned event correction")?;
            assigned.event = "200 Meters".to_owned();
            assigned.mark.clear();
            let unassigned = page
                .unassigned_results
                .first_mut()
                .context("unassigned event correction")?;
            unassigned.event = "200 Meters".to_owned();
            unassigned.mark.clear();
            persist(&database, &target, &page).await?;
            persist(&database, &target, &page).await?;
            assert_partial_correction(&test.pool, "200 Meters").await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_linked_unknown_school_state_when_school_only_corrected_then_review_is_transactional()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        school_profile(&database, Source::AthleticNet, "11", "WI", vec!["https://www.milesplit.com/athletes/21".to_owned()]).await?;
        school_profile(&database, Source::MileSplit, "21", "", Vec::new()).await?;
        let school = School { id: "central".to_owned(), name: "Central High School".to_owned(), state: Some("OH".to_owned()), ..School::default() };
        let target = target(Source::MileSplit, "school-only-review");
        let failing = ParsedPage {
            schools: vec![school.clone()],
            performances: vec![track_scrapper_v2::model::Performance { athlete_id: "unobserved".to_owned(), ..performance("12.34") }],
            ..ParsedPage::default()
        };
        let error = persist(&database, &target, &failing).await.err().context("invalid athlete must fail transaction")?;
        assert!(format!("{error:#}").contains("unknown milesplit athlete ID unobserved"));
        let before: (String, bool) = sqlx::query_as("SELECT s.state,a.needs_review FROM schools s JOIN athlete_sources p ON p.school_id=s.id JOIN athletes a ON a.id=p.athlete_id WHERE s.source='milesplit'").fetch_one(&test.pool).await?;
        assert_eq!(before, (String::new(), false));
        let page = ParsedPage { schools: vec![school], ..ParsedPage::default() };
        persist(&database, &target, &page).await?;
        persist(&database, &target, &page).await?;
        let states: Vec<(String, String, Option<String>, Option<String>, bool)> = sqlx::query_as(
            "SELECT p.source,s.state,p.state,a.state,a.needs_review FROM athlete_sources p JOIN schools s ON s.id=p.school_id JOIN athletes a ON a.id=p.athlete_id ORDER BY p.source",
        ).fetch_all(&test.pool).await?;
        assert_eq!(states, vec![("athleticnet".to_owned(), "WI".to_owned(), None, None, true), ("milesplit".to_owned(), "OH".to_owned(), None, None, true)]);
        let identities: (i64, i64) = sqlx::query_as("SELECT count(*),count(DISTINCT athlete_id) FROM athlete_sources").fetch_one(&test.pool).await?;
        assert_eq!(identities, (2, 1));
        assert_eq!(result_count(&test.pool).await?, 2);
        database.close().await;
        Ok(())
    })).await
}

fn legacy_identity(html: &str) -> Result<(String, String)> {
    use sha2::{Digest, Sha256};
    let id = format!("profile-history:11:{:x}", Sha256::digest(html.as_bytes()));
    let key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(Source::MileSplit, "result", &id))?)
    );
    Ok((id, key))
}

async fn seed_ambiguous_native_history(pool: &sqlx::PgPool) -> Result<()> {
    let (row, html): (i64, String) =
        sqlx::query_as("SELECT id,raw#>>'{source_values,row,html}' FROM results")
            .fetch_one(pool)
            .await?;
    let (id, key) = legacy_identity(&html)?;
    sqlx::query("UPDATE results SET source_result_key=$2,source_result_id=$3,raw=jsonb_set(raw,'{result_id}',to_jsonb($3::text)) #- '{source_values,identity_basis}' #- '{source_values,legacy_result_id}' WHERE id=$1")
        .bind(row).bind(key).bind(id).execute(pool).await?;
    let alternate = html.replacen("<div", "<div data-capture-revision=\"prior\"", 1);
    let (id, key) = legacy_identity(&alternate)?;
    sqlx::query(
        "INSERT INTO results(source_result_key,athlete_source_id,athlete_id,represented_school_id,source,source_result_id,event,raw_mark,sport,meet_id,meet_date,status,is_personal_record,is_relay,normalized_numeric,normalized_unit,source_url,raw) \
         SELECT $2,athlete_source_id,athlete_id,represented_school_id,source,$3,event,raw_mark,sport,meet_id,meet_date,status,is_personal_record,is_relay,normalized_numeric,normalized_unit,source_url, \
         jsonb_set(jsonb_set(raw,'{source_values,row,html}',to_jsonb($4::text)),'{result_id}',to_jsonb($3::text)) FROM results WHERE id=$1",
    ).bind(row).bind(key).bind(id).bind(alternate).execute(pool).await?;
    Ok(())
}

#[tokio::test]
async fn given_multiple_native_legacy_context_matches_when_first_capture_changes_then_ambiguity_survives()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = target(Source::MileSplit, "11");
        let html = profile_document(
            &target.url, "Alex Morgan", r#"<span class="grad-year">Class of 2027</span>"#,
            r#"<div class="record" data-season="outdoor" data-event="100 Meters" data-level="hs"><div class="seed">12.34</div>
            <div class="location"><a href="/meets/900-county">County</a></div><div class="date">2026-05-15</div></div>"#,
        );
        let page = track_scrapper_v2::sources::parse(&target, &html)?;
        assert_eq!(page.performances.len(), 1);
        persist(&database, &target, &page).await?;
        seed_ambiguous_native_history(&test.pool).await?;
        let page = track_scrapper_v2::sources::parse(&target, &html.replace("12.34", "12.35"))?;
        persist(&database, &target, &page).await?;
        persist(&database, &target, &page).await?;
        let observations: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT raw_mark,raw#>>'{source_values,identity_reconciliation}' FROM results ORDER BY raw_mark,id",
        ).fetch_all(&test.pool).await?;
        assert_eq!(observations, vec![("12.34".to_owned(), Some("ambiguous_legacy_context".to_owned())), ("12.34".to_owned(), Some("ambiguous_legacy_context".to_owned())), ("12.35".to_owned(), None)]);
        let reviewed: bool = sqlx::query_scalar("SELECT needs_review FROM athletes").fetch_one(&test.pool).await?;
        assert!(reviewed);
        assert_eq!(result_count(&test.pool).await?, 3);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_raw_revision_ambiguity_when_empty_or_other_file_then_reconfirmation_clears_only_its_marker()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target(Source::MileSplit, "11");
            let mut assigned = performance("12.34");
            assigned.source_values = Some(
                json!({"kind": "raw_result", "published_file_identity": "milesplit-file:900:1"}),
            );
            let original = ParsedPage {
                athletes: vec![athlete("11")],
                performances: vec![assigned],
                ..ParsedPage::default()
            };
            persist(&database, &target, &original).await?;
            let mut relay = unassigned("raw-relay");
            relay.is_relay = true;
            relay.event = "4x100 Relay".to_owned();
            relay.raw =
                json!({"kind": "raw_result", "published_file_identity": "milesplit-file:900:1"});
            persist(
                &database,
                &target,
                &ParsedPage {
                    unassigned_results: vec![relay],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let flagged = stored(&test.pool, "race-1").await?;
            assert_eq!(
                flagged["raw"]["source_values"]["revision_status"],
                "unverified_prior_revision"
            );
            assert_eq!(flagged["unassigned"], false);
            persist(&database, &target, &ParsedPage::default()).await?;
            assert_eq!(stored(&test.pool, "race-1").await?, flagged);
            let mut other = unassigned("other-file");
            other.raw =
                json!({"kind": "raw_result", "published_file_identity": "milesplit-file:900:2"});
            persist(
                &database,
                &target,
                &ParsedPage {
                    unassigned_results: vec![other],
                    ..ParsedPage::default()
                },
            )
            .await?;
            assert_eq!(stored(&test.pool, "race-1").await?, flagged);
            persist(&database, &target, &original).await?;
            let reconfirmed = stored(&test.pool, "race-1").await?;
            assert!(
                reconfirmed["raw"]["source_values"]
                    .get("revision_status")
                    .is_none()
            );
            assert_eq!(reconfirmed["mark"], "12.34");
            assert_eq!(reconfirmed["numeric"], 12_340_000_000_i64);
            assert_eq!(reconfirmed["unassigned"], false);
            assert_eq!(result_count(&test.pool).await?, 3);
            database.close().await;
            Ok(())
        })
    })
    .await
}

fn profile_document(url: &str, name: &str, athlete_info: &str, records: &str) -> String {
    let person = json!({
        "@context": "https://schema.org",
        "@type": "Person",
        "name": name,
        "gender": "Male",
        "url": url
    });
    format!(
        r#"<html><head><link rel="canonical" href="{url}">
    <script type="application/ld+json">{person}</script></head><body>
    <h1 id="athleteName">{name}</h1><div class="athlete-info">{athlete_info}</div>
    <div id="progression"></div><script>
    var gender = "m";
    var progressionTrackerData = {{}};
    </script><div id="stats">{records}</div></body></html>"#
    )
}

fn current_school_profile(url: &str, name: &str, known_current: bool) -> String {
    let current = if known_current {
        r#"<span class="current-school"><a href="/teams/101-current">Current Academy</a></span>"#
    } else {
        ""
    };
    let athlete_info = format!(
        r#"{current}<span class="city-state">Madison, WI</span><span class="grad-year">Class of 2027</span>"#
    );
    let records = r#"<div class="record" data-season="outdoor" data-event="100 Meters" data-level="hs"><div class="seed">12.34</div>
    <div class="school"><a href="/teams/202-historical">Historic Academy</a></div><div class="location"><a href="/meets/900-county">County</a></div><div class="date">2026-05-15</div></div>
    <div class="record" data-season="outdoor" data-event="200 Meters" data-level="hs"><div class="seed">25.00</div>
    <div class="location"><a href="/meets/901-invitational">Invitational</a></div><div class="date">2026-05-16</div></div>"#;
    profile_document(url, name, &athlete_info, records)
}

async fn seed_current_and_historical_school_claims(database: &Database) -> Result<()> {
    for (id, name, current) in [
        ("11", "Current Athlete", true),
        ("12", "Unknown Current", false),
    ] {
        let target = target(Source::MileSplit, id);
        let page = track_scrapper_v2::sources::parse(
            &target,
            &current_school_profile(&target.url, name, current),
        )?;
        assert_eq!(page.performances.len(), 2);
        persist(database, &target, &page).await?;
    }
    for (id, name, state, address, coach) in [
        (
            "101",
            "Current Academy",
            "WI",
            "100 Current Ave",
            "Current Coach",
        ),
        (
            "202",
            "Historic Academy",
            "OH",
            "200 Historical Rd",
            "Historical Coach",
        ),
    ] {
        let target = track_scrapper_v2::model::Target::get(
            Source::MileSplit,
            track_scrapper_v2::model::PageKind::Team,
            format!("https://www.milesplit.com/teams/{id}"),
            track_scrapper_v2::model::PageContext::default(),
        );
        let page = ParsedPage {
            schools: vec![School {
                id: id.to_owned(),
                name: name.to_owned(),
                state: Some(state.to_owned()),
                address: Some(address.to_owned()),
                ..School::default()
            }],
            coaches: vec![track_scrapper_v2::model::Coach {
                school_id: id.to_owned(),
                name: Some(coach.to_owned()),
                role: "Head Track Coach".to_owned(),
                email: Some(format!("coach{id}@schools.example")),
                sport: Some(Sport::Outdoor),
                review_notes: Vec::new(),
            }],
            ..ParsedPage::default()
        };
        persist(database, &target, &page).await?;
    }
    Ok(())
}

fn workbook_table(
    path: &std::path::Path,
    name: &str,
) -> Result<Vec<std::collections::BTreeMap<String, String>>> {
    use calamine::Reader;
    let mut workbook = calamine::open_workbook_auto(path)?;
    let range = workbook.worksheet_range(name)?;
    let headers = range
        .rows()
        .next()
        .context("worksheet headers")?
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Ok(range
        .rows()
        .skip(1)
        .map(|row| {
            headers
                .iter()
                .cloned()
                .zip(row.iter().map(ToString::to_string))
                .collect()
        })
        .collect())
}

fn assert_current_school_workbook(path: &std::path::Path) -> Result<()> {
    let rows = workbook_table(path, "Athletes")?;
    assert_eq!(rows.len(), 2);
    let known = rows
        .iter()
        .find(|row| row.get("name").map(String::as_str) == Some("Current Athlete"))
        .context("current athlete row")?;
    assert_eq!(
        known.get("school_names").map(String::as_str),
        Some("Current Academy")
    );
    assert_eq!(known.get("school_states").map(String::as_str), Some("WI"));
    assert_eq!(
        known.get("coach_names").map(String::as_str),
        Some("Current Coach")
    );
    let address = known
        .get("school_postal_addresses")
        .context("current postal address")?;
    assert!(address.contains("100 Current Ave"));
    assert!(!address.contains("200 Historical Rd"));
    let unknown = rows
        .iter()
        .find(|row| row.get("name").map(String::as_str) == Some("Unknown Current"))
        .context("unknown current athlete row")?;
    assert_eq!(unknown.get("school_names").map(String::as_str), Some(""));
    assert_eq!(
        unknown.get("school_postal_addresses").map(String::as_str),
        Some("")
    );
    assert_eq!(unknown.get("coach_names").map(String::as_str), Some(""));
    assert_eq!(
        unknown.get("needs_review").map(String::as_str),
        Some("true")
    );
    let observations = workbook_table(path, "Observations")?;
    assert_eq!(observations.len(), 4);
    let published = observations
        .iter()
        .map(|row| {
            serde_json::from_str::<Value>(
                row.get("published_observation")
                    .context("published observation")?,
            )
            .map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        published
            .iter()
            .filter(|row| row
                .pointer("/performance/school_id")
                .and_then(Value::as_str)
                == Some("202"))
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn given_current_and_historical_schools_when_profile_records_export_then_only_current_claims_fill_main_rows()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_current_and_historical_school_claims(&database).await?;
            let (records, issues) = database
                .export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            assert_eq!(records.len(), 2);
            let known = records
                .iter()
                .find(|row| row.name == "Current Athlete")
                .context("known current record")?;
            assert_eq!(
                known
                    .schools
                    .iter()
                    .map(|school| school.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["101"]
            );
            assert_eq!(
                known
                    .coaches
                    .iter()
                    .map(|coach| coach.name.as_deref())
                    .collect::<Vec<_>>(),
                vec![Some("Current Coach")]
            );
            assert_eq!(
                known
                    .performances
                    .iter()
                    .map(|row| row.performance.school_id.as_deref())
                    .collect::<Vec<_>>(),
                vec![Some("202"), None]
            );
            let unknown = records
                .iter()
                .find(|row| row.name == "Unknown Current")
                .context("unknown current record")?;
            assert!(unknown.schools.is_empty());
            assert!(unknown.coaches.is_empty());
            assert!(unknown.needs_review);
            assert_eq!(
                unknown
                    .performances
                    .iter()
                    .filter_map(|row| row.performance.school_id.as_deref())
                    .collect::<Vec<_>>(),
                vec!["202"]
            );
            let ohio = database
                .athlete_records_for(&["OH".to_owned()], &[2027])
                .await?;
            assert_eq!(
                ohio.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
                vec!["Unknown Current"]
            );
            let (ohio_snapshot, _) = database
                .export_snapshot(&["OH".to_owned()], &[2027])
                .await?;
            assert_eq!(
                ohio_snapshot
                    .iter()
                    .map(|row| row.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["Unknown Current"]
            );
            let historical: (String, String) = sqlx::query_as(
                "SELECT state,address FROM schools WHERE source='milesplit' AND source_id='202'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(
                historical,
                ("OH".to_owned(), "200 Historical Rd".to_owned())
            );
            assert_eq!(result_count(&test.pool).await?, 4);
            let path = test.directory().join("current-schools.xlsx");
            let options = track_scrapper_v2::export::ExportOptions {
                states: vec!["WI".to_owned()],
                graduation_years: vec![2027],
                rows_per_file: 100,
                per_athlete_sheets: false,
            };
            track_scrapper_v2::export::write_spreadsheets_with_issues(
                &path, &records, &issues, &options,
            )?;
            assert_current_school_workbook(&path)?;
            database.close().await;
            Ok(())
        })
    })
    .await
}
