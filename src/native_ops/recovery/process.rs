use std::{
    ffi::OsString, fs::OpenOptions, os::unix::fs::OpenOptionsExt, path::Path, process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::process::{Child, Command};

pub(super) struct Owned {
    child: Child,
    pid: Pid,
}

impl Owned {
    pub fn start(
        binary: &Path,
        args: &[OsString],
        cwd: &Path,
        log: &Path,
        database: Option<&str>,
    ) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(log)?;
        let mut command = command(binary, args, cwd, database);
        command.stdout(file.try_clone()?).stderr(file);
        Self::spawn(&mut command)
    }

    fn spawn(command: &mut Command) -> Result<Self> {
        let child = command.spawn().context("starting owned native process")?;
        let pid = Pid::from_raw(i32::try_from(
            child.id().context("native child has no PID")?,
        )?)
        .context("native child PID invalid")?;
        Ok(Self { child, pid })
    }

    pub async fn capture(
        binary: &Path,
        args: &[OsString],
        cwd: &Path,
        database: &str,
    ) -> Result<serde_json::Value> {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let output = cwd.join(format!("cli-{number}.stdout"));
        let error = cwd.join(format!("cli-{number}.stderr"));
        let stdout = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&output)?;
        let stderr = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(error)?;
        let mut command = command(binary, args, cwd, Some(database));
        command.stdout(stdout).stderr(stderr);
        let mut owned = Self::spawn(&mut command)?;
        let status = if let Ok(status) =
            tokio::time::timeout(Duration::from_mins(1), owned.child.wait()).await
        {
            status?
        } else {
            owned.stop(false).await?;
            anyhow::bail!("native CLI exceeded60second budget");
        };
        ensure!(
            status.success(),
            "native CLI failed; private stderr retained"
        );
        serde_json::from_slice(&super::io::read(&output, super::io::JSON_LIMIT)?)
            .context("decoding native CLI response")
    }
    pub fn signal(&self, signal: Signal) -> Result<()> {
        kill_process_group(self.pid, signal)?;
        Ok(())
    }

    pub async fn stop(&mut self, graceful: bool) -> Result<bool> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(status.success());
        }
        if graceful {
            self.signal(Signal::CONT)?;
            self.signal(Signal::TERM)?;
            if let Ok(status) =
                tokio::time::timeout(Duration::from_secs(30), self.child.wait()).await
            {
                return Ok(status?.success());
            }
        }
        self.signal(Signal::KILL)?;
        tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .context("owned child kill timed out")??;
        Ok(!graceful)
    }

    pub async fn ready(&mut self, port: u16) -> Result<()> {
        use futures::{StreamExt, TryStreamExt, stream};
        let deadline = super::io::deadline(Duration::from_mins(1))?;
        let ready = stream::iter(0..300)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(false, |ready, _| async move {
                if ready {
                    return Ok(true);
                }
                ensure!(
                    tokio::time::Instant::now() < deadline,
                    "owned child readiness exceeded deadline"
                );
                if tokio::time::timeout(
                    Duration::from_millis(200),
                    tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)),
                )
                .await
                .is_ok_and(|value| value.is_ok())
                {
                    return Ok(true);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(false)
            })
            .await?;
        ensure!(
            ready && self.child.try_wait()?.is_none(),
            "owned child failed before binding loopback"
        );
        Ok(())
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if self.child.id().is_some()
            && let Err(error) = self.signal(Signal::KILL)
        {
            tracing::error!(%error,"owned proof process cleanup failed");
        }
    }
}

pub(super) fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn command(binary: &Path, args: &[OsString], cwd: &Path, database: Option<&str>) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(cwd)
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null());
    command
        .env_remove("DATABASE_URL")
        .env_remove("TEST_DATABASE_URL")
        .env("RUST_LOG", "error")
        .env("NO_PROXY", "127.0.0.1,localhost");
    for (key, _) in
        std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("RESTATE_"))
    {
        command.env_remove(key);
    }
    if let Some(database) = database {
        command.env("DATABASE_URL", database);
    }
    command
}
