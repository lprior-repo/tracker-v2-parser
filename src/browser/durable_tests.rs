use std::{collections::BTreeMap, time::Duration};

use anyhow::Result;

use super::Browser;
use crate::{
    durable::pacing::{
        PersistentPacing,
        test_support::{FixtureKind, WireFixture, with_database},
    },
    model::{PageContext, PageKind, Source, Target},
};

#[tokio::test]
async fn given_a_persistent_browser_when_document_and_subresources_load_then_every_wire_send_is_paced()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let fixture = WireFixture::start(FixtureKind::Browser).await?;
            let profile = tempfile::tempdir()?;
            let mut browser = Browser::start(None, profile.path(), true)
                .await?
                .with_persistent_pacing(Some(
                    PersistentPacing::new(pool.clone())
                        .with_wire_context(&"1".repeat(64), &"2".repeat(64))?,
                ));
            browser.local_origin = Some(fixture.url.origin());
            let target = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                fixture.url.to_string(),
                PageContext::default(),
            );
            let captured = browser
                .fetch(&target, &BTreeMap::new(), Duration::from_millis(500))
                .await;
            let closed = browser.close().await;
            let arrivals = fixture.finish().await?;
            closed?;
            assert_eq!(captured?.status, 200);
            for path in ["/", "/asset", "/api"] {
                assert!(
                    arrivals.iter().any(|(actual, _)| actual == path),
                    "missing {path}: {arrivals:?}"
                );
            }
            for pair in arrivals.windows(2) {
                assert!(pair[1].1.duration_since(pair[0].1) >= Duration::from_millis(450));
            }
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM durable_wire_pacing")
                .fetch_one(pool)
                .await?;
            assert_eq!(rows, 2);
            let recorded: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM durable_wire_requests WHERE state='headers' AND status=200",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(recorded, i64::try_from(arrivals.len())?);
            Ok(())
        })
    })
    .await
}
