use std::sync::mpsc;

use anyhow::{Result, anyhow};
use tokio::sync::oneshot;

use super::BlockingStages;

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
    assert!(
        stages
            .run(|| Ok(()))
            .await
            .err()
            .expect("intake must stop")
            .to_string()
            .contains("intake has stopped")
    );
    Ok(())
}

#[tokio::test]
async fn completed_work_is_reaped_so_more_than_eight_sequential_stages_can_run() -> Result<()> {
    let stages = BlockingStages::new();
    for expected in 0..24 {
        assert_eq!(stages.run(move || Ok(expected)).await?, expected);
    }
    stages.drain().await?;
    assert_eq!(stages.state()?.tasks.len(), 0);
    Ok(())
}

#[tokio::test]
async fn ninth_stage_is_rejected_until_owned_stages_finish() -> Result<()> {
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
    let error = stages
        .run(|| Ok(()))
        .await
        .err()
        .expect("ninth stage must be rejected");
    assert!(error.to_string().contains("capacity of 8"));
    senders.into_iter().try_for_each(|sender| sender.send(()))?;
    for result in futures::future::join_all(runs).await {
        result?;
    }
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
