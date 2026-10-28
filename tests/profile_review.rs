mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Target},
    sources,
};

const URL: &str = "https://oh.milesplit.com/athletes/15005189-brice-fuller";
const REAL_PROFILE: &str = include_str!("../fixtures/milesplit/athlete-oh-15005189-current.html");

fn target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        URL.to_owned(),
        PageContext::default(),
    )
}

fn profile(schema: &str, progression: &str, records: &str) -> String {
    format!(
        r#"<html><head><title>Brice Fuller - MileSplit</title><link rel="canonical" href="{URL}">
        <script type="application/ld+json">{{"@context":"https://schema.org","@type":"Person","name":"Brice Fuller","image":"https://assets.sp.milesplit.com/athlete_photos/15005189?v=1","affiliation":"","gender":"{schema}","url":"{URL}"}}</script>
        </head><body><h1 id="athleteName">Brice Fuller</h1>
        <div class="athlete-info"><span class="current-school"><a href="/teams/9573-fairless">Fairless</a></span>
        <span class="city-state">Brewster, OH</span><span class="grad-year">Class of 2028</span></div>
        <nav class="profile-navigation"><a href="{URL}/progression">Progression</a></nav>
        <div id="progression"></div><script>
        var gender = "{progression}";
        var progressionTrackerData = {{}};
        </script>{records}</body></html>"#
    )
}

fn school_records() -> &'static str {
    r#"<table id="prTable">
    <tr data-pr-mode="indoor" data-pr-level="hs"><td>800m</td><td>2:11.88</td><td></td><td></td><td></td></tr>
    <tr data-pr-mode="outdoor" data-pr-level="ms"><td>1600m</td><td>4:40.00</td><td><a href="/teams/111-old-school">Old School</a></td><td></td><td></td></tr>
    </table><div id="stats">
    <div class="record" data-season="outdoor" data-event="800m" data-level="hs"><div class="seed">2:12.00</div><div class="school"><a href="/teams/111-old-school">Old School</a></div><div class="location"><a href="/meets/900-spring-meet">Spring Meet</a></div><div class="date">2025-05-01</div></div>
    <div class="record" data-season="outdoor" data-event="800m"><div class="seed">2:13.00</div><div class="location"><a href="/meets/901-invitational">Invitational</a></div><div class="date">2025-05-02</div></div>
    </div>"#
}

async fn seed_native_legacy_history_ids(pool: &sqlx::PgPool) -> Result<()> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id,raw#>>'{source_values,row,html}' FROM results WHERE raw#>>'{source_values,kind}'='career_result'",
    ).fetch_all(pool).await?;
    for (id, html) in rows {
        let legacy_id = format!(
            "profile-history:15005189:{:x}",
            Sha256::digest(html.as_bytes())
        );
        let key = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                Source::MileSplit,
                "result",
                &legacy_id
            ))?)
        );
        sqlx::query("UPDATE results SET source_result_key=$2,source_result_id=$3,raw=jsonb_set(raw,'{result_id}',to_jsonb($3::text)) #- '{source_values,identity_basis}' #- '{source_values,school_attribution}' #- '{source_values,school_evidence}' WHERE id=$1")
            .bind(id).bind(key).bind(legacy_id).execute(pool).await?;
    }
    Ok(())
}

