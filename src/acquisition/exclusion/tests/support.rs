use crate::{
    config::Config,
    database::Database,
    durable::{
        model::page_config_digest,
        pacing::test_support::{FixtureKind, WireFixture},
        page::{self, PageOutcome, PageRequest, PageReservation, PageScope, ReservationDecision},
    },
    model::{PageContext, PageKind, Source, Sport, Target},
    transport::{Mode, Options, Transport},
};
use anyhow::{Context, Result};
use serde_json::Value;

pub(super) fn config() -> Result<Config> {
    Config::parse(
        "states=['OH']\ngraduation_years=[2027]\n[acquisition]\nmode='public_results'\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\ntransport='http'",
    )
}

pub(super) fn api() -> Result<Target> {
    let parent = Target::get(
        Source::MileSplit,
        PageKind::Meet,
        "https://oh.milesplit.com/meets/770621-beaver-eastern-invite-2026/results".into(),
        PageContext {
            state: Some("OH".into()),
            sport: Some(Sport::CrossCountry),
            ..Default::default()
        },
    );
    crate::sources::parse(
        &parent,
        include_str!("../../../../fixtures/milesplit/meet-oh-770621-results.html"),
    )?
    .next
    .into_iter()
    .find(|target| target.url.contains("/api/v1/meets/"))
    .context("parser-generated restricted API")
}

pub(super) fn scope(config: &Config, name: &str, now: i64) -> Result<PageScope> {
    Ok(PageScope {
        run_generation: name.into(),
        config_sha256: page_config_digest(config)?,
        deadline_ms: now + 3_600_000,
        cooldown_ms: 15_000,
    })
}

pub(super) async fn snapshot(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(d) ORDER BY operation_id) FROM durable_page_operations d),'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a),'receipts',(SELECT jsonb_agg(to_jsonb(r) ORDER BY operation_id) FROM durable_page_receipts r),'wire',(SELECT jsonb_agg(to_jsonb(w) ORDER BY wire_id) FROM durable_wire_requests w))").fetch_one(database.pool()).await?)
}

pub(super) async fn checkpoint(database: &Database, id: i64) -> Result<Value> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE id=$1")
            .bind(id)
            .fetch_one(database.pool())
            .await?,
    )
}

pub(super) fn transport(
    root: &std::path::Path,
    fixture: &WireFixture,
    database: &Database,
) -> Result<Transport> {
    Ok(Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: root.join("unused"),
        headless: true,
        rps: 3.0,
    })?
    .with_persistent_pacing(database.pool().clone())
    .with_http_fixture("https://oh.milesplit.com", fixture.url.clone())?)
}

pub(super) async fn preserved_stop(
    database: &Database,
    root: &std::path::Path,
    config: &Config,
    now: i64,
) -> Result<(PageRequest, PageReservation)> {
    let target = api()?;
    let request = page::bind_target(
        database,
        target,
        &scope(config, "preserved-local-stop", now)?,
    )
    .await?;
    let ReservationDecision::Reserved { reservation } =
        page::reserve(database, &request, now).await?
    else {
        anyhow::bail!("old reservation")
    };
    let fixture = WireFixture::start(FixtureKind::Page {
        status: 403,
        body: "must remain unused",
    })
    .await?;
    let mut transport = transport(root, &fixture, database)?;
    assert!(matches!(
        page::fetch(database, root, &reservation, &mut transport, now).await?,
        page::FetchDecision::Settled {
            outcome: PageOutcome::Blocked { .. }
        }
    ));
    transport.close().await?;
    assert_eq!(fixture.finish().await?.len(), 0);
    Ok((request, *reservation))
}
