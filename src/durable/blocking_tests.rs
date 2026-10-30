use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

use anyhow::{Result, anyhow};
use tokio::sync::oneshot;

use super::{BlockingStageUnavailable, BlockingStages, MAX_TASKS};

#[tokio::test]
async fn cancelled_awaiter_keeps_blocking_file_write_owned_until_drain_finishes() -> Result<()> {
    let stages = BlockingStages::new();
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("completed.txt");
    let output = path.clone();
    let (started, ready) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let mut operation = Box::pin(stages.run(move || {
        started
            .send(())
            .map_err(|_| anyhow!("test start receiver dropped"))?;
        wait.recv()?;
        std::fs::write(&output, b"completed after await cancellation")?;
        Ok(())
    }));
    tokio::select! {
        result = &mut operation => return Err(anyhow!("stage completed before release: {result:?}")),
        result = ready => result?,
    }
    drop(operation);
    assert_eq!(stages.state()?.tasks.len(), 1);
    assert!(!path.exists());
    release.send(())?;
    stages.drain().await?;
    assert_eq!(std::fs::read(&path)?, b"completed after await cancellation");
    assert_eq!(stages.state()?.tasks.len(), 0);
    assert_eq!(
        stages
            .run(|| Ok(()))
            .await
            .err()
            .expect("intake must stop")
            .downcast_ref::<BlockingStageUnavailable>(),
        Some(&BlockingStageUnavailable::Stopping)
    );
    Ok(())
}

#[tokio::test]
async fn completed_work_is_reaped_so_more_than_eight_sequential_stages_can_run() -> Result<()> {
    let stages = BlockingStages::new();
    for expected in 0..1024 {
        let mut completed = false;
        for _ in 0..=MAX_TASKS {
            match stages.run(move || Ok(expected)).await {
                Ok(observed) => {
                    assert_eq!(observed, expected);
                    completed = true;
                    break;
                }
                Err(error)
                    if error.downcast_ref::<BlockingStageUnavailable>()
                        == Some(&BlockingStageUnavailable::Busy) =>
                {
                    stages.join_next().await?;
                }
                Err(error) => return Err(error),
            }
            assert!(stages.state()?.tasks.len() <= MAX_TASKS);
        }
        assert!(completed, "stage {expected} did not complete after reaping");
    }
    stages.drain().await?;
    assert_eq!(stages.state()?.tasks.len(), 0);
    Ok(())
}