#[test]
fn given_current_school_a_when_history_parsed_then_only_row_supported_school_b_is_represented()
-> Result<()> {
    let page = sources::parse(&target(), &profile("Male", "m", school_records()))?;
    assert_eq!(
        page.athletes
            .first()
            .context("athlete")?
            .school_id
            .as_deref(),
        Some("9573")
    );
    let observed = page
        .performances
        .iter()
        .map(|row| {
            (
                row.mark.as_str(),
                row.school_id.as_deref(),
                row.source_values
                    .as_ref()
                    .and_then(|v| v.get("level"))
                    .cloned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            ("2:11.88", None, Some(json!("hs"))),
            ("4:40.00", Some("111"), Some(json!("ms"))),
            ("2:12.00", Some("111"), Some(json!("hs"))),
            ("2:13.00", None, Some(Value::Null)),
        ]
    );
    let old_school = page
        .schools
        .iter()
        .find(|school| school.id == "111")
        .context("represented school")?;
    assert_eq!(old_school.name, "Old School");
    assert_eq!(old_school.city, None);
    let evidence = page
        .performances
        .iter()
        .find(|row| row.mark == "2:12.00")
        .and_then(|row| row.source_values.as_ref())
        .context("history evidence")?;
    assert!(
        evidence["row"]["html"]
            .as_str()
            .context("row HTML")?
            .contains("/teams/111-old-school")
    );
    assert_eq!(evidence["school_attribution"], "row_link");
    assert_eq!(
        evidence["school_evidence"]["attributes"]["href"],
        "/teams/111-old-school"
    );
    assert_eq!(
        page.performances
            .iter()
            .find(|row| row.mark == "2:13.00")
            .and_then(|row| row.source_values.as_ref())
            .context("unknown school evidence")?["school_attribution"],
        "not_published"
    );
    Ok(())
}

#[test]
fn given_conflicting_real_profile_when_first_parsed_then_both_claims_survive_and_review_is_immediate()
-> Result<()> {
    let page = sources::parse(&target(), REAL_PROFILE)?;
    let athlete = page.athletes.first().context("athlete")?;
    assert!(athlete.needs_review);
    assert_eq!(athlete.gender.as_deref(), Some("male"));
    let evidence = athlete.source_values.as_ref().context("profile evidence")?;
    assert_eq!(
        evidence["pages"][URL]["gender_claims"],
        json!({"schema_person": "female", "progression": "male"})
    );
    assert_eq!(evidence["pages"][URL]["json_ld"][0]["gender"], "Female");
    assert_eq!(
        evidence["pages"][URL]["progression"][0]["800m"]["gender"],
        "m"
    );
    let indoor = page
        .performances
        .iter()
        .filter(|row| row.event == "800m" && row.sport == track_scrapper_v2::model::Sport::Indoor)
        .map(|row| {
            (
                row.source_values.as_ref().and_then(|v| v["level"].as_str()),
                row.mark.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        indoor,
        vec![
            (Some("all"), "2:10.99"),
            (Some("hs"), "2:11.88"),
            (Some("ms"), "2:10.99")
        ]
    );
    assert!(page.performances.iter().all(|row| row.school_id.is_none()));
    Ok(())
}

#[test]
fn given_absent_category_claims_when_profile_parsed_then_no_category_is_invented() -> Result<()> {
    let page = sources::parse(&target(), &profile("Unknown", "unknown", school_records()))?;
    let athlete = page.athletes.first().context("athlete")?;
    assert_eq!(athlete.gender, None);
    assert!(!athlete.needs_review);
    assert!(page.performances.iter().all(|row| row.gender.is_none()));
    Ok(())
}

#[test]
fn given_history_school_without_current_school_when_parsed_then_it_is_not_current_affiliation()
-> Result<()> {
    let body = profile("Male", "m", school_records()).replace(
        r#"<span class="current-school"><a href="/teams/9573-fairless">Fairless</a></span>"#,
        "",
    );
    let page = sources::parse(&target(), &body)?;
    assert_eq!(page.athletes.first().context("athlete")?.school_id, None);
    assert_eq!(page.schools.len(), 1);
    assert_eq!(page.schools.first().context("old school")?.id, "111");
    assert!(page.next.iter().all(|target| target.kind != PageKind::Team));
    assert_eq!(
        page.performances
            .iter()
            .find(|row| row.mark == "2:12.00")
            .and_then(|row| row.school_id.as_deref()),
        Some("111")
    );
    Ok(())
}

#[tokio::test]
async fn given_first_profile_capture_when_persisted_then_conflict_and_historical_schools_survive()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = target();
        let body = profile("Female", "m", school_records());
        let page = sources::parse(&target, &body)?;
        let capture = Capture { url: URL.to_owned(), status: 200, content_type: "text/html".to_owned(), body: body.into_bytes() };
        database.persist(None, &target, &capture, &page).await?;
        let (review, current_school): (bool, String) = sqlx::query_as("SELECT a.needs_review,s.source_id FROM athletes a JOIN schools s ON s.id=a.school_id").fetch_one(&test.pool).await?;
        assert_eq!((review, current_school.as_str()), (true, "9573"));
        let evidence: Value = sqlx::query_scalar("SELECT details->'source_values' FROM athlete_sources WHERE source='milesplit'").fetch_one(&test.pool).await?;
        assert_eq!(evidence["pages"][URL]["gender_claims"], json!({"schema_person": "female", "progression": "male"}));
        let schools: Vec<(String, Option<String>)> = sqlx::query_as("SELECT r.raw_mark,s.source_id FROM results r LEFT JOIN schools s ON s.id=r.represented_school_id ORDER BY r.raw_mark").fetch_all(&test.pool).await?;
        assert_eq!(schools, vec![("2:11.88".to_owned(), None), ("2:12.00".to_owned(), Some("111".to_owned())), ("2:13.00".to_owned(), None), ("4:40.00".to_owned(), Some("111".to_owned()))]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_legacy_inferred_school_with_incidental_team_text_when_capture_reprocessed_then_attribution_is_corrected()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = target();
        let records = school_records().replace("2:13.00</div>", "2:13.00</div><span>Directory /teams/9573 is not represented-school evidence</span>");
        let body = profile("Male", "m", &records);
        let capture = Capture { url: URL.to_owned(), status: 200, content_type: "text/html".to_owned(), body: body.as_bytes().to_vec() };
        let mut legacy = sources::parse(&target, &body)?;
        for row in &mut legacy.performances {
            row.school_id = Some("9573".to_owned());
            let values = row.source_values.as_mut().and_then(Value::as_object_mut).context("legacy evidence")?;
            assert!(values.remove("school_attribution").is_some());
            assert!(values.remove("school_evidence").is_some());
        }
        database.persist(None, &target, &capture, &legacy).await?;
        seed_native_legacy_history_ids(&test.pool).await?;
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM results r JOIN schools s ON s.id=r.represented_school_id WHERE s.source_id='9573'").fetch_one(&test.pool).await?;
        assert_eq!(before, 4);
        let corrected = sources::parse(&target, &body)?;
        database.persist(None, &target, &capture, &corrected).await?;
        let schools: Vec<(String, Option<String>)> = sqlx::query_as("SELECT r.raw_mark,s.source_id FROM results r LEFT JOIN schools s ON s.id=r.represented_school_id ORDER BY r.raw_mark").fetch_all(&test.pool).await?;
        assert_eq!(schools, vec![("2:11.88".to_owned(), None), ("2:12.00".to_owned(), Some("111".to_owned())), ("2:13.00".to_owned(), None), ("4:40.00".to_owned(), Some("111".to_owned()))]);
        let unsupported: i64 = sqlx::query_scalar("SELECT count(*) FROM results WHERE raw->>'school_id'='9573'").fetch_one(&test.pool).await?;
        assert_eq!(unsupported, 0);
        let current: String = sqlx::query_scalar("SELECT s.source_id FROM athletes a JOIN schools s ON s.id=a.school_id").fetch_one(&test.pool).await?;
        assert_eq!(current, "9573");
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_published_old_school_when_profile_row_becomes_sparse_then_published_attribution_survives()
-> Result<()> {
    for legacy_format in [false, true] {
        support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = target();
        let rich = profile("Male", "m", school_records());
        let sparse = rich.replace(r#"<a href="/teams/111-old-school">Old School</a>"#, "");
        for body in [&rich, &sparse, &sparse] {
            let capture = Capture { url: URL.to_owned(), status: 200, content_type: "text/html".to_owned(), body: body.as_bytes().to_vec() };
            let mut page = sources::parse(&target, body)?;
            if body == &rich && legacy_format {
                for row in &mut page.performances {
                    let values = row.source_values.as_mut().and_then(Value::as_object_mut).context("legacy evidence")?;
                    assert!(values.remove("school_attribution").is_some());
                    assert!(values.remove("school_evidence").is_some());
                }
            }
            database.persist(None, &target, &capture, &page).await?;
            if body == &rich && legacy_format {
                seed_native_legacy_history_ids(&test.pool).await?;
            }
        }
        let school: String = sqlx::query_scalar("SELECT s.source_id FROM results r JOIN schools s ON s.id=r.represented_school_id WHERE r.source_result_id='pr:15005189:outdoor:ms:1600m'").fetch_one(&test.pool).await?;
        assert_eq!(school, "111");
        let current: String = sqlx::query_scalar("SELECT s.source_id FROM athletes a JOIN schools s ON s.id=a.school_id").fetch_one(&test.pool).await?;
        assert_eq!(current, "9573");
        let accepted: Value = sqlx::query_scalar("SELECT raw->'source_values' FROM results WHERE source_result_id='pr:15005189:outdoor:ms:1600m'").fetch_one(&test.pool).await?;
        assert_eq!(accepted["school_attribution"], "row_link");
        assert!(accepted["school_evidence"]["html"].as_str().context("accepted school evidence")?.contains("/teams/111-old-school"));
        let careers: Vec<(String, Option<String>)> = sqlx::query_as("SELECT r.raw_mark,s.source_id FROM results r LEFT JOIN schools s ON s.id=r.represented_school_id WHERE NOT r.is_personal_record ORDER BY r.raw_mark").fetch_all(&test.pool).await?;
        assert_eq!(careers, vec![("2:12.00".to_owned(), Some("111".to_owned())), ("2:13.00".to_owned(), None)]);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(count, 4);
        database.close().await;
        Ok(())
    })).await?;
    }
    Ok(())
}

#[test]
fn given_career_rows_when_school_and_mark_change_then_published_context_identity_is_stable()
-> Result<()> {
    let rich = profile("Male", "m", school_records());
    let corrected = rich
        .replace("/teams/111-old-school", "/teams/222-correct-school")
        .replace("Old School", "Correct School")
        .replace("2:12.00", "2:11.95");
    let original = sources::parse(&target(), &rich)?;
    let corrected = sources::parse(&target(), &corrected)?;
    let before = original
        .performances
        .iter()
        .find(|row| row.mark == "2:12.00")
        .context("original career result")?;
    let after = corrected
        .performances
        .iter()
        .find(|row| row.mark == "2:11.95")
        .context("corrected career result")?;
    assert_eq!(before.result_id, after.result_id);
    assert_eq!(after.school_id.as_deref(), Some("222"));
    assert_eq!(
        after.source_values.as_ref().context("identity evidence")?["identity_basis"],
        "unique_context"
    );
    Ok(())
}

#[test]
fn given_duplicate_career_context_when_parsed_then_rows_remain_separate_and_flagged() -> Result<()>
{
    let row = r#"<div class="record" data-season="outdoor" data-event="800m" data-level="hs"><div class="seed">2:12.00</div><div class="location"><a href="/meets/900-spring-meet">Spring Meet</a></div><div class="date">2025-05-01</div></div>"#;
    let body = profile("Male", "m", &format!("<div id=\"stats\">{row}{row}</div>"));
    let page = sources::parse(&target(), &body)?;
    assert_eq!(page.performances.len(), 2);
    assert!(page.athletes.first().context("athlete")?.needs_review);
    let ids = page
        .performances
        .iter()
        .filter_map(|row| row.result_id.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 2);
    assert!(page.performances.iter().all(|row| {
        row.source_values
            .as_ref()
            .is_some_and(|values| values["identity_basis"] == "ambiguous")
    }));
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("Ambiguous published career result identity"))
    );
    Ok(())
}

