#[path = "durable_page/support.rs"]
mod page_support;
#[path = "native_migration/shadow.rs"]
mod shadow;
mod support;

use std::{collections::BTreeMap, str::FromStr};

use anyhow::{Context, Result};
use futures::FutureExt;
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use tokio::process::Command;
use track_scrapper_v2::{
    database::{Database, operation_receipts},
    durable::page,
};

#[tokio::test]
async fn given_thirteen_populated_tables_when_native_backup_restores_then_hashes_and_stops_survive()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let admin_url = std::env::var("TEST_MIGRATION_ADMIN_DATABASE_URL")
            .context("TEST_MIGRATION_ADMIN_DATABASE_URL required for disposable migration databases; no production fallback")?;
        let identity = format!("{}_{}",std::process::id(),chrono::Utc::now().timestamp_nanos_opt().context("test clock")?);
        let source = format!("native_migration_source_{identity}");
        let restored = format!("native_migration_target_{identity}");
        let options = PgConnectOptions::from_str(&test.url)?;
        let owner = options.get_username();
        assert!(owner.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'));
        let mut admin = PgConnection::connect(&admin_url).await?;
        assert!(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=ANY($1))")
            .bind(vec![&source,&restored]).fetch_one(&mut admin).await?);
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{source}\" OWNER \"{owner}\" TEMPLATE template0 ENCODING 'UTF8'")))
            .execute(&mut admin).await?;
        let result = std::panic::AssertUnwindSafe(exercise(test,&source,&restored)).catch_unwind().await;
        for name in [&source,&restored] {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database d JOIN pg_roles r ON r.oid=d.datdba WHERE d.datname=$1 AND r.rolname=$2)")
                .bind(name).bind(owner).fetch_one(&mut admin).await?;
            if exists { sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE \"{name}\" WITH (FORCE)"))).execute(&mut admin).await?; }
        }
        assert!(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=ANY($1))")
            .bind(vec![&source,&restored]).fetch_one(&mut admin).await?);
        admin.close().await?;
        match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
    })).await
}

fn url(original: &str, database: &str) -> Result<String> {
    let mut url = url::Url::parse(original)?;
    let pairs = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(pairs);
    url.set_path(database);
    Ok(url.to_string())
}

fn environment(original: &str) -> Result<BTreeMap<String, String>> {
    let options = PgConnectOptions::from_str(original)?;
    let mut environment = BTreeMap::from([
        ("PGHOST".into(), options.get_host().into()),
        ("PGPORT".into(), options.get_port().to_string()),
        ("PGUSER".into(), options.get_username().into()),
        ("PGSSLMODE".into(), "prefer".into()),
    ]);
    if let Some(raw) = url::Url::parse(original)?.password() {
        let encoded = format!("value={}", raw.replace('+', "%2B"));
        let password = url::form_urlencoded::parse(encoded.as_bytes())
            .next()
            .context("password decoding")?
            .1
            .into_owned();
        environment.insert("PGPASSWORD".into(), password);
    }
    Ok(environment)
}

async fn cli(
    test: &support::Scenario,
    original: &str,
    args: &[&std::ffi::OsStr],
) -> Result<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_native-ops"));
    command
        .current_dir(test.directory())
        .args(["migration"])
        .args(args)
        .envs(environment(original)?)
        .env_remove("DATABASE_URL")
        .env_remove("PGOPTIONS")
        .env_remove("PGDATABASE")
        .env_remove("PGSERVICE")
        .env_remove("PGHOSTADDR")
        .env("RUST_LOG", "error")
        .kill_on_drop(true);
    tokio::time::timeout(std::time::Duration::from_secs(90), command.output())
        .await
        .context("native migration deadline")?
        .context("running native migration CLI")
}

