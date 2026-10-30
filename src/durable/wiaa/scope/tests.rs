use anyhow::{Context, Result};

use super::bind;
use crate::{
    config::Config,
    database::Database,
    durable::{
        model,
        operations::OperationSpec,
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, FetchDecision, PageOutcome, ReservationDecision},
    },
    model::{PageContext, PageKind, Source, Target},
    transport::{Mode, Options, Transport},
};

fn configured() -> Result<Config> {
    Config::parse(
        "states=['WI']\n[run]\nhours=1\nmax_batches=7\ncooldown_seconds=120\n[limits]\nmax_attempts=1\n[acquisition]\nmode='public_results'\ncollect_contacts=true\n[sources.athleticnet]\nenabled=false",
    )
}

fn target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        "https://schools.wiaawi.org/Directory/School/List".into(),
        PageContext {
            state: Some("WI".into()),
            subject_id: Some("wiaa-directory-index".into()),
            ..Default::default()
        },
    )
}

fn connection(
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
    .with_http_fixture("https://schools.wiaawi.org", fixture.url.clone())?)
}

#[test]
fn wiaa_scope_retains_legacy_operation_serialization_and_applies_explicit_run_settings()
-> Result<()> {
    let config = Config::parse("states=['WI']")?;
    let operation = OperationSpec::WiaaDirectory {
        config: config.clone(),
    };
    assert_eq!(
        model::digest(&operation)?,
        "8d8bc7b280cec31906c15a9f9fa7a865beac95adb946b013a0448bd942077b40"
    );
    let encoded = serde_json::to_string(&operation)?;
    assert!(encoded.starts_with("{\"kind\":\"wiaa_directory\",\"config\":{\"acquisition\":"));
    assert_eq!(
        serde_json::to_string(&serde_json::from_str::<OperationSpec>(&encoded)?)?,
        encoded
    );
    let legacy = bind(&config, "old-binding", None, 1000)?;
    assert_eq!(
        (legacy.deadline_ms, legacy.cooldown_ms),
        (86_401_000, 60_000)
    );
    assert_eq!(
        legacy.run_generation,
        model::digest(&("old-binding", "wiaa-directory"))?
    );
    let config = configured()?;
    let standalone = bind(&config, "new-binding", None, 1000)?;
    assert_eq!(
        (standalone.deadline_ms, standalone.cooldown_ms),
        (3_601_000, 120_000)
    );
    let parent = bind(&config, "parent-binding", Some((1_801_000, 15_000)), 1000)?;
    assert_eq!(
        (parent.deadline_ms, parent.cooldown_ms),
        (1_801_000, 15_000)
    );
    assert_eq!(parent.config_sha256, model::page_config_digest(&config)?);
    for bounds in [
        (86_401_001, 60_000),
        (0, 60_000),
        (2000, 14_999),
        (2000, 3_600_001),
    ] {
        assert!(bind(&config, "invalid", Some(bounds), 1000).is_err());
    }
    assert_eq!(
        bind(&config, "expired", Some((999, 120_000)), 1000)?.deadline_ms,
        999
    );
    Ok(())
}

#[tokio::test]
async fn wiaa_run_bound_cooldown_is_used_by_actual_fetch_and_expired_bounds_make_zero_wire_requests()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = configured()?;
            let now = chrono::Utc::now().timestamp_millis();
            let root = tempfile::tempdir()?;
            let scope = bind(&config, "active", Some((now + 3_600_000, 120_000)), now)?;
            let request = page::bind_target(&database, target(), &scope).await?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &config, &request, now).await?
            else {
                anyhow::bail!("active reservation missing")
            };
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 503,
                body: "temporarily unavailable",
            })
            .await?;
            let mut transport = connection(root.path(), &fixture, &database)?;
            let started = tokio::time::Instant::now();
            let FetchDecision::Settled {
                outcome: PageOutcome::Deferred { retry_at_ms, .. },
            } = page::fetch_configured(
                &database,
                root.path(),
                &reservation,
                &mut transport,
                now,
                &config,
            )
            .await?
            else {
                anyhow::bail!("configured cooldown missing")
            };
            assert!(retry_at_ms >= now + 120_000);
            assert!(retry_at_ms <= now + 120_000 + i64::try_from(started.elapsed().as_millis())?);
            transport.close().await?;
            assert_eq!(fixture.finish().await?.len(), 1);
            let scope = bind(&config, "already-expired", Some((now - 1, 120_000)), now)?;
            let expired = page::bind_target(&database, target(), &scope).await?;
            let attempts = sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages")
                .fetch_one(pool)
                .await?;
            assert!(matches!(
                page::reserve_configured(&database, &config, &expired, now).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Exhausted { .. }
                }
            ));
            assert_eq!(
                sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages")
                    .fetch_one(pool)
                    .await?,
                attempts
            );
            let scope = bind(
                &config,
                "expires-before-fetch",
                Some((now + 1000, 120_000)),
                now,
            )?;
            let request = page::bind_target(&database, target(), &scope).await?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &config, &request, now).await?
            else {
                anyhow::bail!("expiring reservation missing")
            };
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 200,
                body: "unused",
            })
            .await?;
            let mut transport = connection(root.path(), &fixture, &database)?;
            assert!(matches!(
                page::fetch_configured(
                    &database,
                    root.path(),
                    &reservation,
                    &mut transport,
                    now + 1000,
                    &config
                )
                .await?,
                FetchDecision::Settled {
                    outcome: PageOutcome::Exhausted { .. }
                }
            ));
            transport.close().await?;
            assert_eq!(fixture.finish().await?.len(), 0);
            let saved = database
                .durable_stages(&reservation)
                .await?
                .2
                .context("expired outcome")?;
            assert!(matches!(saved, PageOutcome::Exhausted { .. }));
            Ok(())
        })
    })
    .await
}
