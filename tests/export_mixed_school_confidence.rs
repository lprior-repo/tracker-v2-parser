mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use sqlx::PgPool;
use track_scrapper_v2::{
    database::Database,
    export::{ExportLayout, ExportOptions},
};

async fn seed(
    pool: &PgPool,
    mile_split_unverified: bool,
    athletic_net_unverified: bool,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "WITH school AS (
            INSERT INTO schools(source,source_id,name,state,source_url)
            VALUES('milesplit','1000','Synthetic High','WI','https://wi.milesplit.com/teams/1000')
            RETURNING id
        ), athlete AS (
            INSERT INTO athletes(name,graduation_year) VALUES('Synthetic Athlete',2028) RETURNING id
        ), profiles AS (
            INSERT INTO athlete_sources(athlete_id,source,source_id,school_id,details)
            SELECT athlete.id,profile.source,'11',school.id,jsonb_build_object('source_values',
                jsonb_build_object('current_affiliation',jsonb_build_object(
                    'school_state_unverified',profile.unverified,'unverified_school_state','WI')))
            FROM athlete CROSS JOIN school CROSS JOIN (VALUES ('milesplit',$1::boolean),
                ('athleticnet',$2::boolean)) profile(source,unverified)
        ), coach AS (
            INSERT INTO contacts(school_id,kind,name,role,email,sport,source_url,source_key,raw)
            SELECT id,'coach','Synthetic Track Coach','Head Track Coach','track@example.edu',
                   'outdoor','https://example.edu/track',repeat('a',64),'{}' FROM school
        ) SELECT id FROM athlete",
    )
    .bind(mile_split_unverified)
    .bind(athletic_net_unverified)
    .fetch_one(pool)
    .await?)
}

async fn export_scenario(
    mile_split_unverified: bool,
    athletic_net_unverified: bool,
    expected_state: Option<&'static str>,
    expected_warning: bool,
) -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let athlete = seed(
                database.pool(),
                mile_split_unverified,
                athletic_net_unverified,
            )
            .await?;
            let options = ExportOptions {
                states: vec!["WI".into()],
                graduation_years: vec![2028],
                rows_per_file: 1000,
                per_athlete_sheets: false,
            };
            let (records, issues) = database
                .export_snapshot(&options.states, &options.graduation_years)
                .await?;
            assert!(issues.is_empty());
            assert_eq!(records.len(), 1);
            let record = records.first().context("exported athlete")?;
            assert_eq!(
                (record.id, record.needs_review),
                (athlete, expected_warning)
            );
            assert_eq!(record.profile_details.len(), 2);
            assert_eq!((record.schools.len(), record.coaches.len()), (1, 1));
            let school = record.schools.first().context("physical school")?;
            assert_eq!(
                (school.id.as_str(), school.state.as_deref()),
                ("1000", expected_state)
            );
            let coach = record.coaches.first().context("school coach")?;
            assert_eq!(coach.school_id, "1000");
            assert_eq!(coach.email.as_deref(), Some("track@example.edu"));
            let output = test.directory().join("confidence.xlsx");
            assert_eq!(
                database
                    .write_export_with_layout(&output, &options, ExportLayout::SingleSheet)
                    .await?,
                vec![output.clone()]
            );
            assert_workbook(&output, expected_state.is_some())?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

fn assert_workbook(path: &std::path::Path, school_confirmed: bool) -> Result<()> {
    let mut workbook = open_workbook_auto(path)?;
    assert_eq!(workbook.sheet_names(), &["Athletes"]);
    let sheet = workbook.worksheet_range("Athletes")?;
    let mut rows = sheet.rows();
    let headers = rows.next().context("Athletes headers")?;
    assert_eq!(headers.first().context("Name header")?.to_string(), "Name");
    if school_confirmed {
        let row = rows.next().context("confirmed athlete")?;
        for (name, expected) in [
            ("Name", "Synthetic Athlete"),
            ("School", "Synthetic High"),
            ("Track coach email", "track@example.edu"),
        ] {
            let column = headers
                .iter()
                .position(|header| header.to_string() == name)
                .with_context(|| format!("{name} header"))?;
            assert_eq!(
                row.get(column).context("exported cell")?.to_string(),
                expected
            );
        }
    }
    assert_eq!(
        rows.next(),
        None,
        "only confirmed membership produces one athlete row"
    );
    Ok(())
}

#[tokio::test]
async fn given_reviewed_profiles_with_mixed_school_confidence_when_exporting_then_school_and_coaches_are_not_duplicated()
-> Result<()> {
    export_scenario(true, false, Some("WI"), true).await
}

#[tokio::test]
async fn given_reviewed_profiles_with_unverified_school_when_exporting_then_one_uncertain_school_remains()
-> Result<()> {
    export_scenario(true, true, None, true).await
}

#[tokio::test]
async fn given_reviewed_profiles_with_verified_school_when_exporting_then_one_confirmed_school_remains()
-> Result<()> {
    export_scenario(false, false, Some("WI"), false).await
}
