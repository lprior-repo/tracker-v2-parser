use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{
    config::Config,
    database::Database,
    durable::{
        model,
        page::{self, PageScope},
    },
    model::{PageContext, PageKind, Source, Target},
    native_ops::recovery::{io, runtime::Runtime},
};

pub(super) struct Seeded {
    pub input: Value,
    pub stops: Vec<i64>,
}

pub(super) async fn prepare(runtime: &Runtime, identity: &str) -> Result<Seeded> {
    let config = Config::parse(
        "states=['OH']\ngraduation_years=[2027,2028]\n[acquisition]\nmode='public_results'\ncollect_contacts=true\n[limits]\nrequests_per_second=3.0\nconcurrency=3\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\ntransport='http'\n[export]\nname='local-stop-recovery'\nlayout='single_sheet'\nreview_sheet=true",
    )?;
    let database = Database::connect_for_export(&runtime.database).await?;
    let scope = PageScope {
        run_generation: format!("local-stop-{identity}"),
        config_sha256: model::page_config_digest(&config)?,
        deadline_ms: chrono::Utc::now()
            .timestamp_millis()
            .checked_add(3_600_000)
            .context("local proof deadline overflow")?,
        cooldown_ms: 15_000,
        inherited_retry_campaigns: Vec::new(),
    };
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "https://oh.milesplit.com/athletes/15005189-brice-fuller".into(),
        PageContext {
            state: Some("OH".into()),
            athlete_id: Some("15005189".into()),
            sport: Some(crate::model::Sport::Outdoor),
            season: Some(2026),
            ..Default::default()
        },
    );
    let request = page::bind_target(&database, target, &scope).await?;
    database.close().await;
    let stops = stops(runtime, identity).await?;
    Ok(Seeded {
        input: json!({"request":request,"config":config,"scheduler_version":"rolling_v2"}),
        stops,
    })
}

async fn stops(runtime: &Runtime, identity: &str) -> Result<Vec<i64>> {
    let origin = "https://oh.milesplit.com";
    let url = format!("{origin}/teams/942981-proof/admin");
    let detail = format!(
        "browser interception failed: source access stopped at {origin} (HTTP None); human review required"
    );
    let contact:i64=sqlx::query_scalar("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',$1,$2,$3,'blocked',1,$4) RETURNING id")
        .bind(io::digest(format!("contact-stop-{identity}").as_bytes())).bind(&url).bind(json!({"source":"milesplit","kind":"contacts","url":url,"context":{},"access_stop_url":origin})).bind(&detail).fetch_one(&runtime.pool).await?;
    let operation = io::digest(format!("contact-operation-{identity}").as_bytes());
    let attempt = io::digest(format!("contact-attempt-{identity}").as_bytes());
    let outcome = json!({"state":"blocked","detail":detail});
    sqlx::query("INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,cycle,cycle_attempts,original_attempts,outcome) VALUES($1,$2,'local-contact-proof','{}','blocked',1,1,0,$3)")
        .bind(&operation).bind(contact).bind(&outcome).execute(&runtime.pool).await?;
    sqlx::query("INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts,state,outcome) VALUES($1,$2,1,1,1,'failed',$3)")
        .bind(&attempt).bind(&operation).bind(outcome).execute(&runtime.pool).await?;
    sqlx::query("INSERT INTO durable_wire_requests(wire_id,operation_id,attempt_id,request_sha256,occurrence,source,origin,method,state,status,finished_at) VALUES($1,$2,$3,$4,1,'milesplit',$5,'GET','headers',302,clock_timestamp())")
        .bind(io::digest(format!("wire-{identity}").as_bytes())).bind(operation).bind(attempt).bind(io::digest(url.as_bytes())).bind(origin).execute(&runtime.pool).await?;
    let derived_url = format!("{origin}/athletes/942982-proof/stats");
    let derived:i64=sqlx::query_scalar("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',$1,$2,$3,'blocked',1,$4) RETURNING id")
        .bind(io::digest(format!("derived-stop-{identity}").as_bytes())).bind(&derived_url).bind(json!({"source":"milesplit","kind":"athlete","url":derived_url,"context":{},"access_stop_url":origin})).bind(format!("source access stopped at {origin} (HTTP None); human review required")).fetch_one(&runtime.pool).await?;
    let derived_operation = io::digest(format!("derived-owner-{identity}").as_bytes());
    let derived_attempt = io::digest(format!("derived-attempt-{identity}").as_bytes());
    let derived_outcome = json!({"state":"blocked","detail":format!("source access stopped at {origin} (HTTP None); human review required")});
    sqlx::query("INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,cycle,cycle_attempts,original_attempts,outcome) VALUES($1,$2,'derived-stop-proof','{}','blocked',1,1,0,$3)").bind(&derived_operation).bind(derived).bind(&derived_outcome).execute(&runtime.pool).await?;
    sqlx::query("INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts,state,outcome) VALUES($1,$2,1,1,1,'failed',$3)").bind(derived_attempt).bind(derived_operation).bind(derived_outcome).execute(&runtime.pool).await?;
    Ok(vec![contact, derived])
}

pub(super) async fn retain(runtime: &Runtime, input: &Value, before: &Value) -> Result<()> {
    let bytes = io::read(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/milesplit/athlete-oh-15005189.html"),
        16 * 1024 * 1024,
    )?;
    let hash = io::digest(&bytes);
    let root = runtime.root.clone();
    let attempt = io::text(before, "/attempt/attempt_id")?.to_owned();
    let manifest = json!({"attempt_id":attempt,"operation_id":input.pointer("/request/binding/operation_id").context("fixture owner absent")?,"capture":{"body":{"sha256":hash,"bytes":bytes.len()},"url":io::text(input,"/request/target/url")?,"status":200,"content_type":"text/html","retained_at_ms":chrono::Utc::now().timestamp_millis()}});
    tokio::task::spawn_blocking(move || -> Result<()> {
        let blob = root.join(format!("{hash}.blob"));
        if blob.try_exists()? {
            ensure!(
                io::read(&blob, 16 * 1024 * 1024)? == bytes,
                "fixture blob differs"
            );
        } else {
            io::write(&blob, &bytes)?;
        }
        io::save(&root.join(format!("{attempt}.capture")), &manifest)
    })
    .await
    .context("fixture retained capture writer")?
}