#[tokio::test]
async fn given_career_result_when_source_corrects_mark_and_school_then_one_result_is_updated()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = target();
        let rich = profile("Male", "m", school_records());
        let corrected = rich.replace("/teams/111-old-school", "/teams/222-correct-school").replace("Old School", "Correct School").replace("2:12.00", "2:11.95");
        for body in [&rich, &corrected, &corrected] {
            let capture = Capture { url: URL.to_owned(), status: 200, content_type: "text/html".to_owned(), body: body.as_bytes().to_vec() };
            let page = sources::parse(&target, body)?;
            database.persist(None, &target, &capture, &page).await?;
            if body == &rich {
                seed_native_legacy_history_ids(&test.pool).await?;
            }
        }
        let careers: Vec<(String, Option<String>)> = sqlx::query_as("SELECT r.raw_mark,s.source_id FROM results r LEFT JOIN schools s ON s.id=r.represented_school_id WHERE NOT r.is_personal_record ORDER BY r.raw_mark").fetch_all(&test.pool).await?;
        assert_eq!(careers, vec![("2:11.95".to_owned(), Some("222".to_owned())), ("2:13.00".to_owned(), None)]);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(count, 4);
        let current: String = sqlx::query_scalar("SELECT s.source_id FROM athletes a JOIN schools s ON s.id=a.school_id").fetch_one(&test.pool).await?;
        assert_eq!(current, "9573");
        database.close().await;
        Ok(())
    })).await
}

