use crate::{
    config::Config,
    database::{Database, Job},
    durable::{
        model::page_config_digest,
        page::{self, PageRequest, PageScope},
    },
    model::{PageContext, PageKind, Source, Target},
};
use anyhow::Result;
use serde_json::Value;

pub(super) fn config(reviewed: bool) -> Result<Config> {
    Config::parse(&format!(
        "states=['WI']\n[acquisition]\nmode='public_results'\ncollect_contacts=true\nreviewed_legacy_failures={}\n[sources.athleticnet]\nenabled=false",
        if reviewed {
            "['wiaa_script_sanitization']"
        } else {
            "[]"
        }
    ))
}

pub(super) async fn request(
    database: &Database,
    config: &Config,
    address: &str,
    kind: PageKind,
    now: i64,
    generation: &str,
) -> Result<PageRequest> {
    let target = Target::get(
        Source::MileSplit,
        kind,
        address.into(),
        PageContext {
            state: Some("WI".into()),
            ..Default::default()
        },
    );
    database
        .enqueue(std::slice::from_ref(&target), false)
        .await?;
    let id = sqlx::query_scalar("SELECT id FROM scrape_pages WHERE cursor=$1")
        .bind(serde_json::to_value(&target)?)
        .fetch_one(database.pool())
        .await?;
    page::request(
        Job { id, target },
        &PageScope {
            run_generation: generation.into(),
            config_sha256: page_config_digest(config)?,
            deadline_ms: now + 3_600_000,
            cooldown_ms: 15_000,
        },
    )
}

pub(super) async fn legacy(
    database: &Database,
    request: &PageRequest,
    detail: &str,
    status: &str,
) -> Result<()> {
    sqlx::query("UPDATE scrape_pages SET status=$2,attempts=3,last_error=$3,notes=ARRAY['original evidence'] WHERE id=$1").bind(request.binding.page_id).bind(status).bind(detail).execute(database.pool()).await?;
    Ok(())
}

pub(super) async fn snapshot(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(d) ORDER BY operation_id) FROM durable_page_operations d),'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a),'receipts',(SELECT jsonb_agg(to_jsonb(r) ORDER BY operation_id) FROM durable_page_receipts r))").fetch_one(database.pool()).await?)
}
