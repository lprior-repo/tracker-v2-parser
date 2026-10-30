#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::{
    config::Config,
    database::Database,
    durable::operations::{
        OperationSpec, Workspace, freeze_export, frozen_plan, prepare_export_chunk, publish_export,
        render_export, verify_export,
    },
    export::ExportLayout,
    model::{ParsedPage, School, Source},
};

fn config() -> Config {
    let mut config = Config::default();
    config.states = vec!["WI".into()];
    config.export.layout = ExportLayout::SingleSheet;
    config.export.rows_per_file = 2000;
    config
}

async fn seed(database: &Database) -> Result<()> {
    let school: i64 = sqlx::query_scalar("INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit','1000','Published WI School','WI','https://wi.milesplit.com/teams/1000') RETURNING id")
        .fetch_one(database.pool()).await?;
    sqlx::query("WITH seeded AS (INSERT INTO athletes(name,state,graduation_year,school_id) SELECT 'Athlete '||n,'WI',2027,$1 FROM generate_series(1,999) n RETURNING id,name,state,graduation_year,school_id) INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,name,school_id,state,graduation_year) SELECT id,'milesplit',(1000000+id)::text,'https://wi.milesplit.com/athletes/'||(1000000+id)::text,name,school_id,state,graduation_year FROM seeded")
        .bind(school).execute(database.pool()).await?;
    for (id, event, mark) in [
        ("11", "100 Meters", "11.00"),
        ("22", "800 Meters", "2:00.00"),
    ] {
        let mut performance = records::performance(mark);
        performance.athlete_id = id.into();
        performance.result_id = Some(format!("result-{id}"));
        performance.event = event.into();
        let mut athlete = records::athlete(id);
        athlete.school_id = Some("1000".into());
        records::persist(
            database,
            &records::target(Source::MileSplit, id),
            &ParsedPage {
                schools: vec![School {
                    id: "1000".into(),
                    name: "Published WI School".into(),
                    state: Some("WI".into()),
                    ..Default::default()
                }],
                athletes: vec![athlete],
                performances: vec![performance],
                ..ParsedPage::default()
            },
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn frozen_chunks_resume_after_db_changes_and_publish_one_verified_sheet_idempotently()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(&database).await?;
            let config = config();
            let spec = OperationSpec::Export {
                config: config.clone(),
            };
            let root = test.directory().join("artifacts");
            let workspace = Workspace::open(&root, "export-a", &spec)?;
            let frozen = freeze_export(&workspace, &config, &test.url).await?;
            let plan = frozen_plan(&workspace, &frozen)?;
            assert_eq!(plan.rows, 1001);
            assert!(plan.chunks.len() >= 2);
            let first = prepare_export_chunk(&workspace, &frozen, 0)?;
            let before = workspace.read(&first)?;
            sqlx::query("UPDATE athletes SET name='Changed after snapshot'")
                .execute(database.pool())
                .await?;
            let resumed = Workspace::open(&root, "export-a", &spec)?;
            assert_eq!(
                freeze_export(&resumed, &config, &test.url).await?.manifest,
                frozen.manifest
            );
            assert_eq!(prepare_export_chunk(&resumed, &frozen, 0)?, first);
            assert_eq!(resumed.read(&first)?, before);
            let workbook = render_export(&resumed, &frozen)?;
            let verified = verify_export(&resumed, &frozen, &workbook)?;
            assert_eq!(verified.rows, 1001);
            let published = publish_export(&resumed, &verified)?;
            assert!(published.promoted);
            assert_eq!(
                publish_export(&resumed, &verified)?.workbook,
                published.workbook
            );
            let mut book = open_workbook_auto(&published.workbook.path)?;
            assert_eq!(book.sheet_names(), ["Athletes"]);
            let range = book.worksheet_range("Athletes")?;
            let headers = range
                .rows()
                .next()
                .context("headers")?
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            assert!(headers.iter().any(|value| value.contains("100 Meters")));
            assert!(headers.iter().any(|value| value.contains("800 Meters")));
            assert!(
                !range
                    .rows()
                    .flatten()
                    .any(|value| value.to_string() == "Changed after snapshot")
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn abandoned_freeze_generation_is_retained_and_new_generation_completes() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            sqlx::query(
                "INSERT INTO athletes(name,state,graduation_year) VALUES('Original','WI',2027)",
            )
            .execute(database.pool())
            .await?;
            let config = config();
            let workspace = Workspace::open(
                &test.directory().join("artifacts"),
                "export-a",
                &OperationSpec::Export {
                    config: config.clone(),
                },
            )?;
            workspace.store(
                "freeze-generation-1.json",
                br#"{"binding":"interrupted","generation":1}"#,
            )?;
            let abandoned =
                workspace.store("freeze-1-chunk-0.json", b"abandoned original bytes")?;
            let frozen = freeze_export(&workspace, &config, &test.url).await?;
            assert_eq!(frozen_plan(&workspace, &frozen)?.generation, 2);
            assert_eq!(workspace.read(&abandoned)?, b"abandoned original bytes");
            assert!(!workspace.path("publication-receipt.json")?.exists());
            database.close().await;
            Ok(())
        })
    })
    .await
}
