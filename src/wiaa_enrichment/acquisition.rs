use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};

use crate::{
    database::{Database, Job},
    model::{Capture, PageContext, PageKind, Source, Target},
    sources::wiaa::{self, DirectorySchool},
    transport::{self, Failure, Transport},
};

const LIST: &str = "https://schools.wiaawi.org/Directory/School/List";

pub(super) async fn check_stops(database: &Database) -> Result<()> {
    let stopped: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM scrape_pages p CROSS JOIN LATERAL unnest(ARRAY[p.url,p.cursor->>'access_stop_url']) AS stop(url) WHERE p.status='blocked' AND p.cursor->>'kind'='contacts' AND lower(substring(stop.url from '(?i)^https?://([^/?#]+)')) IN ('schools.wiaawi.org','schools.wiaawi.org:443','schools.wiaawi.org:80'))"
    ).fetch_one(database.pool()).await?;
    ensure!(
        !stopped,
        "WIAA contact authority has a preserved access stop requiring human review"
    );
    Ok(())
}

pub(super) async fn collect_index(
    database: &Database,
    transport: &mut Transport,
    source: Source,
) -> Result<Vec<DirectorySchool>> {
    let body = fetch(
        database,
        transport,
        Target::get(
            source,
            PageKind::Contacts,
            LIST.into(),
            PageContext::default(),
        ),
    )
    .await?;
    let requests = wiaa::listing_requests(&body)?;
    ensure!(
        requests.len() == 26,
        "WIAA index acquisition requires exactly 26 published requests"
    );
    let (_, entries) = stream::iter(requests.into_iter().map(Ok::<_, anyhow::Error>))
        .try_fold(
            (transport, BTreeMap::<String, DirectorySchool>::new()),
            |(transport, mut entries), url| async move {
                let mut target =
                    Target::get(source, PageKind::Contacts, url, PageContext::default());
                target.method = Some("POST".into());
                let body = fetch(database, transport, target).await?;
                let rows = wiaa::parse_listing(&body)?;
                rows.into_iter().try_for_each(|row| -> Result<()> {
                    if let Some(previous) = entries.insert(row.organization_id.clone(), row.clone())
                    {
                        ensure!(
                            previous == row,
                            "WIAA index contains conflicting organization identities"
                        );
                    }
                    Ok(())
                })?;
                ensure!(
                    entries.len() <= 10000,
                    "WIAA index exceeds 10000 high schools"
                );
                Ok((transport, entries))
            },
        )
        .await?;
    Ok(entries.into_values().collect())
}

async fn fetch(database: &Database, transport: &mut Transport, target: Target) -> Result<String> {
    check_stops(database).await?;
    let capture = match transport.fetch(&target).await {
        Ok(capture) => capture,
        Err(failure) => return failed(database, target, failure).await,
    };
    ensure!(
        capture.body.len() <= 8 * 1024 * 1024,
        "WIAA index response exceeds 8 MiB"
    );
    if transport::denied_capture(&capture) {
        return failed(database, target, Failure { stopped_origin: None, capture: None, blocked: true, detail: "WIAA public directory returned an access challenge, login requirement, or paywall; no alternate transport attempted".into() }).await;
    }
    String::from_utf8(capture.body).context("WIAA index response is not UTF8")
}

async fn failed(database: &Database, target: Target, failure: Failure) -> Result<String> {
    if failure.blocked {
        database
            .enqueue(std::slice::from_ref(&target), false)
            .await?;
        let id: i64 = sqlx::query_scalar("SELECT id FROM scrape_pages WHERE source=$1 AND url=$2 AND cursor=$3 ORDER BY id LIMIT 1")
            .bind(target.source.as_str()).bind(&target.url).bind(serde_json::to_value(&target)?).fetch_one(database.pool()).await?;
        ensure!(
            database.start_attempt(id, 1).await?,
            "WIAA index stop checkpoint could not record its first attempt"
        );
        let job = Job { id, target };
        let failure = Failure {
            capture: failure.capture.map(|capture| Capture {
                body: Vec::new(),
                ..capture
            }),
            ..failure
        };
        database.fail_acquisition(&job, &failure).await?;
        return Err(failure.into());
    }
    Err(failure.into())
}
