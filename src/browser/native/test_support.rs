use std::sync::atomic::Ordering;

use anyhow::{Context, Result, anyhow, ensure};
use tokio::{sync::oneshot, time::timeout};

use super::{Actor, DEADLINE, NativeSink, status::Command};

impl Actor {
    pub(super) async fn hold(
        &mut self,
        expected: Option<u8>,
        ready: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) -> Result<()> {
        if let Some(expected) = expected {
            let (socket, _) = timeout(DEADLINE, self.listener.accept()).await??;
            let mut byte = [0; 1];
            let count = timeout(DEADLINE, socket.peek(&mut byte)).await??;
            ensure!(
                count == 1 && byte.first() == Some(&expected),
                "native test prefix mismatch"
            );
            self.accept(socket)?;
        }
        ready
            .send(())
            .map_err(|()| anyhow!("native sink pause requester cancelled"))?;
        timeout(DEADLINE, release)
            .await
            .context("native sink test pause expired")??;
        Ok(())
    }
}

impl NativeSink {
    pub fn denied_connections(&self) -> usize {
        self.status.denied.load(Ordering::Acquire)
    }

    pub fn address(&self) -> Result<std::net::SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    pub async fn pause(&self) -> Result<oneshot::Sender<()>> {
        let (ready, observed) = oneshot::channel();
        let (release, released) = oneshot::channel();
        self.status
            .control
            .send(Command::Hold {
                expected: None,
                ready,
                release: released,
            })
            .await?;
        observed.await?;
        Ok(release)
    }

    pub async fn pause_after_prefix(&self, expected: u8) -> Result<oneshot::Sender<()>> {
        let (ready, observed) = oneshot::channel();
        let (release, released) = oneshot::channel();
        self.status
            .control
            .send(Command::Hold {
                expected: Some(expected),
                ready,
                release: released,
            })
            .await?;
        observed.await?;
        Ok(release)
    }
}
