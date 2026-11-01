use anyhow::Result;
use sqlx::PgConnection;

use crate::model::{Capture, ParsedPage, Target};

use super::performance::{Stage, Timings};
use super::{contacts, identity, queue, results, schools, unassigned};

#[tracing::instrument(skip(connection, target, response, page))]
pub(super) fn persist<'a>(
    connection: &'a mut PgConnection,
    job_id: Option<i64>,
    target: &'a Target,
    response: &'a Capture,
    page: &'a ParsedPage,
) -> futures::future::BoxFuture<'a, Result<i64>> {
    Box::pin(persist_inner(connection, job_id, target, response, page))
}

async fn persist_inner(
    connection: &mut PgConnection,
    job_id: Option<i64>,
    target: &Target,
    response: &Capture,
    page: &ParsedPage,
) -> Result<i64> {
    let mut timings = Timings::new(job_id, "page_domain");
    timings
        .measure(
            Stage::PrepareReplay,
            results::prepare_replay(connection, target, response, page),
        )
        .await?;
    let revision_notes = save_domain(connection, target, page, &response.url, &mut timings).await?;
    timings
        .measure(
            Stage::Enqueue,
            queue::enqueue(connection, &page.next, false),
        )
        .await?;
    let id = timings
        .measure(
            Stage::Checkpoint,
            queue::succeed(connection, job_id, target),
        )
        .await?;
    let notes = page
        .notes
        .iter()
        .chain(&revision_notes)
        .map(String::as_str)
        .collect::<Vec<_>>();
    sqlx::query("UPDATE scrape_pages SET notes=$2 WHERE id=$1")
        .bind(id)
        .bind(notes)
        .execute(connection)
        .await?;
    Ok(id)
}

#[tracing::instrument(skip(connection, target, page, url, timings))]
async fn save_domain(
    connection: &mut PgConnection,
    target: &Target,
    page: &ParsedPage,
    url: &str,
    timings: &mut Timings,
) -> Result<Vec<String>> {
    save_profiles(connection, target, page, url, timings).await?;
    save_results(connection, target, page, url, timings).await?;
    timings
        .measure(
            Stage::RevisionReview,
            results::review_revisions(connection, target.source, page),
        )
        .await
}

#[tracing::instrument(skip(connection, target, page, url, timings))]
async fn save_profiles(
    connection: &mut PgConnection,
    target: &Target,
    page: &ParsedPage,
    url: &str,
    timings: &mut Timings,
) -> Result<()> {
    timings
        .measure(
            Stage::ContactReview,
            contacts::review_office(connection, target, url),
        )
        .await?;
    timings
        .measure(
            Stage::Schools,
            schools::save_all(connection, target, &page.schools, url),
        )
        .await?;
    timings
        .measure(
            Stage::SchoolConflicts,
            identity::review_school_conflicts(connection, target.source, &page.schools),
        )
        .await?;
    timings
        .measure(
            Stage::Profiles,
            identity::save_all(connection, target.source, &page.athletes),
        )
        .await
}

#[tracing::instrument(skip(connection, target, page, url, timings))]
async fn save_results(
    connection: &mut PgConnection,
    target: &Target,
    page: &ParsedPage,
    url: &str,
    timings: &mut Timings,
) -> Result<()> {
    timings
        .measure(
            Stage::Results,
            results::save_all(connection, target.source, &page.performances, url),
        )
        .await?;
    timings
        .measure(
            Stage::Unassigned,
            unassigned::save_all(connection, target.source, &page.unassigned_results, url),
        )
        .await?;
    timings
        .measure(
            Stage::Contacts,
            contacts::save_all(connection, target, &page.coaches, url),
        )
        .await
}
