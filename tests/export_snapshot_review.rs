#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result, ensure};
use calamine::{Reader, open_workbook_auto};
use sqlx::AssertSqlSafe;
use track_scrapper_v2::{
    database::{
        Database, EXPORT_BATCH_BYTES, EXPORT_BATCH_ROWS, EXPORT_COLLECTION_BYTES,
        EXPORT_COLLECTION_ROWS, EXPORT_COMPONENT_BYTES, EXPORT_MAX_OBSERVATIONS,
        ExportLimitExceeded, ExportSnapshotInvalidated,
    },
    export::{ExportOptions, StagedExport},
    model::{ParsedPage, Source},
};

fn options() -> ExportOptions {
    ExportOptions {
        states: vec!["WI".to_owned()],
        graduation_years: vec![2027],
        rows_per_file: 2,
        per_athlete_sheets: false,
    }
}

async fn seed_athletes(database: &Database, count: i64) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query_as("INSERT INTO athletes (name, graduation_year) SELECT 'Athlete ' || number, 2027 FROM generate_series(1, $1::bigint) number RETURNING id, name")
        .bind(count).fetch_all(database.pool()).await?)
}

async fn schema_signature(pool: &sqlx::PgPool) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar("SELECT COALESCE(jsonb_agg(jsonb_build_array(c.oid,c.relname,c.relkind) ORDER BY c.relname), '[]'::jsonb) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema()")
        .fetch_one(pool).await?)
}

fn readonly_url(url: &str, role: Option<&str>) -> Result<String> {
    let mut url = url::Url::parse(url)?;
    let pairs: Vec<_> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    let mut options = String::new();
    for (key, value) in pairs {
        if key == "options" {
            options = value;
        } else {
            url.query_pairs_mut().append_pair(&key, &value);
        }
    }
    options.push_str(" -cdefault_transaction_read_only=on");
    if let Some(role) = role {
        options.push_str(&format!(" -crole={role}"));
    }
    url.query_pairs_mut().append_pair("options", &options);
    Ok(url.to_string())
}

#[tokio::test]
async fn given_missing_schema_when_export_connects_then_it_fails_without_creating_metadata()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let before = schema_signature(&test.pool).await?;
            let error = Database::connect_for_export(&readonly_url(&test.url, None)?)
                .await
                .err()
                .context("missing schema must fail")?;
            assert!(format!("{error:#}").contains("compatible schema"));
            assert_eq!(schema_signature(&test.pool).await?, before);
            let database = Database::connect(&test.url).await?;
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM _sqlx_migrations WHERE success")
                    .fetch_one(database.pool())
                    .await?,
                i64::try_from(sqlx::migrate!("./migrations").iter().count())?
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_outdated_schema_when_export_connects_then_no_migration_or_ddl_runs() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version=(SELECT max(version) FROM _sqlx_migrations)").execute(database.pool()).await?;
        let before = schema_signature(database.pool()).await?;
        let history: serde_json::Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m").fetch_one(database.pool()).await?;
        let error = Database::connect_for_export(&readonly_url(&test.url, None)?).await.err().context("outdated schema must fail")?;
        assert!(format!("{error:#}").contains("outdated"));
        assert_eq!(schema_signature(database.pool()).await?, before);
        assert_eq!(sqlx::query_scalar::<_, serde_json::Value>("SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m").fetch_one(database.pool()).await?, history);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_select_only_role_when_exporting_compatible_schema_then_workbooks_are_published()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        seed_athletes(&database, 3).await?;
        let schema: String = sqlx::query_scalar("SELECT current_schema()").fetch_one(database.pool()).await?;
        let configured = std::env::var("TEST_EXPORT_READONLY_ROLE").ok();
        let created = configured.is_none();
        let role = configured.map_or_else(|| format!("{schema}_reader"), |role| role);
        ensure!(!role.is_empty() && role.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'), "TEST_EXPORT_READONLY_ROLE must be a simple PostgreSQL identifier");
        if created {
            sqlx::query(AssertSqlSafe(format!("CREATE ROLE {role}"))).execute(database.pool()).await.context("read-only proof requires TEST_EXPORT_READONLY_ROLE with existing membership, or a CI administrator allowed to create the isolated test role")?;
        }
        let result: Result<()> = async {
            if created {
                sqlx::query(AssertSqlSafe(format!("GRANT {role} TO CURRENT_USER"))).execute(database.pool()).await?;
            }
            ensure!(sqlx::query_scalar::<_, bool>("SELECT pg_has_role(current_user,$1,'MEMBER')").bind(&role).fetch_one(database.pool()).await?, "test role must already be a member of TEST_EXPORT_READONLY_ROLE");
            sqlx::query(AssertSqlSafe(format!("GRANT USAGE ON SCHEMA {schema} TO {role}"))).execute(database.pool()).await?;
            sqlx::query(AssertSqlSafe(format!("GRANT SELECT ON ALL TABLES IN SCHEMA {schema} TO {role}"))).execute(database.pool()).await?;
            let reader = Database::connect_for_export(&readonly_url(&test.url, Some(&role))?).await?;
            assert_eq!(sqlx::query_scalar::<_, String>("SHOW transaction_read_only").fetch_one(reader.pool()).await?, "on");
            assert!(!sqlx::query_scalar::<_, bool>("SELECT has_table_privilege(current_user,'athletes','INSERT')").fetch_one(reader.pool()).await?);
            assert!(!sqlx::query_scalar::<_, bool>("SELECT has_schema_privilege(current_user,current_schema(),'CREATE')").fetch_one(reader.pool()).await?);
            let paths = reader.write_export(&test.directory().join("readonly.xlsx"), &options()).await?;
            reader.close().await;
            let names = workbook_names(&paths)?;
            assert_eq!(names, vec!["Athlete 1", "Athlete 2", "Athlete 3"]);
            assert!(test.directory().join("readonly.manifest.json").exists());
            let output = test.directory().join("readonly-cli.xlsx");
            let cli = support::run_operation(test.directory(), Some(&readonly_url(&test.url, Some(&role))?), &support::OfflineOperation::Export{config:None,output:output.clone(),per_athlete:false}).await?;
            support::assert_success(&cli);
            assert_eq!(workbook_names(&[output])?, vec!["Athlete 1", "Athlete 2", "Athlete 3"]);
            Ok(())
        }.await;
        sqlx::query(AssertSqlSafe(format!("REVOKE SELECT ON ALL TABLES IN SCHEMA {schema} FROM {role}"))).execute(database.pool()).await?;
        sqlx::query(AssertSqlSafe(format!("REVOKE USAGE ON SCHEMA {schema} FROM {role}"))).execute(database.pool()).await?;
        if created {
            sqlx::query(AssertSqlSafe(format!("DROP ROLE {role}"))).execute(database.pool()).await?;
        }
        database.close().await;
        result
    })).await
}

