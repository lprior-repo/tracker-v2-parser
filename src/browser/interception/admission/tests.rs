use std::{sync::atomic::Ordering, task::Poll};

use anyhow::{Context, Result};
use chromiumoxide::cdp::Event;
use futures::{channel::mpsc::unbounded, poll};

use super::*;

fn event(index: usize) -> Result<Arc<dyn Event>> {
    let event: EventRequestPaused = serde_json::from_value(serde_json::json!({
        "requestId": index.to_string(), "frameId": "main", "resourceType": "Script",
        "request": {
            "url": format!("https://example.com/{index}.js"), "method": "GET", "headers": {},
            "initialPriority": "Medium", "referrerPolicy": "no-referrer"
        }
    }))?;
    Ok(Arc::new(event))
}

#[tokio::test]
async fn full_pending_queue_applies_backpressure_then_delivers_every_event_in_order() -> Result<()>
{
    let (events, stream) = unbounded();
    let (pending, mut received) = mpsc::channel(32);
    let blocked = Arc::new(AtomicBool::new(false));
    let (_gate, admission) = new(EventStream::new(stream), pending, Arc::clone(&blocked));
    for index in 0..64 {
        events.unbounded_send(event(index)?)?;
    }
    drop(events);
    let mut run = Box::pin(admission.run());
    assert!(matches!(poll!(run.as_mut()), Poll::Pending));
    assert_eq!(received.len(), 32);
    let drain = async {
        let mut actual = Vec::new();
        while let Some(event) = received.recv().await {
            actual.push(event.request_id.as_ref().to_owned());
        }
        actual
    };
    let (count, actual) = tokio::join!(run, drain);
    assert_eq!(count?, 64);
    assert_eq!(
        actual,
        (0..64).map(|index| index.to_string()).collect::<Vec<_>>()
    );
    assert!(!blocked.load(Ordering::Acquire));
    Ok(())
}

#[tokio::test]
async fn quiescence_interrupts_full_queue_and_retains_incomplete_capture_signal() -> Result<()> {
    let (events, stream) = unbounded();
    let (pending, received) = mpsc::channel(1);
    let blocked = Arc::new(AtomicBool::new(false));
    let (gate, admission) = new(EventStream::new(stream), pending, Arc::clone(&blocked));
    events.unbounded_send(event(0)?)?;
    events.unbounded_send(event(1)?)?;
    let mut run = Box::pin(admission.run());
    assert!(matches!(poll!(run.as_mut()), Poll::Pending));
    let control = async {
        gate.quiesce().await?;
        assert!(blocked.load(Ordering::Acquire));
        gate.stop().await;
        Ok::<_, anyhow::Error>(())
    };
    let (count, control) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(run, control)
    })
    .await
    .context("quiescence must not wait for a full data queue")?;
    control?;
    assert_eq!(count?, 2);
    assert_eq!(received.len(), 1);
    Ok(())
}

#[tokio::test]
async fn stop_is_prioritized_over_already_ready_page_events() -> Result<()> {
    let (events, stream) = unbounded();
    let (pending, received) = mpsc::channel(1);
    let blocked = Arc::new(AtomicBool::new(false));
    let (gate, admission) = new(EventStream::new(stream), pending, Arc::clone(&blocked));
    events.unbounded_send(event(0)?)?;
    gate.stop().await;
    assert_eq!(admission.run().await?, 0);
    assert!(received.is_empty());
    assert!(blocked.load(Ordering::Acquire));
    Ok(())
}

#[tokio::test]
async fn quiescence_does_not_certify_success_with_an_already_ready_request() -> Result<()> {
    let (events, stream) = unbounded();
    let (pending, received) = mpsc::channel(1);
    let blocked = Arc::new(AtomicBool::new(false));
    let (gate, admission) = new(EventStream::new(stream), pending, Arc::clone(&blocked));
    events.unbounded_send(event(0)?)?;
    let mut quiescence = Box::pin(gate.quiesce());
    assert!(matches!(poll!(quiescence.as_mut()), Poll::Pending));
    let mut run = Box::pin(admission.run());
    assert!(matches!(poll!(run.as_mut()), Poll::Pending));
    quiescence.await?;
    assert!(blocked.load(Ordering::Acquire));
    assert!(received.is_empty());
    gate.stop().await;
    assert_eq!(run.await?, 0);
    Ok(())
}

#[tokio::test]
async fn closed_pending_queue_is_a_reported_failure() -> Result<()> {
    let (events, stream) = unbounded();
    let (pending, received) = mpsc::channel(1);
    drop(received);
    let (_gate, admission) = new(
        EventStream::new(stream),
        pending,
        Arc::new(AtomicBool::new(false)),
    );
    events.unbounded_send(event(0)?)?;
    let error = admission
        .run()
        .await
        .expect_err("closed receiver must fail acquisition");
    assert!(format!("{error:#}").contains("closed"));
    Ok(())
}

#[tokio::test]
async fn request_budget_stays_bounded_and_never_reports_partial_success() -> Result<()> {
    let (events, stream) = unbounded();
    let (pending, mut received) = mpsc::channel(32);
    let (_gate, admission) = new(
        EventStream::new(stream),
        pending,
        Arc::new(AtomicBool::new(false)),
    );
    for index in 0..=MAX_PAGE_REQUESTS {
        events.unbounded_send(event(index)?)?;
    }
    drop(events);
    let (result, count) = tokio::join!(admission.run(), async {
        let mut count = 0;
        while received.recv().await.is_some() {
            count += 1;
        }
        count
    });
    let error = result.expect_err("hard request cap must fail acquisition");
    assert!(format!("{error:#}").contains("request budget exceeded"));
    assert_eq!(count, MAX_PAGE_REQUESTS);
    Ok(())
}

#[tokio::test]
async fn production_queue_consumes_excess_burst_up_to_hard_cap_without_a_consumer() -> Result<()> {
    let (events, stream) = unbounded();
    let (pending, received) = mpsc::channel(super::super::PENDING_REQUESTS);
    let (_gate, admission) = new(
        EventStream::new(stream),
        pending,
        Arc::new(AtomicBool::new(false)),
    );
    for index in 0..=MAX_PAGE_REQUESTS {
        events.unbounded_send(event(index)?)?;
    }
    drop(events);
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), admission.run())
        .await
        .context("production admission must detect the hard cap without a pending consumer")?;
    let error = result.expect_err("excess burst must fail before a consumer is needed");
    assert!(format!("{error:#}").contains("request budget exceeded"));
    assert_eq!(received.len(), MAX_PAGE_REQUESTS);
    Ok(())
}
