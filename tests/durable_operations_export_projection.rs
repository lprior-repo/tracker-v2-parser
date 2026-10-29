#[path = "support/records.rs"]
mod records;
mod support;

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use serde_json::json;
use track_scrapper_v2::{
    config::Config,
    database::Database,
    durable::operations::{
        OperationSpec, Workspace, freeze_export, frozen_plan, render_export, verify_export,
    },
    export::{ExportLayout, ExportOptions, StagedExport},
    model::{Coach, ParsedPage, School, Source, Sport},
};

fn details() -> serde_json::Value {
    json!({"biography": "unused biography".repeat(100_000), "pages": {
        "https://wi.milesplit.com/athletes/11": {"career_history": "unused history".repeat(100_000), "pr_rows": [
            {"masked": false, "unused": "unused row metadata".repeat(30_000), "html": "<tr data-pr-mode='all' data-pr-level='all'><td class='pr-td-event'>800m</td><td class='pr-td-mark'>2:10.00</td><td class='pr-td-date'>Apr 14, 2026</td></tr>"},
            {"masked": true, "html": "<tr data-pr-mode='all' data-pr-level='all'><td class='pr-td-event'>1600m</td><td class='pr-td-mark'>4:10.00</td></tr>"},
            {"masked": false, "html": "<tr data-pr-mode='all' data-pr-level='all'><td class='pr-td-event'>3200m</td><td class='pr-td-mark mask'>8:10.00</td></tr>"}
        ]},
        "https://wi.milesplit.com/athletes/11/progression": {"history": "unused progression".repeat(50_000)}
    }})
}

async fn seed(database: &Database) -> Result<()> {
    let mut athlete = records::athlete("11");
    athlete.school_id = Some("school".into());
    athlete.source_values = Some(details());
    athlete.profile_url = Some(records::target(Source::MileSplit, "11").url);
    let mut hs = records::performance("11.00");
    let published_html = "unused complete result table".repeat(20_000);
    hs.source_values = Some(json!({"level": "hs", "published_html": published_html}));
    let mut ms = hs.clone();
    ms.mark = "9.80".into();
    ms.result_id = Some("middle-school".into());
    ms.source_values = Some(json!({"level": "ms", "published_html": published_html}));
    let mut best = hs.clone();
    best.is_personal_record = false;
    best.mark = "10.80".into();
    best.wind = Some(3.0);
    best.result_id = Some("ordinary-observation".into());
    let mut stale = best.clone();
    stale.mark = "9.00".into();
    stale.result_id = Some("unconfirmed-old-observation".into());
    stale.source_values = Some(
        json!({"level": "hs", "revision_status": "unverified_prior_revision",
        "published_html": published_html}),
    );
    let coaches = [
        ("Track Person", "track@example.edu", "Track Coach", None),
        (
            "XC Person",
            "xc@example.edu",
            "Head Cross Country Coach",
            Some(Sport::CrossCountry),
        ),
    ]
    .into_iter()
    .map(|(name, email, role, sport)| Coach {
        school_id: "school".into(),
        name: Some(name.into()),
        email: Some(email.into()),
        role: role.into(),
        sport,
        review_notes: Vec::new(),
    })
    .collect();
    records::persist(
        database,
        &records::target(Source::MileSplit, "11"),
        &ParsedPage {
            schools: vec![School {
                id: "school".into(),
                name: "Published School".into(),
                state: Some("WI".into()),
                city: Some("Madison".into()),
                address: Some("10 Main Street".into()),
                postal_code: Some("53701".into()),
                ..School::default()
            }],
            athletes: vec![athlete],
            performances: vec![hs, ms, best, stale],
            coaches,
            ..ParsedPage::default()
        },
    )
    .await?;
    sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,name,state,graduation_year,details) SELECT id,'athleticnet','22','https://www.athletic.net/athlete/22',name,state,graduation_year,$1 FROM athletes")
        .bind(json!({"source_values": {"unused": "source provenance only"}}))
        .execute(database.pool()).await?;
    Ok(())
}

fn cells(path: &Path) -> Result<Vec<Vec<String>>> {
    let mut workbook = open_workbook_auto(path)?;
    assert_eq!(workbook.sheet_names(), ["Athletes"]);
    Ok(workbook
        .worksheet_range("Athletes")?
        .rows()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect())
}