fn workbook_names(paths: &[std::path::PathBuf]) -> Result<Vec<String>> {
    paths.iter().try_fold(Vec::new(), |mut names, path| {
        let mut workbook = open_workbook_auto(path)?;
        let main = workbook.worksheet_range("Athletes")?;
        let column = main
            .rows()
            .next()
            .context("athlete headers")?
            .iter()
            .position(|cell| cell.to_string() == "name")
            .context("name column")?;
        names.extend(main.rows().skip(1).map(|row| row[column].to_string()));
        Ok(names)
    })
}

#[tokio::test]
async fn given_multiple_batches_when_writer_mutates_between_parts_then_snapshot_has_every_original_once()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let expected = seed_athletes(&database, 5).await?;
            let mut snapshot = database
                .begin_export_snapshot(&options().states, &options().graduation_years)
                .await?;
            let mut staged =
                StagedExport::new(&test.directory().join("snapshot.xlsx"), &options())?;
            let first = snapshot.next_records(2).await?.context("first batch")?;
            assert_eq!(first.len(), 2);
            staged.stage_part(&first, &[])?;
            sqlx::query("UPDATE athletes SET name='Changed' WHERE name='Athlete 3'")
                .execute(database.pool())
                .await?;
            sqlx::query("DELETE FROM athletes WHERE name='Athlete 4'")
                .execute(database.pool())
                .await?;
            sqlx::query("INSERT INTO athletes(name,graduation_year) VALUES ('Late athlete',2027)")
                .execute(database.pool())
                .await?;
            let second = snapshot.next_records(2).await?.context("second batch")?;
            let third = snapshot.next_records(2).await?.context("third batch")?;
            assert!(snapshot.next_records(2).await?.is_none());
            let actual: Vec<_> = first
                .iter()
                .chain(&second)
                .chain(&third)
                .map(|record| (record.id, record.name.clone()))
                .collect();
            assert_eq!(actual, expected);
            staged.stage_part(&second, &[])?;
            staged.stage_part(&third, &[])?;
            assert!(snapshot.next_issues().await?.is_none());
            snapshot.finish().await?;
            let paths = staged.finish()?;
            assert_eq!(paths.len(), 3);
            assert_eq!(
                workbook_names(&paths)?,
                expected
                    .into_iter()
                    .map(|(_, name)| name)
                    .collect::<Vec<_>>()
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn seed_notes(database: &Database, count: usize, text: &str) -> Result<()> {
    sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor,status,notes) VALUES ('milesplit',repeat('a',64),'https://www.milesplit.com/athletes/notes','{\"context\":{\"state\":\"WI\"}}','succeeded',array_fill($1::text,ARRAY[$2::integer]))")
        .bind(text).bind(i32::try_from(count)?).execute(database.pool()).await?;
    Ok(())
}

