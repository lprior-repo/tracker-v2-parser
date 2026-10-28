use anyhow::{Context, Result, anyhow};
use tokio::task::JoinSet;

use super::drain::drain;

#[tokio::test]
async fn given_failed_owned_task_when_tracing_is_disabled_then_drain_reports_incomplete()
-> Result<()> {
    let _subscriber =
        tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
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
        let error = drain(&mut tasks, false)
            .await
            .err()
            .context("failed owned task must not certify successful cleanup")?;
        assert!(tasks.is_empty());
        assert_eq!(
            error.root_cause().to_string(),
            format!("controlled interception task {failed} failure")
        );
        assert!(format!("{error:#}").contains("acquisition incomplete"));
    }
    Ok(())
}
