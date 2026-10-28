use std::sync::{Arc, atomic::AtomicBool};

use anyhow::{Context, Result, anyhow};
use tokio::task::JoinSet;

use super::Interception;

#[tokio::test]
async fn given_failed_owned_task_when_tracing_is_disabled_then_finish_reports_incomplete() -> Result<()> {
    let _subscriber = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    for failed in 0..3 {
        let mut tasks = JoinSet::new();
        for index in 0..3 {
            tasks.spawn(async move {
                if index == failed {
                    Err(anyhow!("controlled interception task {failed} failure"))
                } else {
                    Ok(index)
                }
            });
        }
        let interception = Interception {
            stop: None,
            tasks,
            blocked: Arc::new(AtomicBool::new(false)),
        };
        let error = interception.finish(false).await.err().context("failed owned task must not certify successful cleanup")?;
        assert_eq!(error.root_cause().to_string(), format!("controlled interception task {failed} failure"));
        assert!(format!("{error:#}").contains("acquisition incomplete"));
    }
    Ok(())
}
