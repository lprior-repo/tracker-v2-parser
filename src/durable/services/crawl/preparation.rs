use anyhow::Result;

use crate::{
    database::Database,
    durable::model::{CrawlPhase, CrawlRequest},
};

#[tracing::instrument(skip_all)]
pub(super) async fn run(database: &Database, request: &CrawlRequest) -> Result<()> {
    crate::runner::durable::bootstrap(database, &request.config).await?;
    if matches!(
        request.phase,
        CrawlPhase::SchoolChains | CrawlPhase::PublicResults
    ) {
        database
            .prepare_profile_coverage(&request.config, &request.page_scope()?)
            .await?;
    }
    Ok(())
}