async fn exercise(test: &support::Scenario, source: &str, restored: &str) -> Result<()> {
    let source_url = url(&test.url, source)?;
    let database = Database::connect(&source_url).await?;
    let data = test.directory().join("data");
    let artifacts = test.directory().join("durable-artifacts");
    std::fs::create_dir_all(data.join("evidence"))?;
    std::fs::create_dir_all(data.join("exports"))?;
    let config = data.join("config.toml");
    std::fs::write(
        &config,
        "states=['OH']\ngraduation_years=[2027]\nseason=2026\n",
    )?;
    let scope = page_support::scope(&page_support::config()?)?;
    page::offline_replay(
        &database,
        &artifacts,
        &scope,
        page_support::profile(),
        page_support::capture()?,
    )
    .await?;
    seed_ledgers(&database).await?;
    let expected = fingerprint(&database).await?;
    assert_eq!(expected.len(), 13);
    assert!(expected.iter().all(|(_, count, _)| *count > 0));
    database.close().await;
    shadow::install(&source_url, source).await?;
    let archive = test.directory().join("backup.tar.gz");
    let backup = [
        "backup".as_ref(),
        "--database".as_ref(),
        source.as_ref(),
        "--data-dir".as_ref(),
        data.as_os_str(),
        "--config".as_ref(),
        config.as_os_str(),
        "--artifacts".as_ref(),
        artifacts.as_os_str(),
        "--output".as_ref(),
        archive.as_os_str(),
    ];
    let wrong_root = test.directory().join("empty-artifacts");
    std::fs::create_dir(&wrong_root)?;
    let mut wrong_backup = backup;
    *wrong_backup.get_mut(8).expect("artifacts argument") = wrong_root.as_os_str();
    let rejected = cli(test, &source_url, &wrong_backup).await?;
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("referenced durable artifact is missing")
    );
    assert!(!archive.exists());
    let output = cli(test, &source_url, &backup).await?;
    support::assert_success(&output);
    let hash = std::fs::read(&archive)?;
    assert!(!cli(test, &source_url, &backup).await?.status.success());
    assert_eq!(std::fs::read(&archive)?, hash);
    let mut source_connection = PgConnection::connect(&source_url).await?;
    sqlx::query("CREATE TABLE public.unverified_future_table(id bigint)")
        .execute(&mut source_connection)
        .await?;
    let unverified = test.directory().join("unverified.tar.gz");
    let mut rejected_backup = backup;
    *rejected_backup.last_mut().expect("output argument") = unverified.as_os_str();
    let rejected = cli(test, &source_url, &rejected_backup).await?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("public table set"));
    assert!(!unverified.exists());
    sqlx::query("DROP TABLE public.unverified_future_table")
        .execute(&mut source_connection)
        .await?;
    source_connection.close().await?;
    let destination = test.directory().join("restored");
    let restore = [
        "restore".as_ref(),
        "--archive".as_ref(),
        archive.as_os_str(),
        "--database".as_ref(),
        restored.as_ref(),
        "--data-dir".as_ref(),
        destination.as_os_str(),
        "--create-with-admin-env".as_ref(),
        "TEST_MIGRATION_ADMIN_DATABASE_URL".as_ref(),
    ];
    let output = cli(test, &source_url, &restore).await?;
    support::assert_success(&output);
    let target_url = url(&test.url, restored)?;
    let restored_db = Database::connect_for_export(&target_url).await?;
    assert_eq!(fingerprint(&restored_db).await?, expected);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM scrape_pages WHERE page_key=repeat('d',64)"
        )
        .fetch_one(restored_db.pool())
        .await?,
        "blocked"
    );
    assert!(destination.join("durable-artifacts").is_dir());
    assert!(destination.join("private.env").is_file());
    assert!(!std::fs::read_to_string(destination.join("private.env"))?.contains("password"));
    let instruction = String::from_utf8(output.stdout)?;
    assert!(
        instruction.contains("separate Restate server") && instruction.contains("durable export")
    );
    assert!(!cli(test, &source_url, &restore).await?.status.success());
    assert_eq!(fingerprint(&restored_db).await?, expected);
    restored_db.close().await;
    Ok(())
}

async fn seed_ledgers(database: &Database) -> Result<()> {
    sqlx::raw_sql("INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit','migration-fixture','Migration School','OH','https://school.test/directory'); INSERT INTO contacts(school_id,kind,role,name,email,source_key,source_url,raw) SELECT id,'coach','Head Coach','Fixture Coach','coach@school.test',repeat('a',64),'https://school.test/staff','{}'::jsonb FROM schools WHERE source_id='migration-fixture'; INSERT INTO scrape_pages(source,url,page_key,cursor,status,last_error) VALUES('milesplit','https://school.test/blocked',repeat('d',64),'{\"source\":\"milesplit\",\"kind\":\"contacts\",\"url\":\"https://school.test/blocked\",\"context\":{\"state\":\"OH\",\"school_id\":\"migration-fixture\"}}','blocked','403 retained stop'); INSERT INTO durable_wire_pacing(pacing_key) VALUES('migration-fixture'); INSERT INTO durable_wire_requests(wire_id,operation_id,attempt_id,request_sha256,occurrence,source,origin,method,state) VALUES(repeat('a',64),repeat('b',64),repeat('c',64),repeat('d',64),1,'milesplit','https://school.test','GET','intent');")
        .execute(database.pool()).await?;
    operation_receipts::with_receipt(
        database.pool(),
        "migration-fixture",
        &"e".repeat(64),
        |_| Box::pin(async { Ok(serde_json::json!({"fixture":true})) }),
    )
    .await?;
    Ok(())
}

async fn fingerprint(database: &Database) -> Result<Vec<(String, i64, String)>> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT tablename::text FROM pg_tables WHERE schemaname='public' ORDER BY tablename",
    )
    .fetch_all(database.pool())
    .await?;
    let mut values = Vec::new();
    for name in names {
        let (count,hash): (i64,String) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*),md5(COALESCE(string_agg(value,'' ORDER BY value),'')) FROM (SELECT to_jsonb(t)::text AS value FROM public.\"{name}\" t) rows")))
            .fetch_one(database.pool()).await?;
        values.push((name, count, hash));
    }
    Ok(values)
}