#[tokio::test]
async fn given_duplicate_coverage_notes_when_writer_changes_notes_then_keyset_batches_retain_original_multiplicity()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_notes(&database, EXPORT_BATCH_ROWS + 1, "same published note").await?;
            let mut snapshot = database.begin_export_snapshot(&[], &[]).await?;
            let first = snapshot.next_issues().await?.context("first issue batch")?;
            assert_eq!(first.len(), EXPORT_BATCH_ROWS);
            sqlx::query("UPDATE scrape_pages SET notes=ARRAY['replacement note']")
                .execute(database.pool())
                .await?;
            let last = snapshot.next_issues().await?.context("last issue batch")?;
            assert_eq!(last.len(), 1);
            assert!(
                first
                    .iter()
                    .chain(&last)
                    .all(|issue| issue.detail == "page note: same published note")
            );
            assert!(snapshot.next_issues().await?.is_none());
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_coverage_byte_boundary_when_issue_exceeds_budget_then_typed_error_prevents_publication()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let prefix = "page note: ".len()
                + "milesplit".len()
                + "https://www.milesplit.com/athletes/notes".len();
            seed_notes(&database, 1, &"x".repeat(EXPORT_BATCH_BYTES - prefix)).await?;
            let mut snapshot = database.begin_export_snapshot(&[], &[]).await?;
            let boundary = snapshot.next_issues().await?.context("boundary issue")?;
            assert_eq!(
                boundary[0].detail.len() + boundary[0].url.len() + boundary[0].source.len(),
                EXPORT_BATCH_BYTES
            );
            snapshot.finish().await?;
            sqlx::query("UPDATE scrape_pages SET notes=ARRAY[notes[1] || 'x']")
                .execute(database.pool())
                .await?;
            let mut snapshot = database.begin_export_snapshot(&[], &[]).await?;
            let error = snapshot
                .next_issues()
                .await
                .err()
                .context("oversized issue must fail")?;
            let limit = error
                .downcast_ref::<ExportLimitExceeded>()
                .context("typed byte limit")?;
            assert_eq!(limit.kind, "coverage issue serialized bytes");
            assert_eq!(limit.actual, i64::try_from(EXPORT_BATCH_BYTES + 1)?);
            snapshot.finish().await?;
            let output = test.directory().join("oversized-issue.xlsx");
            let error = database
                .write_export(&output, &options())
                .await
                .err()
                .context("export must fail")?;
            assert!(error.downcast_ref::<ExportLimitExceeded>().is_some());
            assert!(!output.with_extension("manifest.json").exists());
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn record_bytes(database: &Database) -> Result<i64> {
    let sql = format!(
        "SELECT octet_length(document::text)::bigint FROM ({}) report(document)",
        include_str!("../src/database/athlete_records.sql")
    );
    Ok(sqlx::query_scalar(AssertSqlSafe(sql))
        .bind(&options().states)
        .bind(&options().graduation_years)
        .fetch_one(database.pool())
        .await?)
}

#[tokio::test]
async fn given_athlete_byte_boundary_when_one_byte_is_added_then_typed_limit_replaces_unbounded_materialization()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_athletes(&database, 1).await?;
            let baseline = record_bytes(&database).await?;
            let name_length =
                i64::try_from(EXPORT_BATCH_BYTES)? - baseline + i64::try_from("Athlete 1".len())?;
            sqlx::query("UPDATE athletes SET name=repeat('x',$1::integer)")
                .bind(i32::try_from(name_length)?)
                .execute(database.pool())
                .await?;
            let mut snapshot = database
                .begin_export_snapshot(&options().states, &options().graduation_years)
                .await?;
            let boundary = snapshot
                .next_records(2)
                .await?
                .context("boundary athlete")?;
            assert_eq!(boundary.len(), 1);
            assert_eq!(boundary[0].name.len(), usize::try_from(name_length)?);
            snapshot.finish().await?;
            sqlx::query("UPDATE athletes SET name=name || 'x'")
                .execute(database.pool())
                .await?;
            let output = test.directory().join("oversized-athlete.xlsx");
            let error = database
                .write_export(&output, &options())
                .await
                .err()
                .context("oversized athlete must fail")?;
            let limit = error
                .downcast_ref::<ExportLimitExceeded>()
                .context("typed athlete byte limit")?;
            assert_eq!(limit.kind, "athlete serialized bytes");
            assert_eq!(limit.actual, i64::try_from(EXPORT_BATCH_BYTES + 1)?);
            assert!(!output.with_extension("manifest.json").exists());
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn add_observations(database: &Database, first: i64, last: i64) -> Result<()> {
    let raw = serde_json::to_value(records::performance("12.34"))?;
    sqlx::query("INSERT INTO results(source_result_key,athlete_id,athlete_source_id,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw) SELECT lpad(number::text,64,'0'), p.athlete_id,p.id,p.source,'100 Meters','12.34','outdoor','recorded',false,false,'https://www.athletic.net/athlete/11',$1::jsonb FROM athlete_sources p CROSS JOIN generate_series($2::bigint,$3::bigint) number WHERE p.source_id='11'")
        .bind(raw).bind(first).bind(last).execute(database.pool()).await?;
    Ok(())
}

