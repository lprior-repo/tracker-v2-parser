use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::{cdp::browser_protocol::fetch::EventRequestPaused, listeners::EventStream};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use super::MAX_PAGE_REQUESTS;

enum Command {
    Quiesce(oneshot::Sender<()>),
    Stop,
}

pub(super) struct Gate {
    commands: mpsc::Sender<Command>,
}

pub(super) struct Admission {
    events: EventStream<EventRequestPaused>,
    pending: Option<mpsc::Sender<Arc<EventRequestPaused>>>,
    commands: mpsc::Receiver<Command>,
    blocked: Arc<AtomicBool>,
}

pub(super) fn new(
    events: EventStream<EventRequestPaused>,
    pending: mpsc::Sender<Arc<EventRequestPaused>>,
    blocked: Arc<AtomicBool>,
) -> (Gate, Admission) {
    let (commands, receiver) = mpsc::channel(1);
    (
        Gate { commands },
        Admission {
            events,
            pending: Some(pending),
            commands: receiver,
            blocked,
        },
    )
}

impl Gate {
    pub async fn quiesce(&self) -> Result<()> {
        let (reply, received) = oneshot::channel();
        self.commands
            .send(Command::Quiesce(reply))
            .await
            .context("browser acquisition incomplete: admission ended before quiescence")?;
        received
            .await
            .context("browser acquisition incomplete: admission did not acknowledge quiescence")
    }

    pub async fn stop(&self) {
        if self.commands.send(Command::Stop).await.is_err() {
            tracing::debug!("browser admission already ended; owned result will be drained");
        }
    }
}

impl Admission {
    pub async fn run(mut self) -> Result<usize> {
        let mut count = 0_usize;
        for _ in 0..MAX_PAGE_REQUESTS.saturating_add(4) {
            tokio::select! {
                biased;
                event = self.events.next() => {
                    let Some(event) = event else { return Ok(count); };
                    ensure!(count < MAX_PAGE_REQUESTS, "browser acquisition incomplete: request budget exceeded");
                    count = count.saturating_add(1);
                    if let Some(pending) = &self.pending {
                        pending.try_send(event).context("browser request admission budget exceeded")?;
                    } else {
                        self.blocked.store(true, Ordering::Release);
                    }
                }
                command = self.commands.recv() => match command.context("browser admission owner abandoned its guard")? {
                    Command::Quiesce(reply) => {
                        drop(self.pending.take());
                        reply.send(()).map_err(|()| anyhow!("browser quiescence owner disappeared"))?;
                    }
                    Command::Stop => return Ok(count),
                }
            }
        }
        Err(anyhow!(
            "browser acquisition incomplete: admission control budget exceeded"
        ))
    }
}
