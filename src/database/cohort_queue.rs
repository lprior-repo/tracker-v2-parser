use anyhow::Result;

use crate::model::Source;

use super::{Database, Job, JobClass, queue};

impl Database {
    #[tracing::instrument(skip(self, states, years))]
    pub async fn next_job_for_cohort(
        &self,
        source: Option<Source>,
        max_attempts: u32,
        preferred: JobClass,
        states: &[String],
        years: &[i32],
    ) -> Result<Option<Job>> {
        queue::next_for_cohort(&self.pool, source, max_attempts, preferred, states, years).await
    }
}