#[tokio::test]
async fn given_observation_count_boundary_when_another_row_arrives_then_large_athlete_fails_before_aggregation()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            records::persist(
                &database,
                &records::target(Source::AthleticNet, "11"),
                &ParsedPage {
                    athletes: vec![records::athlete("11")],
                    ..ParsedPage::default()
                },
            )
            .await?;
            add_observations(&database, 1, EXPORT_MAX_OBSERVATIONS).await?;
            let mut snapshot = database
                .begin_export_snapshot(&options().states, &options().graduation_years)
                .await?;
            assert_eq!(
                snapshot
                    .next_records(1)
                    .await?
                    .context("boundary athlete")?[0]
                    .performances
                    .len(),
                usize::try_from(EXPORT_MAX_OBSERVATIONS)?
            );
            snapshot.finish().await?;
            add_observations(
                &database,
                EXPORT_MAX_OBSERVATIONS + 1,
                EXPORT_MAX_OBSERVATIONS + 1,
            )
            .await?;
            let mut snapshot = database
                .begin_export_snapshot(&options().states, &options().graduation_years)
                .await?;
            let error = snapshot
                .next_records(1)
                .await
                .err()
                .context("observation count must fail")?;
            let limit = error
                .downcast_ref::<ExportLimitExceeded>()
                .context("typed observation limit")?;
            assert_convenience_limit(&database, "athlete observations").await?;
            assert_eq!(limit.kind, "athlete observations");
            assert_eq!(limit.actual, EXPORT_MAX_OBSERVATIONS + 1);
            assert_eq!(limit.limit, EXPORT_MAX_OBSERVATIONS);
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_two_large_athletes_when_batch_byte_budget_fills_then_next_batch_retains_pending_athlete()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let expected = seed_athletes(&database, 2).await?;
            sqlx::query("UPDATE athletes SET name=repeat('x',$1::integer)")
                .bind(i32::try_from(EXPORT_BATCH_BYTES / 2)?)
                .execute(database.pool())
                .await?;
            let mut snapshot = database
                .begin_export_snapshot(&options().states, &options().graduation_years)
                .await?;
            let first = snapshot
                .next_records(EXPORT_BATCH_ROWS)
                .await?
                .context("first bounded byte batch")?;
            let second = snapshot
                .next_records(EXPORT_BATCH_ROWS)
                .await?
                .context("second bounded byte batch")?;
            assert_eq!(first.len(), 1);
            assert_eq!(second.len(), 1);
            assert_eq!(
                vec![first[0].id, second[0].id],
                expected.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
            );
            assert!(snapshot.next_records(EXPORT_BATCH_ROWS).await?.is_none());
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_large_coverage_rows_when_batch_byte_budget_fills_then_both_duplicate_rows_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_notes(&database, 2, &"x".repeat(EXPORT_BATCH_BYTES / 2)).await?;
            let mut snapshot = database.begin_export_snapshot(&[], &[]).await?;
            let first = snapshot
                .next_issues()
                .await?
                .context("first byte bounded issue")?;
            let second = snapshot
                .next_issues()
                .await?
                .context("second byte bounded issue")?;
            assert_eq!(first.len(), 1);
            assert_eq!(second.len(), 1);
            assert_eq!(first[0].detail, second[0].detail);
            assert_eq!(
                first[0].detail.len(),
                EXPORT_BATCH_BYTES / 2 + "page note: ".len()
            );
            assert!(snapshot.next_issues().await?.is_none());
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_current_migration_history_but_missing_export_column_when_connecting_then_schema_is_not_repaired()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        sqlx::query("ALTER TABLE schools DROP COLUMN uuid").execute(database.pool()).await?;
        let history: serde_json::Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m").fetch_one(database.pool()).await?;
        let error = Database::connect_for_export(&readonly_url(&test.url, None)?).await.err().context("missing required export column must fail")?;
        assert!(format!("{error:#}").contains("validating existing athlete export schema"));
        assert!(!sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='schools' AND column_name='uuid')").fetch_one(database.pool()).await?);
        assert_eq!(sqlx::query_scalar::<_, serde_json::Value>("SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m").fetch_one(database.pool()).await?, history);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_oversized_constituents_when_exporting_then_every_component_is_checked_before_aggregation()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        records::persist(&database, &records::target(Source::AthleticNet,"11"), &ParsedPage { athletes: vec![records::athlete("11")], performances: vec![records::performance("12.34")], ..ParsedPage::default() }).await?;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,source_url) VALUES ('athleticnet','bounded-school','School','WI','https://www.athletic.net/team/1')").execute(database.pool()).await?;
        sqlx::query("UPDATE athlete_sources SET school_id=(SELECT id FROM schools LIMIT 1)").execute(database.pool()).await?;
        sqlx::query("INSERT INTO contacts(school_id,kind,role,email,source_url,raw,source_key) SELECT id,'coach','Head Coach','coach@example.org','https://www.athletic.net/team/1','{}',encode(sha256(convert_to(jsonb_build_array('https://www.athletic.net/team/1',NULL::text,'Head Coach',NULL::text,'coach@example.org')::text,'UTF8')),'hex') FROM schools").execute(database.pool()).await?;
        let cases = [
            ("UPDATE athlete_sources SET details=jsonb_build_object('source_values',jsonb_build_object('published',repeat('x',$1)))", "UPDATE athlete_sources SET details='{}'"),
            ("UPDATE results SET raw=raw || jsonb_build_object('source_values',jsonb_build_object('published',repeat('x',$1)))", "UPDATE results SET raw=raw-'source_values'"),
            ("UPDATE schools SET address=repeat('x',$1)", "UPDATE schools SET address=NULL"),
            ("UPDATE contacts SET raw=jsonb_build_object('review_notes',jsonb_build_array(repeat('x',$1)))", "UPDATE contacts SET raw='{}'"),
        ];
        for (oversize, restore) in cases {
            sqlx::query(AssertSqlSafe(oversize)).bind(i32::try_from(EXPORT_COMPONENT_BYTES + 1)?).execute(database.pool()).await?;
            let mut snapshot = database.begin_export_snapshot(&options().states, &options().graduation_years).await?;
            let error = snapshot.next_records(1).await.err().context("oversized constituent must fail")?;
            let limit = error.downcast_ref::<ExportLimitExceeded>().context("typed constituent budget")?;
            assert_convenience_limit(&database, "athlete constituent serialized bytes").await?;
            assert_eq!(limit.kind, "athlete constituent serialized bytes");
            assert!(limit.actual > EXPORT_COMPONENT_BYTES);
            assert!(snapshot.next_records(1).await.err().context("failed snapshot must not resume")?.downcast_ref::<ExportSnapshotInvalidated>().is_some());
            assert!(snapshot.next_issues().await.err().context("failed snapshot must not read another category")?.downcast_ref::<ExportSnapshotInvalidated>().is_some());
            snapshot.finish().await?;
            sqlx::query(AssertSqlSafe(restore)).execute(database.pool()).await?;
        }
        assert_eq!(database.export_snapshot(&options().states,&options().graduation_years).await?.0.len(),1);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_failed_issue_budget_when_retrying_the_same_snapshot_then_offending_row_cannot_be_skipped()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_notes(&database, 1, &"x".repeat(EXPORT_BATCH_BYTES)).await?;
            let mut snapshot = database.begin_export_snapshot(&[], &[]).await?;
            assert!(
                snapshot
                    .next_issues()
                    .await
                    .err()
                    .context("oversized note must fail")?
                    .downcast_ref::<ExportLimitExceeded>()
                    .is_some()
            );
            assert!(
                snapshot
                    .next_issues()
                    .await
                    .err()
                    .context("retry must fail closed")?
                    .downcast_ref::<ExportSnapshotInvalidated>()
                    .is_some()
            );
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_large_convenience_snapshot_when_collecting_then_total_byte_budget_is_typed()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_athletes(&database, 6).await?;
            sqlx::query("UPDATE athletes SET name=repeat('x',$1::integer)")
                .bind(i32::try_from(EXPORT_BATCH_BYTES * 3 / 4)?)
                .execute(database.pool())
                .await?;
            let error = database
                .export_snapshot(&options().states, &options().graduation_years)
                .await
                .err()
                .context("whole snapshot collection must be bounded")?;
            let limit = error
                .downcast_ref::<ExportLimitExceeded>()
                .context("typed total byte limit")?;
            assert_convenience_limit(&database, "collected snapshot serialized bytes").await?;
            assert_eq!(limit.kind, "collected snapshot serialized bytes");
            assert_eq!(limit.limit, i64::try_from(EXPORT_COLLECTION_BYTES)?);
            assert!(limit.actual > limit.limit);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_many_small_issues_when_collecting_then_total_item_budget_is_typed() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_notes(&database, EXPORT_COLLECTION_ROWS + 1, "note").await?;
            let error = database
                .export_snapshot(&[], &[])
                .await
                .err()
                .context("whole snapshot item count must be bounded")?;
            let limit = error
                .downcast_ref::<ExportLimitExceeded>()
                .context("typed total item limit")?;
            assert_eq!(limit.kind, "collected snapshot rows");
            assert_eq!(limit.actual, i64::try_from(EXPORT_COLLECTION_ROWS + 1)?);
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn install_blocked_second_result(database: &Database, owner_id: i64, key: i64) -> Result<()> {
    let columns:String=sqlx::query_scalar("SELECT string_agg(CASE WHEN attname='raw' THEN 'snapshot_block_raw(raw,athlete_id) AS raw' ELSE quote_ident(attname) END,',' ORDER BY attnum) FROM pg_attribute WHERE attrelid='results'::regclass AND attnum>0 AND NOT attisdropped").fetch_one(database.pool()).await?;
    sqlx::query("ALTER TABLE results RENAME TO snapshot_results_storage")
        .execute(database.pool())
        .await?;
    sqlx::query(AssertSqlSafe(format!("CREATE FUNCTION snapshot_block_raw(value jsonb,owner_id bigint) RETURNS jsonb LANGUAGE plpgsql STABLE AS $body$ BEGIN IF owner_id={owner_id} THEN PERFORM pg_advisory_xact_lock({key}::bigint); END IF; RETURN value; END $body$"))).execute(database.pool()).await?;
    sqlx::query(AssertSqlSafe(format!(
        "CREATE VIEW results AS SELECT {columns} FROM snapshot_results_storage"
    )))
    .execute(database.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn given_batch_with_partial_progress_when_cancelled_during_database_wait_then_snapshot_cannot_resume()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        use futures::{StreamExt,TryStreamExt};
        let database=Database::connect(&test.url).await?;
        let ids=seed_athletes(&database,2).await?;
        sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id) SELECT id,'athleticnet',id::text FROM athletes").execute(database.pool()).await?;
        sqlx::query("INSERT INTO results(source_result_key,athlete_id,athlete_source_id,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw) SELECT lpad(id::text,64,'0'),athlete_id,id,source,'100 Meters','12.34','outdoor','recorded',false,false,'https://www.athletic.net/athlete/11',$1::jsonb FROM athlete_sources").bind(serde_json::to_value(records::performance("12.34"))?).execute(database.pool()).await?;
        let key:i64=sqlx::query_scalar("SELECT hashtextextended(current_schema(),0)").fetch_one(database.pool()).await?;
        install_blocked_second_result(&database,ids[1].0,key).await?;
        let mut holder=database.pool().acquire().await?;
        let holder_pid:i32=sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *holder).await?;
        sqlx::query("SELECT pg_advisory_lock($1)").bind(key).execute(&mut *holder).await?;
        let mut snapshot=database.begin_export_snapshot(&options().states,&options().graduation_years).await?;
        let mut batch=Box::pin(snapshot.next_records(2));
        let mut observed=futures::stream::iter(0..10_000).then(|_| async {
            sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))").bind(holder_pid).fetch_one(&test.pool).await.map_err(anyhow::Error::from)
        }).try_filter(|waiting| futures::future::ready(*waiting)).boxed();
        let waiting=observed.try_next();
        tokio::pin!(waiting);
        tokio::time::timeout(std::time::Duration::from_secs(10),async {
            tokio::select! {
                result=&mut batch => anyhow::bail!("batch unexpectedly completed before releasing second result: {}",result.is_ok()),
                waiting=&mut waiting => ensure!(waiting?.is_some(),"database wait was not observed"),
            }
            Ok::<_,anyhow::Error>(())
        }).await??;
        drop(batch);
        assert!(snapshot.next_records(2).await.err().context("cancelled snapshot must fail closed")?.downcast_ref::<ExportSnapshotInvalidated>().is_some());
        assert!(snapshot.next_issues().await.err().context("cancelled snapshot cannot switch categories")?.downcast_ref::<ExportSnapshotInvalidated>().is_some());
        assert!(sqlx::query_scalar::<_,bool>("SELECT pg_advisory_unlock($1)").bind(key).fetch_one(&mut *holder).await?);
        drop(holder);
        snapshot.finish().await?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_too_many_coaches_when_reading_an_athlete_then_count_limit_precedes_coach_aggregation()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        records::persist(&database,&records::target(Source::AthleticNet,"11"),&ParsedPage { athletes:vec![records::athlete("11")],..ParsedPage::default() }).await?;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,source_url) VALUES ('athleticnet','school','School','WI','https://www.athletic.net/team/1')").execute(database.pool()).await?;
        sqlx::query("UPDATE athlete_sources SET school_id=(SELECT id FROM schools LIMIT 1)").execute(database.pool()).await?;
        sqlx::query("INSERT INTO contacts(school_id,kind,role,email,source_url,raw,source_key) SELECT s.id,'coach','Head Coach','coach-' || number || '@example.org','https://www.athletic.net/team/1','{}',encode(sha256(convert_to(jsonb_build_array('https://www.athletic.net/team/1',NULL::text,'Head Coach',NULL::text,'coach-' || number || '@example.org')::text,'UTF8')),'hex') FROM schools s CROSS JOIN generate_series(1,10001) number").execute(database.pool()).await?;
        let mut snapshot=database.begin_export_snapshot(&options().states,&options().graduation_years).await?;
        let error=snapshot.next_records(1).await.err().context("coach component count must be bounded")?;
        let limit=error.downcast_ref::<ExportLimitExceeded>().context("typed coach count limit")?;
        assert_eq!(limit.kind,"athlete component rows");
        assert_eq!(limit.actual,10_001);
        snapshot.finish().await?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_historical_school_when_snapshot_scopes_and_budgets_main_rows_then_only_current_profile_schools_apply()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let athletes = seed_athletes(&database, 2).await?;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,address,source_url) VALUES ('athleticnet','current','Current School','WI','Current address','https://www.athletic.net/team/1'),('athleticnet','historical','Historical School','OH',repeat('x',$1),'https://www.athletic.net/team/2')")
            .bind(i32::try_from(EXPORT_COMPONENT_BYTES + 1)?).execute(database.pool()).await?;
        sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,school_id) SELECT id,'athleticnet',id::text,CASE WHEN id=$1 THEN (SELECT id FROM schools WHERE source_id='current') END FROM athletes")
            .bind(athletes[0].0).execute(database.pool()).await?;
        sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,school_id) VALUES ($1,'milesplit','same-current-school',(SELECT id FROM schools WHERE source_id='current'))")
            .bind(athletes[0].0).execute(database.pool()).await?;
        sqlx::query("INSERT INTO contacts(school_id,kind,role,name,email,source_url,raw,source_key) SELECT id,'coach','Head Coach',name,source_id || '@example.org',source_url,'{}',encode(sha256(convert_to(jsonb_build_array(source_url,name,'Head Coach',NULL::text,lower(btrim(source_id || '@example.org')))::text,'UTF8')),'hex') FROM schools").execute(database.pool()).await?;
        let mut performance = records::performance("12.34");
        performance.school_id = Some("historical".to_owned());
        sqlx::query("INSERT INTO results(source_result_key,athlete_id,athlete_source_id,represented_school_id,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw) SELECT lpad(p.id::text,64,'0'),p.athlete_id,p.id,(SELECT id FROM schools WHERE source_id='historical'),p.source,'100 Meters','12.34','outdoor','recorded',false,false,'https://www.athletic.net/athlete/11',$1::jsonb FROM athlete_sources p WHERE p.source='athleticnet'")
            .bind(serde_json::to_value(performance)?).execute(database.pool()).await?;
        let (selected, _) = database.export_snapshot(&["WI".to_owned()], &[2027]).await?;
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].id, athletes[0].0);
        assert_eq!(selected[0].schools.len(), 1);
        assert_eq!(selected[0].schools[0].name, "Current School");
        assert_eq!(selected[0].coaches.len(), 1);
        assert_eq!(selected[0].coaches[0].email.as_deref(), Some("current@example.org"));
        assert!(selected[1].schools.is_empty());
        assert!(selected[1].coaches.is_empty());
        assert!(selected[1].needs_review);
        assert!(selected.iter().all(|athlete| athlete.performances.len() == 1 && athlete.performances[0].performance.school_id.as_deref() == Some("historical")));
        let (ohio, _) = database.export_snapshot(&["OH".to_owned()], &[2027]).await?;
        assert_eq!(ohio.iter().map(|athlete| athlete.id).collect::<Vec<_>>(), vec![athletes[1].0]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_mixed_coverage_backlog_when_batches_cross_phases_and_writer_updates_then_snapshot_preserves_every_issue()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor,status,notes) SELECT 'milesplit',lpad(number::text,64,'0'),'https://www.milesplit.com/athletes/' || number,'{\"context\":{\"state\":\"WI\"}}','pending',ARRAY['published-note','published-note'] FROM generate_series(1,2001) number").execute(database.pool()).await?;
        sqlx::query("INSERT INTO results(source_result_key,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw) VALUES (repeat('c',64),'milesplit','100 Meters','12.34','outdoor','recorded',false,false,'https://www.milesplit.com/athletes/2001','{}')").execute(database.pool()).await?;
        let mut snapshot = database.begin_export_snapshot(&["WI".to_owned()], &[2027]).await?;
        let mut issues = snapshot.next_issues().await?.context("first status batch")?;
        assert_eq!(issues.len(), EXPORT_BATCH_ROWS);
        assert!(issues.iter().all(|issue| issue.detail == "page pending"));
        sqlx::query("UPDATE scrape_pages SET status='succeeded',notes=ARRAY['changed-note']").execute(database.pool()).await?;
        sqlx::query("UPDATE results SET event='Changed event'").execute(database.pool()).await?;
        while let Some(batch) = snapshot.next_issues().await? {
            assert!(batch.len() <= EXPORT_BATCH_ROWS);
            assert!(batch.iter().map(|issue| issue.source.len() + issue.url.len() + issue.detail.len()).sum::<usize>() <= EXPORT_BATCH_BYTES);
            issues.extend(batch);
        }
        assert_eq!(issues.len(), 6_004);
        assert_eq!(issues.iter().filter(|issue| issue.detail == "page pending").count(), 2_001);
        assert_eq!(issues.iter().filter(|issue| issue.detail == "page note: published-note").count(), 4_002);
        assert_eq!(issues.iter().filter(|issue| issue.detail == "unassigned result | 100 Meters | 12.34 | recorded").count(), 1);
        assert_eq!(issues.iter().filter(|issue| issue.url.ends_with("/2001") && issue.detail == "page note: published-note").count(), 2);
        assert!(issues.iter().all(|issue| !issue.detail.contains("Changed") && !issue.detail.contains("changed-note")));
        snapshot.finish().await?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_small_issues_in_all_phases_when_exporting_then_requested_single_workbook_contains_every_issue()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        seed_athletes(&database, 1).await?;
        seed_notes(&database, 1, "published-note").await?;
        sqlx::query("UPDATE scrape_pages SET status='pending'").execute(database.pool()).await?;
        sqlx::query("INSERT INTO results(source_result_key,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw) VALUES (repeat('b',64),'milesplit','100 Meters','12.34','outdoor','recorded',false,false,'https://www.milesplit.com/athletes/notes','{}')").execute(database.pool()).await?;
        let output = test.directory().join("all-phases.xlsx");
        let parts = database.write_export(&output, &options()).await?;
        assert_eq!(parts, vec![output.clone()]);
        assert!(!test.directory().join("all-phases-0001.xlsx").exists());
        assert_eq!(workbook_names(&parts)?, vec!["Athlete 1"]);
        let mut workbook = open_workbook_auto(&output)?;
        let issues = workbook.worksheet_range("Issues")?;
        let cells = issues.rows().flatten().map(ToString::to_string).collect::<Vec<_>>();
        for published in ["page pending", "page note: published-note", "unassigned result | 100 Meters | 12.34 | recorded"] {
            assert!(cells.iter().any(|cell| cell.contains(published)), "missing {published}");
        }
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_issue_byte_boundary_between_phases_when_reading_then_next_phase_is_deferred_without_limit_error()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_notes(&database, 1, &"x".repeat(EXPORT_BATCH_BYTES / 2)).await?;
            sqlx::query(
                "UPDATE scrape_pages SET status='failed',last_error=repeat('y',$1::integer)",
            )
            .bind(i32::try_from(EXPORT_BATCH_BYTES / 2)?)
            .execute(database.pool())
            .await?;
            let mut snapshot = database
                .begin_export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            let first = snapshot.next_issues().await?.context("status batch")?;
            assert_eq!(first.len(), 1);
            assert!(first[0].detail.starts_with("page failed: "));
            let second = snapshot
                .next_issues()
                .await?
                .context("deferred note batch")?;
            assert_eq!(second.len(), 1);
            assert_eq!(
                second[0].detail,
                format!("page note: {}", "x".repeat(EXPORT_BATCH_BYTES / 2))
            );
            assert!(snapshot.next_issues().await?.is_none());
            snapshot.finish().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_current_school_and_factual_athlete_states_when_scoped_then_school_state_wins_and_unknowns_remain_reviewable()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        sqlx::query("INSERT INTO athletes(name,state,graduation_year) VALUES ('Known Alabama','AL',2027),('Known Wisconsin',U&'\\00A0wi\\2003',2027),('Unknown State',NULL,2027),('Blank State',U&'\\2003\\00A0',2027),('Current WI overrides AL','AL',2027),('Current AL overrides WI','WI',2027),('Unknown Current School uses WI','WI',2027),('Unknown Current School uses AL','AL',2027),('Unknown Current School no state',NULL,2027)").execute(database.pool()).await?;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,source_url) VALUES ('athleticnet','wi','Current Wisconsin',U&'\\2003wI\\00A0','https://www.athletic.net/team/1'),('athleticnet','al','Current Alabama','AL','https://www.athletic.net/team/2'),('athleticnet','unknown','Current Unknown',U&'\\2003\\00A0','https://www.athletic.net/team/3')").execute(database.pool()).await?;
        sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,school_id) SELECT id,'athleticnet',id::text,CASE WHEN name='Current WI overrides AL' THEN (SELECT id FROM schools WHERE source_id='wi') WHEN name='Current AL overrides WI' THEN (SELECT id FROM schools WHERE source_id='al') WHEN name LIKE 'Unknown Current School %' THEN (SELECT id FROM schools WHERE source_id='unknown') END FROM athletes").execute(database.pool()).await?;
        let mut historical = records::performance("12.34");
        historical.school_id = Some("wi".to_owned());
        sqlx::query("INSERT INTO results(source_result_key,athlete_id,athlete_source_id,represented_school_id,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw) SELECT repeat('d',64),p.athlete_id,p.id,(SELECT id FROM schools WHERE source_id='wi'),p.source,'100 Meters','12.34','outdoor','recorded',false,false,'https://www.athletic.net/athlete/11',$1::jsonb FROM athlete_sources p JOIN athletes a ON a.id=p.athlete_id WHERE a.name='Known Alabama'").bind(serde_json::to_value(historical)?).execute(database.pool()).await?;
        let expected_wi = vec!["Known Wisconsin", "Unknown State", "Blank State", "Current WI overrides AL", "Unknown Current School uses WI", "Unknown Current School no state"];
        let selected = database.athlete_records_for(&["\u{00a0}Wi\u{2003}".to_owned()], &[2027]).await?;
        assert_eq!(selected.iter().map(|athlete| athlete.name.as_str()).collect::<Vec<_>>(), expected_wi);
        let (snapshot, _) = database.export_snapshot(&["WI".to_owned()], &[2027]).await?;
        assert_eq!(snapshot.iter().map(|athlete| athlete.name.as_str()).collect::<Vec<_>>(), expected_wi);
        for unknown in ["Known Wisconsin", "Unknown State", "Blank State", "Unknown Current School no state"] {
            assert!(snapshot.iter().find(|athlete| athlete.name == unknown).context("retained review athlete")?.needs_review);
        }
        let (alabama, _) = database.export_snapshot(&["AL".to_owned()], &[2027]).await?;
        assert_eq!(alabama.iter().map(|athlete| athlete.name.as_str()).collect::<Vec<_>>(), vec!["Known Alabama", "Unknown State", "Blank State", "Current AL overrides WI", "Unknown Current School uses AL", "Unknown Current School no state"]);
        let historical = alabama.iter().find(|athlete| athlete.name == "Known Alabama").context("historical athlete")?;
        assert!(historical.schools.is_empty());
        assert_eq!(historical.performances.len(), 1);
        assert_eq!(historical.performances[0].performance.school_id.as_deref(), Some("wi"));
        let mut export_options = options();
        export_options.rows_per_file = 100;
        export_options.states = vec!["\u{00a0}Wi\u{2003}".to_owned()];
        let paths = database.write_export(&test.directory().join("state-fallback.xlsx"), &export_options).await?;
        assert_eq!(workbook_names(&paths)?, expected_wi);
        database.close().await;
        Ok(())
    })).await
}

