mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::{
    database::Database,
    export::{ExportLayout, ExportOptions, StagedExport, prepare_chunk, verify_chunks},
    model::{AthleteRecord, School},
};

const STATES: [&str; 5] = ["MI", "MN", "IA", "IL", "CO"];

fn options(state: &str) -> ExportOptions {
    ExportOptions {
        states: vec![state.into()],
        graduation_years: vec![2028, 2029],
        rows_per_file: 1000,
        per_athlete_sheets: false,
    }
}

fn record(id: i64, state: Option<&str>, year: Option<i32>) -> AthleteRecord {
    AthleteRecord {
        id,
        name: format!("Synthetic Athlete {id}"),
        gender: None,
        grade: None,
        graduation_year: year,
        needs_review: true,
        schools: vec![School {
            id: format!("school-{id}"),
            name: format!("Synthetic School {id}"),
            state: state.map(str::to_owned),
            ..Default::default()
        }],
        coaches: vec![],
        profiles: vec![format!("https://mi.milesplit.com/athletes/{id}")],
        performances: vec![],
        profile_details: vec![],
    }
}

#[test]
fn prepared_state_exports_require_published_matching_state_for_both_selected_cohorts() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let mut records = vec![
        record(1, None, Some(2028)),
        record(2, Some("\u{a0} \t"), Some(2029)),
        record(3, Some("WI"), Some(2028)),
        record(4, Some("MI"), None),
        record(5, Some("MI"), Some(2027)),
    ];
    let mut no_school = record(6, Some("MI"), Some(2028));
    no_school.schools.clear();
    records.push(no_school);
    for (index, state) in STATES.iter().enumerate() {
        let id = i64::try_from(index)?.checked_mul(2).context("fixture id")? + 10;
        records.push(record(
            id,
            Some(&format!(" {} ", state.to_ascii_lowercase())),
            Some(2028),
        ));
        records.push(record(id + 1, Some(state), Some(2029)));
    }
    let before = serde_json::to_value(&records)?;
    for (index, state) in STATES.iter().enumerate() {
        let first = i64::try_from(index)?
            .checked_mul(2)
            .context("expected id")?
            + 10;
        let direct_output = directory.path().join(format!("{state}-direct.xlsx"));
        let mut direct_export = StagedExport::new(&direct_output, &options(state))?
            .with_layout(ExportLayout::SingleSheet);
        direct_export.stage_part(&records, &[])?;
        direct_export.finish()?;
        let mut direct_workbook = open_workbook_auto(&direct_output)?;
        let direct_sheet = direct_workbook.worksheet_range("Athletes")?;
        assert_eq!(direct_sheet.height(), 3, "direct {state}");
        let chunk = prepare_chunk(&records, &options(state))?;
        assert_eq!(chunk.athlete_ids, vec![first, first + 1], "{state}");
        let output = directory.path().join(format!("{state}-2028-2029.xlsx"));
        let mut export =
            StagedExport::new(&output, &options(state))?.with_layout(ExportLayout::SingleSheet);
        export.stage_prepared(chunk.clone())?;
        assert_eq!(export.finish()?, vec![output.clone()]);
        assert_eq!(verify_chunks(&output, &[chunk])?.0, 2);
        let mut workbook = open_workbook_auto(&output)?;
        assert_eq!(workbook.sheet_names(), &["Athletes"]);
        let sheet = workbook.worksheet_range("Athletes")?;
        assert_eq!(sheet.height(), 3);
        assert_eq!(sheet, direct_sheet, "prepared/direct {state} cell parity");
        let names: Vec<_> = sheet.rows().skip(1).map(|row| row[0].to_string()).collect();
        assert_eq!(
            names,
            vec![
                format!("Synthetic Athlete {first}"),
                format!("Synthetic Athlete {}", first + 1)
            ]
        );
    }
    assert_eq!(serde_json::to_value(&records)?, before);
    Ok(())
}

async fn seed(
    test: &support::Scenario,
    id: &str,
    state: Option<&str>,
    year: i32,
    unverified: bool,
) -> Result<i64> {
    let school: i64 = sqlx::query_scalar(
        "INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit',$1,$2,$3,$4) RETURNING id",
    ).bind(id).bind(format!("Synthetic School {id}")).bind(state)
        .bind(format!("https://mi.milesplit.com/teams/{id}"))
        .fetch_one(&test.pool).await?;
    let athlete: i64 = sqlx::query_scalar(
        "INSERT INTO athletes(name,graduation_year,needs_review) VALUES($1,$2,TRUE) RETURNING id",
    )
    .bind(format!("Synthetic Athlete {id}"))
    .bind(year)
    .fetch_one(&test.pool)
    .await?;
    let details = if unverified {
        serde_json::json!({"source_values":{"current_affiliation":{"school_state_unverified":true,"unverified_school_state":state}}})
    } else {
        serde_json::json!({})
    };
    sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,school_id,graduation_year,details) VALUES($1,'milesplit',$2,$3,$4,$5)")
        .bind(athlete).bind(id).bind(school).bind(year).bind(details).execute(&test.pool).await?;
    Ok(athlete)
}

async fn snapshot(test: &support::Scenario) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('athletes',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM athletes a),'profiles',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM athlete_sources p),'schools',(SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM schools s))")
        .fetch_one(&test.pool).await?)
}

#[tokio::test]
async fn database_unverified_school_states_do_not_become_confirmed_prepared_membership()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let selected = seed(test, "100", Some("MI"), 2028, false).await?;
            let unknown = seed(test, "101", None, 2028, false).await?;
            let unverified = seed(test, "102", Some("MI"), 2029, true).await?;
            let before = snapshot(test).await?;
            let (records, _) = database
                .export_snapshot(&["MI".into()], &[2028, 2029])
                .await?;
            assert_eq!(records.len(), 3);
            for id in [unknown, unverified] {
                let record = records
                    .iter()
                    .find(|record| record.id == id)
                    .context("retained source record")?;
                assert!(record.schools.iter().all(|school| school.state.is_none()));
                assert!(record.needs_review);
            }
            let chunk = prepare_chunk(&records, &options("MI"))?;
            assert_eq!(chunk.athlete_ids, vec![selected]);
            let output = test.directory().join("mi-confirmed.xlsx");
            let mut export =
                StagedExport::new(&output, &options("MI"))?.with_layout(ExportLayout::SingleSheet);
            export.stage_prepared(chunk.clone())?;
            export.finish()?;
            assert_eq!(verify_chunks(&output, &[chunk])?.0, 1);
            assert_eq!(snapshot(test).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}
