use anyhow::Result;
use sqlx::PgConnection;

use crate::model::{Capture, ParsedPage, Target};

use super::{contacts, identity, queue, results, schools, unassigned};

#[tracing::instrument(skip(connection, target, response, page))]
pub(super) async fn persist(
    connection: &mut PgConnection,
    job_id: Option<i64>,
    target: &Target,
    response: &Capture,
    page: &ParsedPage,
) -> Result<i64> {
    save_domain(connection, target, page, &response.url).await?;
    queue::enqueue(connection, &page.next, false).await?;
    let id = queue::succeed(connection, job_id, target).await?;
    sqlx::query("UPDATE scrape_pages SET notes=$2 WHERE id=$1")
        .bind(id)
        .bind(&page.notes)
        .execute(connection)
        .await?;
    Ok(id)
}

#[tracing::instrument(skip(connection, target, page))]
async fn save_domain(
    connection: &mut PgConnection,
    target: &Target,
    page: &ParsedPage,
    url: &str,
) -> Result<()> {
    schools::save_all(connection, target.source, &page.schools, url).await?;
    identity::save_all(connection, target.source, &page.athletes).await?;
    save_results(connection, target, page, url).await
}

#[tracing::instrument(skip(connection, target, page))]
async fn save_results(
    connection: &mut PgConnection,
    target: &Target,
    page: &ParsedPage,
    url: &str,
) -> Result<()> {
    results::save_all(connection, target.source, &page.performances, url).await?;
    unassigned::save_all(connection, target.source, &page.unassigned_results, url).await?;
    contacts::save_all(connection, target.source, &page.coaches, url).await
}