async fn assert_convenience_limit(database: &Database, kind: &str) -> Result<()> {
    for scoped in [false, true] {
        let result = if scoped {
            database
                .athlete_records_for(&options().states, &options().graduation_years)
                .await
        } else {
            database.athlete_records().await
        };
        let error = result
            .err()
            .context("convenience API must enforce snapshot limits")?;
        let limit = error
            .downcast_ref::<ExportLimitExceeded>()
            .context("typed convenience limit")?;
        assert_eq!(limit.kind, kind);
        assert!(limit.actual > limit.limit);
    }
    Ok(())
}

#[tokio::test]
async fn given_oversized_athlete_when_using_convenience_apis_then_both_return_typed_limits()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_athletes(&database, 1).await?;
            sqlx::query("UPDATE athletes SET name=repeat('x',$1::integer)")
                .bind(i32::try_from(EXPORT_BATCH_BYTES)?)
                .execute(database.pool())
                .await?;
            assert_convenience_limit(&database, "athlete serialized bytes").await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_excess_coverage_when_using_convenience_apis_then_shared_collection_bound_applies()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_notes(&database, EXPORT_COLLECTION_ROWS + 1, "note").await?;
            assert_convenience_limit(&database, "collected snapshot rows").await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_readonly_connection_when_using_convenience_apis_then_records_match_snapshot_without_migrations()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_athletes(&database, 2).await?;
            sqlx::query("UPDATE athletes SET graduation_year=2028 WHERE name='Athlete 2'")
                .execute(database.pool())
                .await?;
            let before = schema_signature(database.pool()).await?;
            let reader = Database::connect_for_export(&readonly_url(&test.url, None)?).await?;
            let all = reader.athlete_records().await?;
            assert_eq!(
                all.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
                ["Athlete 1", "Athlete 2"]
            );
            let scoped = reader
                .athlete_records_for(&options().states, &options().graduation_years)
                .await?;
            assert_eq!(scoped.len(), 1);
            assert_eq!(scoped[0].name, "Athlete 1");
            let expected = reader
                .export_snapshot(&options().states, &options().graduation_years)
                .await?
                .0;
            assert_eq!(
                serde_json::to_value(scoped)?,
                serde_json::to_value(expected)?
            );
            assert_eq!(schema_signature(database.pool()).await?, before);
            reader.close().await;
            database.close().await;
            Ok(())
        })
    })
    .await
}
