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
                    stages.join_next(false).await?;
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
    stages.join_next(false).await?;
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
    stages.join_next(false).await?;
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

#[tokio::test]
async fn queued_but_unreceived_error_is_reported_after_awaiter_cancellation() -> Result<()> {
    let stages = BlockingStages::new();
    let (started, ready) = oneshot::channel();
    let mut operation = Box::pin(stages.run(move || -> Result<()> {
        started
            .send(())
            .map_err(|_| anyhow!("start receiver dropped"))?;
        Err(anyhow!("unreceived file error"))
    }));
    tokio::select! {
        biased;
        result = ready => result?,
        result = &mut operation => return Err(anyhow!("consumed result before test could cancel: {result:?}")),
    }
    stages.join_next(false).await?;
    drop(operation);
    let error = stages
        .drain()
        .await
        .expect_err("the queued error was not consumed");
    assert!(error.to_string().contains("unreceived file error"));
    Ok(())
}

#[tokio::test]
async fn artifact_failure_delivered_to_caller_does_not_fail_drain_and_closed_intake_cannot_publish()
-> Result<()> {
    use crate::durable::page::{PageFiles, artifacts};

    let stages = BlockingStages::new();
    let root = tempfile::tempdir()?;
    let files = PageFiles::new(root.path(), &stages);
    let error = artifacts::write(files, b"evidence".to_vec(), 1)
        .await
        .expect_err("artifact exceeds its byte grant");
    assert!(format!("{error:#}").contains("byte grant"));
    stages.drain().await?;
    let error = artifacts::write(files, b"evidence".to_vec(), 8)
        .await
        .expect_err("artifact writer must use the worker's closed intake");
    assert_eq!(
        error.downcast_ref::<BlockingStageUnavailable>(),
        Some(&BlockingStageUnavailable::Stopping)
    );
    assert!(std::fs::read_dir(root.path())?.next().is_none());
    Ok(())
}

#[tokio::test]
async fn blocking_panic_is_distinct_from_an_artifact_error_at_shutdown() -> Result<()> {
    let stages = BlockingStages::new();
    let result = stages
        .run(|| -> Result<()> { panic!("injected blocking panic") })
        .await;
    assert!(result.is_err());
    let error = stages
        .drain()
        .await
        .expect_err("worker panic must fail shutdown");
    let detail = error.to_string();
    assert!(detail.contains("owned blocking stage panicked"));
    assert!(detail.contains("injected blocking panic"));
    Ok(())
}

#[tokio::test]
async fn artifact_publication_waits_for_a_slot_without_exceeding_the_worker_bound() -> Result<()> {
    use crate::durable::page::{PageFiles, artifacts};

    let stages = BlockingStages::new();
    let root = tempfile::tempdir()?;
    let files = PageFiles::new(root.path(), &stages);
    let mut releases = Vec::new();
    for _ in 0..MAX_TASKS {
        let (release, wait) = mpsc::channel();
        drop(stages.state()?.tasks.spawn_blocking(move || {
            wait.recv()?;
            Ok(())
        }));
        releases.push(release);
    }
    let mut write = std::pin::pin!(artifacts::write(files, b"evidence".to_vec(), 8));
    assert!(futures::poll!(&mut write).is_pending());
    assert!(std::fs::read_dir(root.path())?.next().is_none());
    releases.pop().expect("one slot is occupied").send(())?;
    let artifact = tokio::time::timeout(std::time::Duration::from_secs(5), write).await??;
    assert!(
        root.path()
            .join(format!("{}.blob", artifact.sha256))
            .exists()
    );
    for release in releases {
        release.send(())?;
    }
    stages.drain().await?;
    assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[test]
fn shutdown_finishes_admitted_artifact_publication_after_awaiter_cancellation() -> Result<()> {
    use crate::durable::page::{PageFiles, artifacts};
    use anyhow::Context;

    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()?
        .block_on(async {
            let directory = tempfile::tempdir()?;
            for invalid_root in [false, true] {
                let root = directory
                    .path()
                    .join(if invalid_root { "file" } else { "artifacts" });
                if invalid_root {
                    std::fs::write(&root, b"not a directory")?;
                }
                let stages = BlockingStages::new();
                let (started, ready) = oneshot::channel();
                let (release, wait) = mpsc::channel();
                let hold_pool = tokio::task::spawn_blocking(move || -> Result<()> {
                    started
                        .send(())
                        .map_err(|_| anyhow!("start receiver dropped"))?;
                    wait.recv()?;
                    Ok(())
                });
                ready.await?;
                let files = PageFiles::new(&root, &stages);
                let bytes = b"evidence";
                let mut write = Box::pin(artifacts::write(files, bytes.to_vec(), bytes.len()));
                assert!(futures::poll!(&mut write).is_pending());
                drop(write);
                let mut shutdown = Box::pin(stages.drain());
                assert!(futures::poll!(&mut shutdown).is_pending());
                release.send(())?;
                hold_pool.await??;
                let disposition = shutdown.await;
                if invalid_root {
                    let failure =
                        disposition.expect_err("an unreceived I/O error must fail shutdown");
                    let expected =
                        std::fs::create_dir_all(&root).expect_err("root is still a file");
                    assert!(failure.to_string().contains(&expected.to_string()));
                    assert_eq!(std::fs::read(&root)?, b"not a directory");
                } else {
                    disposition?;
                    let artifact = std::fs::read_dir(&root)?
                        .next()
                        .context("shutdown did not publish the admitted artifact")??;
                    assert_eq!(std::fs::read(artifact.path())?, bytes);
                    assert_eq!(
                        artifact.file_name(),
                        format!("{}.blob", crate::durable::page::body_digest(bytes)).as_str()
                    );
                    assert_eq!(std::fs::read_dir(root)?.count(), 1);
                }
            }
            Ok(())
        })
}
