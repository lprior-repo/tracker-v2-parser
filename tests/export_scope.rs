mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::database::Database;

async fn seed(test: &support::Scenario) -> Result<Database> {
    let database = Database::connect(&test.url).await?;
    sqlx::query("INSERT INTO schools(source,source_id,name,state,source_url) VALUES ('athleticnet','wi-school','Same Name','WI','school-wi'),('milesplit','wi-school','Same Name','AL','school-al')")
        .execute(&test.pool).await?;
    sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error,notes,created_at) SELECT source,lpad(id::text,64,'0'),url,jsonb_build_object('kind',kind,'context',context),status,attempts,error,ARRAY[note],'2025-01-01'::timestamptz FROM (VALUES
        (1,'athleticnet','wi-pending','athlete','{\"state\":\"WI\"}'::jsonb,'pending',0,NULL,'wi pending note'),
        (2,'athleticnet','wi-failed','athlete','{\"state\":\"WI\"}'::jsonb,'failed',3,'wi failure','wi failed note'),
        (3,'athleticnet','al-pending','athlete','{\"state\":\"AL\"}'::jsonb,'pending',0,NULL,'al pending note'),
        (4,'athleticnet','al-failed','athlete','{\"state\":\"AL\"}'::jsonb,'failed',3,'al failure','al failed note'),
        (5,'athleticnet','unknown','athlete','{}'::jsonb,'pending',2,NULL,'unknown note'),
        (6,'athleticnet','wi-school-page','school','{\"school_id\":\"wi-school\"}'::jsonb,'succeeded',0,NULL,'wi school note'),
        (7,'milesplit','al-school-page','school','{\"school_id\":\"wi-school\"}'::jsonb,'succeeded',0,NULL,'al school note'),
        (8,'athleticnet','wi-root','directory','{\"subject_id\":\"WI\"}'::jsonb,'pending',0,NULL,'wi root note'),
        (9,'athleticnet','national-root','directory','{\"subject_id\":\"AL,WI\"}'::jsonb,'pending',1,NULL,'national root note')
        ) AS fixture(id,source,url,kind,context,status,attempts,error,note)")
        .execute(&test.pool).await?;
    sqlx::query("INSERT INTO results(source_result_key,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw,represented_school_id)
        SELECT lpad(id::text,64,'0'),source,event,'45.20','outdoor','ok',false,true,url,'{}'::jsonb,(SELECT id FROM schools WHERE source='athleticnet' AND source_id=school)
        FROM (VALUES (1,'athleticnet','WI provenance relay','wi-pending',NULL),
        (2,'athleticnet','AL provenance relay','al-pending',NULL),
        (3,'athleticnet','Unknown relay','unknown',NULL),
        (4,'athleticnet','WI school relay','no-page','wi-school'),
        (5,'milesplit','Wrong provider relay','wi-pending',NULL)) AS fixture(id,source,event,url,school)")
        .execute(&test.pool).await?;
    Ok(database)
}

async fn snapshot(test: &support::Scenario) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r),'schools',(SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM schools s))")
        .fetch_one(&test.pool).await?)
}

#[tokio::test]
async fn given_national_backlog_when_wisconsin_cli_exports_then_only_scoped_issues_are_written_without_mutation()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = seed(test).await?;
            let before = snapshot(test).await?;
            let config = test.directory().join("wi.toml");
            support::write_config(&config)?;
            let configured = std::fs::read_to_string(&config)?
                .replace("states = [\"WI\", \"OH\", \"CA\"]", "states = [\"WI\"]");
            std::fs::write(&config, configured)?;
            let output = test.directory().join("scoped.xlsx");
            support::assert_success(
                &test
                    .cli(&[
                        "export",
                        "--config",
                        config.to_str().context("UTF-8 config")?,
                        "--output",
                        output.to_str().context("UTF-8 output")?,
                    ])
                    .await?,
            );
            let mut workbook = open_workbook_auto(&output)?;
            let sheet = workbook.worksheet_range("Issues")?;
            let rows: Vec<String> = sheet
                .rows()
                .skip(1)
                .map(|row| {
                    row.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect();
            assert_eq!(rows.len(), 9);
            for marker in [
                "page pending",
                "wi failure",
                "wi pending note",
                "wi failed note",
                "wi school note",
                "wi root note",
                "WI provenance relay",
                "WI school relay",
            ] {
                assert!(
                    rows.iter().any(|row| row.contains(marker)),
                    "missing {marker}: {rows:?}"
                );
            }
            assert!(!rows.iter().any(|row| row.contains("al-")
                || row.contains("AL provenance")
                || row.contains("Unknown relay")
                || row.contains("unknown note")
                || row.contains("national-root")
                || row.contains("Wrong provider")));
            assert_eq!(snapshot(test).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_national_backlog_when_empty_state_report_is_requested_then_all_issues_and_attempts_are_preserved()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = seed(test).await?;
            let before = snapshot(test).await?;
            let (_, issues) = database.export_snapshot(&[], &[2027]).await?;
            assert_eq!(issues.len(), 21);
            for marker in [
                "al failure",
                "al pending note",
                "unknown note",
                "national root note",
                "AL provenance relay",
                "Unknown relay",
                "Wrong provider relay",
            ] {
                assert!(
                    issues.iter().any(|issue| issue.detail.contains(marker)),
                    "missing {marker}"
                );
            }
            assert_eq!(snapshot(test).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}