#[tokio::test]
async fn saturation_has_bounded_owned_tasks_and_retry_succeeds_after_reaping() -> Result<()> {
    use std::{future::Future, task::Poll};
    let stages = BlockingStages::new();
    let (senders, mut runs): (Vec<_>, Vec<_>) = (0..8)
        .map(|_| {
            let (sender, receiver) = mpsc::channel();
            (
                sender,
                Box::pin(stages.run(move || {
                    receiver.recv()?;
                    Ok(())
                })),
            )
        })
        .unzip();
    futures::future::poll_fn(|context| {
        runs.iter_mut()
            .for_each(|run| assert!(run.as_mut().poll(context).is_pending()));
        Poll::Ready(())
    })
    .await;
    assert_eq!(stages.state()?.tasks.len(), 8);
    let rejected_executions = Arc::new(AtomicUsize::new(0));
    for _ in 0..16 {
        let counter = Arc::clone(&rejected_executions);
        let error = stages
            .run(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
            .err()
            .expect("saturated stage must be deferred");
        assert_eq!(
            error.downcast_ref::<BlockingStageUnavailable>(),
            Some(&BlockingStageUnavailable::Busy)
        );
        assert_eq!(stages.state()?.tasks.len(), MAX_TASKS);
    }
    assert_eq!(rejected_executions.load(Ordering::SeqCst), 0);
    senders.into_iter().try_for_each(|sender| sender.send(()))?;
    for result in futures::future::join_all(runs).await {
        result?;
    }
    stages.join_next().await?;
    assert_eq!(
        stages.run(|| Ok("retried successfully")).await?,
        "retried successfully"
    );
    stages.drain().await?;
    Ok(())
}

#[tokio::test]
async fn delivered_results_with_unfinished_handles_defer_without_running_new_work() -> Result<()> {
    let stages = BlockingStages::new();
    let mut releases = Vec::new();
    let mut delivered = Vec::new();
    for _ in 0..MAX_TASKS {
        let (result, received) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        drop(stages.state()?.tasks.spawn_blocking(move || {
            result
                .send(())
                .map_err(|_| anyhow!("result receiver dropped"))?;
            wait.recv()?;
            Ok(())
        }));
        releases.push(release);
        delivered.push(received);
    }
    for result in futures::future::join_all(delivered).await {
        result?;
    }
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("must-not-exist.txt");
    let output = path.clone();
    let error = stages
        .run(move || {
            std::fs::write(output, b"unexpected work")?;
            Ok(())
        })
        .await
        .err()
        .expect("unfinished task handles must retain ownership");
    assert_eq!(
        error.downcast_ref::<BlockingStageUnavailable>(),
        Some(&BlockingStageUnavailable::Busy)
    );
    assert!(!path.exists());
    assert_eq!(stages.state()?.tasks.len(), MAX_TASKS);
    releases
        .into_iter()
        .try_for_each(|release| release.send(()))?;
    stages.join_next().await?;
    let output = path.clone();
    stages
        .run(move || {
            std::fs::write(output, b"retried after completion")?;
            Ok(())
        })
        .await?;
    stages.drain().await?;
    assert_eq!(std::fs::read(path)?, b"retried after completion");
    Ok(())
}

#[tokio::test]
async fn cancelled_unpolled_admission_retains_no_task_or_effect() -> Result<()> {
    let stages = BlockingStages::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let operation_counter = Arc::clone(&executions);
    let operation = stages.run(move || {
        operation_counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    drop(operation);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(Arc::strong_count(&executions), 1);
    assert_eq!(stages.state()?.tasks.len(), 0);
    stages.drain().await?;
    Ok(())
}

#[tokio::test]
async fn dropped_error_receiver_is_reported_by_owned_drain() -> Result<()> {
    let stages = BlockingStages::new();
    let (started, ready) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let mut operation = Box::pin(stages.run(move || -> Result<()> {
        started
            .send(())
            .map_err(|_| anyhow!("test start receiver dropped"))?;
        wait.recv()?;
        Err(anyhow!("injected blocking stage failure"))
    }));
    tokio::select! {
        result = &mut operation => return Err(anyhow!("stage completed before release: {result:?}")),
        result = ready => result?,
    }
    drop(operation);
    release.send(())?;
    let error = stages
        .drain()
        .await
        .err()
        .expect("failed stage must be reported");
    assert!(
        error
            .to_string()
            .contains("injected blocking stage failure")
    );
    assert_eq!(stages.state()?.tasks.len(), 0);
    Ok(())
}

#[tokio::test]
async fn cancelled_drain_keeps_registry_ownership_until_a_later_drain() -> Result<()> {
    let stages = BlockingStages::new();
    let (started, ready) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let mut operation = Box::pin(stages.run(move || {
        started
            .send(())
            .map_err(|_| anyhow!("test start receiver dropped"))?;
        wait.recv()?;
        Ok(())
    }));
    tokio::select! {
        result = &mut operation => return Err(anyhow!("stage completed before release: {result:?}")),
        result = ready => result?,
    }
    drop(operation);
    let mut drain = Box::pin(stages.drain());
    tokio::select! {
        result = &mut drain => return Err(anyhow!("drain completed before release: {result:?}")),
        () = tokio::task::yield_now() => {},
    }
    drop(drain);
    assert_eq!(stages.state()?.tasks.len(), 1);
    release.send(())?;
    stages.drain().await?;
    assert_eq!(stages.state()?.tasks.len(), 0);
    Ok(())
}
