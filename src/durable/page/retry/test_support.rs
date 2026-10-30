use crate::{
    config::Config,
    database::Database,
    durable::{
        model::page_config_digest,
        page::{FailureClass, FailureRecord, PageScope},
    },
    model::{PageContext, PageKind, Source, Target},
};
use anyhow::Result;
use serde_json::Value;

pub(super) fn config(attempts: u32) -> Result<Config> {
    Config::parse(&format!(
        "states=['WI']\n[limits]\nmax_attempts={attempts}\n[acquisition]\nmode='public_results'\n[sources.athleticnet]\nenabled=false"
    ))
}

pub(super) fn scope(config: &Config, now: i64) -> Result<PageScope> {
    Ok(PageScope {
        run_generation: format!("configured-retry-{}", config.limits.max_attempts),
        config_sha256: page_config_digest(config)?,
        deadline_ms: now + 3_600_000,
        cooldown_ms: 15_000,
    })
}

pub(super) fn target(attempts: u32) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        format!("https://wi.milesplit.com/athletes/{attempts}-runner"),
        PageContext {
            state: Some("WI".into()),
            athlete_id: Some(attempts.to_string()),
            ..Default::default()
        },
    )
}

pub(super) async fn snapshot(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(d) ORDER BY operation_id) FROM durable_page_operations d),'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a))").fetch_one(database.pool()).await?)
}

pub(super) fn failure(now: i64) -> FailureRecord<'static> {
    FailureRecord {
        class: FailureClass::Transient,
        detail: "DNS lookup failed",
        capture: None,
        stopped_origin: None,
        observed_at_ms: now,
    }
}
