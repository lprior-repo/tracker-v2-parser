use super::{scope, target};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, FetchDecision, PageOutcome, ReservationDecision},
    },
    model::{Capture, PageContext, PageKind, Source, Target},
    transport::{Mode, Options, Transport},
};
use anyhow::Result;

const UNSUPPORTED: &str = "<main><h1>School directory</h1></main><script>window['api'+'Key']='private-canary-never-retain';</script>";
const CHALLENGED: &str = "<title>Verify you are human</title><script>window['api'+'Key']='private-canary-never-retain';</script>";

fn contact() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=320".into(),
        PageContext {
            state: Some("WI".into()),
            school_id: Some("14324".into()),
            school_name: Some("Oshkosh West".into()),
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn unsupported_contact_is_durably_rejected_without_retaining_body_or_repeating_get()
-> Result<()> {
    verify_http_rejection(UNSUPPORTED, false).await
}

#[tokio::test]
async fn challenged_contact_with_unsupported_script_remains_blocked_without_repeating_get()
-> Result<()> {
    verify_http_rejection(CHALLENGED, true).await
}

async fn verify_http_rejection(body: &'static str, blocked: bool) -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let root = tempfile::tempdir()?;
            let fixture = WireFixture::start(FixtureKind::Page { status: 200, body }).await?;
            let request = page::bind_target(&database, contact(), &scope()?).await?;
            let now = chrono::Utc::now().timestamp_millis();
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &request, now).await?
            else {
                anyhow::bail!("reservation")
            };
            let mut transport = Transport::new(Options {
                mode: Mode::Http,
                driver_url: None,
                profile: root.path().join("unused"),
                headless: true,
                rps: 2.0,
            })?
            .with_persistent_pacing(pool.clone())
            .with_http_fixture("https://schools.wiaawi.org", fixture.url.clone())?;
            let first =
                page::fetch(&database, root.path(), &reservation, &mut transport, now).await?;
            let FetchDecision::Settled { ref outcome } = first else {
                anyhow::bail!("unsafe body must not be retained")
            };
            assert!(if blocked {
                matches!(outcome, PageOutcome::Blocked { .. })
            } else {
                matches!(outcome, PageOutcome::Rejected { .. })
            });
            assert!(!format!("{outcome:?}").contains("private-canary"));
            let stages = database.durable_stages(&reservation).await?;
            assert!(stages.0.is_none());
            assert_eq!(stages.2, Some(outcome.clone()));
            let replay =
                page::fetch(&database, root.path(), &reservation, &mut transport, now).await?;
            transport.close().await?;
            let FetchDecision::Settled { outcome: replayed } = replay else {
                anyhow::bail!("settled replay")
            };
            assert_eq!(outcome, &replayed);
            let saved: (String, i32) =
                sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE id=$1")
                    .bind(request.binding.page_id)
                    .fetch_one(pool)
                    .await?;
            assert_eq!(
                saved,
                (if blocked { "blocked" } else { "failed" }.into(), 1)
            );
            for entry in std::fs::read_dir(root.path())? {
                let path = entry?.path();
                if path.is_file() {
                    assert!(
                        !String::from_utf8_lossy(&std::fs::read(path)?).contains("private-canary")
                    );
                }
            }
            assert_eq!(fixture.finish().await?.len(), 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn rejected_offline_contact_does_not_prevent_the_next_athlete_from_committing() -> Result<()>
{
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let root = tempfile::tempdir()?;
            let scope = scope()?;
            let rejected = page::offline_replay(
                &database,
                root.path(),
                &scope,
                contact(),
                Capture {
                    url: contact().url,
                    status: 200,
                    content_type: "text/html".into(),
                    body: UNSUPPORTED.as_bytes().to_vec(),
                },
            )
            .await?;
            assert!(matches!(rejected, PageOutcome::Rejected { .. }));
            let athlete = target();
            let completed = page::offline_replay(
                &database,
                root.path(),
                &scope,
                athlete.clone(),
                Capture {
                    url: athlete.url,
                    status: 200,
                    content_type: "text/html".into(),
                    body: include_bytes!("../../../../fixtures/milesplit/athlete-oh-15005189.html")
                        .to_vec(),
                },
            )
            .await?;
            assert!(matches!(completed, PageOutcome::Completed { .. }));
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM athletes")
                    .fetch_one(pool)
                    .await?,
                1
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_receipts")
                    .fetch_one(pool)
                    .await?,
                1
            );
            let request = page::bind_target(&database, contact(), &scope).await?;
            let replay =
                page::reserve(&database, &request, chrono::Utc::now().timestamp_millis()).await?;
            let ReservationDecision::Settled { outcome } = replay else {
                anyhow::bail!("rejected attempt reopened")
            };
            assert_eq!(outcome, rejected);
            Ok(())
        })
    })
    .await
}
