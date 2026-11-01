use std::{
    net::{Ipv4Addr, TcpListener},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};

use super::{Runtime, backup, io};

impl Runtime {
    pub async fn start_previous_worker(&mut self) -> Result<()> {
        let binary = self
            .args
            .previous_worker
            .clone()
            .context("previous worker absent")?;
        self.start_binary(&binary, "previous-worker.log").await
    }

    pub async fn stop_worker(&mut self) -> Result<()> {
        ensure!(
            self.worker
                .as_mut()
                .context("owned worker absent")?
                .stop(true)
                .await?,
            "owned worker failed graceful shutdown"
        );
        self.worker = None;
        Ok(())
    }

    pub fn rotate_worker_port(&mut self) -> Result<()> {
        ensure!(
            self.worker.is_none(),
            "worker must stop before endpoint rotation"
        );
        let selected = (0..64)
            .find_map(|_| {
                match TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .and_then(|socket| socket.local_addr())
                {
                    Ok(address) if !self.ports.contains(&address.port()) => {
                        Some(Ok(address.port()))
                    }
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .context("owned worker endpoint allocation exhausted")??;
        *self.ports.last_mut().context("worker port absent")? = selected;
        self.endpoint = format!("http://127.0.0.1:{selected}");
        Ok(())
    }

    pub async fn patch_owned(&self, route: &str) -> Result<u16> {
        let url = url::Url::parse(&self.admin)?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port() == self.ports.get(1).copied(),
            "proof control escaped owned admin endpoint"
        );
        Ok(reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?
            .patch(format!("{}{route}", self.admin))
            .send()
            .await?
            .status()
            .as_u16())
    }

    pub async fn register_fixture(&self, endpoint: &str, label: &str) -> Result<Value> {
        let url = url::Url::parse(endpoint)?;
        ensure!(
            url.scheme() == "http" && url.host_str() == Some("127.0.0.1"),
            "scheduler fixture endpoint must use IPv4 loopback"
        );
        let deadline = io::deadline(Duration::from_secs(30))?;
        let value = stream::iter(0..120)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(None, |result, _| async move {
                if result.is_some() {
                    return Ok(result);
                }
                ensure!(
                    tokio::time::Instant::now() < deadline,
                    "fixture registration exceeded 30 seconds"
                );
                let (status, value) = self
                    .http
                    .request(
                        &self.admin,
                        "/deployments",
                        Some(&json!({"uri":endpoint})),
                        None,
                    )
                    .await?;
                if matches!(status, 200 | 201) {
                    return Ok(Some(value));
                }
                ensure!(
                    matches!(status, 429 | 502 | 503 | 504),
                    "fixture registration failed HTTP {status}"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok(None)
            })
            .await?
            .context("fixture registration did not complete")?;
        io::save(&self.args.evidence.join(label), &value)?;
        Ok(value)
    }

    pub async fn restart_active(&mut self, id: &str) -> Result<Value> {
        self.worker
            .as_ref()
            .context("owned worker absent")?
            .signal(rustix::process::Signal::STOP)?;
        ensure!(
            self.server
                .as_mut()
                .context("owned server absent")?
                .stop(true)
                .await?,
            "owned server did not stop gracefully with active children"
        );
        self.server = None;
        ensure!(
            self.worker
                .as_mut()
                .context("owned worker absent")?
                .stop(false)
                .await?,
            "owned worker abrupt stop failed"
        );
        self.worker = None;
        let restored = backup::restore(&self.args.evidence, &self.config, id)?;
        self.start_server().await?;
        self.start_worker().await?;
        Ok(
            json!({"root_invocation_id":id,"worker_abruptly_terminated":true,
            "server_gracefully_stopped":true,"backup_restore":restored,
            "started_from_verified_backup_clone":true}),
        )
    }
}