#[tokio::test]
async fn freeze_discards_unused_profile_json_with_full_cell_parity_and_retains_headline_masks()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(&database).await?;
            let mut config = Config::default();
            config.states = vec!["WI".into()];
            config.export.layout = ExportLayout::SingleSheet;
            config.export.rows_per_file = 2000;
            let records = database
                .athlete_records_for(&config.states, &config.graduation_years)
                .await?;
            assert_eq!(records.len(), 1);
            let full_bytes = serde_json::to_vec(&records)?;
            assert!(full_bytes.len() > 3 * 1024 * 1024);
            let original_details: String = sqlx::query_scalar(
                "SELECT md5(jsonb_agg(details ORDER BY source)::text) FROM athlete_sources",
            )
            .fetch_one(database.pool())
            .await?;
            let baseline = test.directory().join("full-record.xlsx");
            let mut export = StagedExport::new(
                &baseline,
                &ExportOptions {
                    states: config.states.clone(),
                    graduation_years: config.graduation_years.clone(),
                    rows_per_file: 2000,
                    per_athlete_sheets: false,
                },
            )?
            .with_layout(ExportLayout::SingleSheet);
            export.stage_part(&records, &[])?;
            export.finish()?;
            let workspace = Workspace::open(
                &test.directory().join("artifacts"),
                "projected-export",
                &OperationSpec::Export {
                    config: config.clone(),
                },
            )?;
            let frozen = freeze_export(&workspace, &config, &test.url).await?;
            let plan = frozen_plan(&workspace, &frozen)?;
            assert_eq!((plan.rows, plan.chunks.len()), (1, 1));
            assert!(plan.bytes < 20_000 && plan.bytes * 100 < u64::try_from(full_bytes.len())?);
            let frozen_bytes = workspace.read(&plan.chunks[0].artifact)?;
            let document: serde_json::Value = serde_json::from_slice(&frozen_bytes)?;
            let projected_record = &document["records"][0];
            let profiles = projected_record["profile_details"]
                .as_array()
                .context("projected profiles")?;
            assert_eq!(profiles.len(), 2);
            for (full, projected) in records[0].profile_details.iter().zip(profiles) {
                assert_eq!(projected["source"], serde_json::to_value(full.source)?);
                assert_eq!(projected["source_id"], full.source_id);
                assert_eq!(projected["url"], serde_json::to_value(&full.url)?);
            }
            let projected = &profiles
                .iter()
                .find(|profile| profile["source"] == "milesplit")
                .context("MileSplit profile")?["source_values"];
            for (full, projected) in records[0].performances.iter().zip(
                projected_record["performances"]
                    .as_array()
                    .context("projected performances")?,
            ) {
                let mut full = serde_json::to_value(full)?;
                let mut projected = projected.clone();
                full["performance"]
                    .as_object_mut()
                    .context("full performance")?
                    .remove("source_values");
                projected["performance"]
                    .as_object_mut()
                    .context("projected performance")?
                    .remove("source_values");
                assert_eq!(full, projected);
            }
            assert!(!String::from_utf8(frozen_bytes)?.contains("published_html"));
            assert!(projected.get("biography").is_none());
            let base = &projected["pages"]["https://wi.milesplit.com/athletes/11"];
            assert!(base.get("career_history").is_none());
            assert!(base["pr_rows"][0].get("unused").is_none());
            assert_eq!(base["pr_rows"][1]["masked"], true);
            let workbook = render_export(&workspace, &frozen)?;
            assert_eq!(verify_export(&workspace, &frozen, &workbook)?.rows, 1);
            let actual = cells(&workbook.path)?;
            assert_eq!(actual, cells(&baseline)?);
            assert_eq!(actual.len(), 2);
            let fields = actual[0]
                .iter()
                .cloned()
                .zip(actual[1].iter().cloned())
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                fields["All seasons 800m PR [all levels; milesplit]"],
                "2:10.00 (2026-04-14)"
            );
            assert!(fields["Outdoor 100 Meters PR [hs; milesplit]"].starts_with("11.00"));
            assert!(fields["Outdoor 100 Meters PR [ms; milesplit]"].starts_with("9.80"));
            assert!(
                fields["Outdoor 100m best [hs; milesplit]"]
                    .contains("10.80 (2026-05-15; FAT; wind=3)")
            );
            assert!(!fields["Outdoor 100m best [hs; milesplit]"].contains("9.00"));
            assert_eq!(fields["Track coach"], "Track Person");
            assert_eq!(fields["Track coach email"], "track@example.edu");
            assert_eq!(fields["XC coach"], "XC Person");
            assert_eq!(fields["XC coach email"], "xc@example.edu");
            assert_eq!(
                fields["School address"],
                "Published School: 10 Main Street, Madison, WI 53701"
            );
            assert!(
                !fields
                    .keys()
                    .any(|header| header.contains("1600m") || header.contains("3200m"))
            );
            assert_eq!(
                sqlx::query_scalar::<_, String>(
                    "SELECT md5(jsonb_agg(details ORDER BY source)::text) FROM athlete_sources"
                )
                .fetch_one(database.pool())
                .await?,
                original_details
            );
            let after = database
                .athlete_records_for(&config.states, &config.graduation_years)
                .await?;
            assert_eq!(serde_json::to_vec(&after)?, full_bytes);
            database.close().await;
            Ok(())
        })
    })
    .await
    .context("durable single-sheet projection parity")
}
