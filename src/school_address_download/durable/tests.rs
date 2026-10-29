use super::*;
use crate::durable::pacing::test_support::{FixtureKind, WireFixture, with_database};

#[test]
fn given_public_reference_redirects_when_checked_then_only_the_original_https_authority_is_admitted()
-> Result<()> {
    approved(&Url::parse(super::super::SOURCE)?)?;
    approved(&Url::parse(
        "https://raw.githubusercontent.com/another/public/path?download=1",
    )?)?;
    for address in [
        "http://raw.githubusercontent.com/file",
        "https://raw.githubusercontent.com:444/file",
        "https://raw.githubusercontent.com.evil.example/file",
        "https://user:password@raw.githubusercontent.com/file",
        "https://127.0.0.1/file",
        "https://raw.githubusercontent.com/file#fragment",
    ] {
        assert!(approved(&Url::parse(address)?).is_err(), "{address}");
    }
    Ok(())
}

#[tokio::test]
async fn given_three_persisted_reference_attempts_when_worker_restarts_then_the_budget_does_not_reset()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let operation = "a".repeat(64);
            let expected = "b".repeat(64);
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 200,
                body: "reference",
            })
            .await?;
            let client = Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .build()?;
            for ordinal in 1..=3 {
                let attempt = next_attempt(pool, &operation, &expected).await?;
                assert_eq!(attempt, attempt_identity(&operation, &expected, ordinal)?);
                let pacing =
                    PersistentPacing::new(pool.clone()).with_wire_context(&operation, &attempt)?;
                let identity =
                    WireRequest::new(Source::MileSplit, super::super::SOURCE, "GET", &[])?;
                let response = pacing
                    .send(identity, Duration::from_millis(1), async {
                        Ok(client.get(fixture.url.clone()).send().await?)
                    })
                    .await?;
                assert_eq!(response.status().as_u16(), 200);
            }
            let rejected = next_attempt(pool, &operation, &expected)
                .await
                .expect_err("persisted source budget is exhausted");
            assert!(format!("{rejected:#}").contains("exhausted three persisted source attempts"));
            assert_eq!(fixture.finish().await?.len(), 3);
            Ok(())
        })
    })
    .await
}
