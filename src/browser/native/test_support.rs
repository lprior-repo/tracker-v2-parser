use std::sync::atomic::Ordering;

use anyhow::Result;
use tokio::sync::oneshot;

use super::{NativeSink, status::Command};

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
                ready,
                release: released,
            })
            .await?;
        observed.await?;
        Ok(release)
    }
}