#[test]
fn given_published_result_ids_when_context_matches_then_distinct_source_results_remain_distinct()
-> Result<()> {
    let row = r#"<div class="record" data-result-id="400" data-season="outdoor" data-event="800m"><div class="seed">2:12.00</div></div>"#;
    let other = row.replace("400", "401");
    let body = profile(
        "Male",
        "m",
        &format!("<div id=\"stats\">{row}{other}</div>"),
    );
    let original = sources::parse(&target(), &body)?;
    let corrected = sources::parse(&target(), &body.replace("2:12.00", "2:11.95"))?;
    let before = original
        .performances
        .iter()
        .map(|row| row.result_id.as_deref())
        .collect::<Vec<_>>();
    let after = corrected
        .performances
        .iter()
        .map(|row| row.result_id.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        before,
        vec![Some("profile-result:400"), Some("profile-result:401")]
    );
    assert_eq!(before, after);
    assert!(!corrected.athletes.first().context("athlete")?.needs_review);
    Ok(())
}

#[tokio::test]
async fn given_unrelated_legacy_html_when_profile_replayed_then_it_cannot_prove_school_attribution()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = target();
        let body = profile("Male", "m", school_records());
        let capture = Capture { url: URL.to_owned(), status: 200, content_type: "text/html".to_owned(), body: body.as_bytes().to_vec() };
        let mut legacy = sources::parse(&target, &body)?;
        let row = legacy.performances.iter_mut().find(|row| row.mark == "2:13.00").context("legacy unknown result")?;
        row.school_id = Some("9573".to_owned());
        let values = row.source_values.as_mut().and_then(Value::as_object_mut).context("legacy evidence")?;
        assert!(values.remove("school_attribution").is_some());
        assert!(values.remove("school_evidence").is_some());
        *values.get_mut("row").and_then(|row| row.get_mut("html")).context("legacy row HTML")? =
            json!(r#"<article><a href="/teams/9573-fairless">Unrelated directory entry</a></article>"#);
        database.persist(None, &target, &capture, &legacy).await?;
        let corrected = sources::parse(&target, &body)?;
        database.persist(None, &target, &capture, &corrected).await?;
        database.persist(None, &target, &capture, &corrected).await?;
        let (school, evidence): (String, Value) = sqlx::query_as("SELECT s.source_id,r.raw->'source_values' FROM results r JOIN schools s ON s.id=r.represented_school_id WHERE r.raw_mark='2:13.00'").fetch_one(&test.pool).await?;
        assert_eq!(school, "9573");
        assert_eq!(evidence["school_attribution"], "unverified_prior");
        assert!(evidence.get("school_evidence").is_none_or(Value::is_null));
        database.close().await;
        Ok(())
    })).await
}
