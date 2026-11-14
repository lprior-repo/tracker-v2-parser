use anyhow::{Context, Result, bail, ensure};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

struct RunningChild(Option<Child>);

impl Drop for RunningChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if let Err(error) = child.kill() {
                tracing::debug!(%error, "compression child termination raced with exit");
            }
            if let Err(error) = child.wait() {
                tracing::warn!(%error, "compression child reap failed");
            }
        }
    }
}

pub(super) fn run(mut command: Command, polls: u16) -> Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear();
    let started = std::time::Instant::now();
    let budget = Duration::from_millis(u64::from(polls).saturating_mul(10));
    let mut owned = RunningChild(Some(
        command
            .spawn()
            .context("starting bounded artifact compressor")?,
    ));
    let result = (0..polls)
        .find_map(|_| {
            if started.elapsed() >= budget {
                return Some(Err(anyhow::anyhow!(
                    "artifact compression time budget reached"
                )));
            }
            let observed = owned
                .0
                .as_mut()
                .context("compression process already reaped")
                .and_then(|child| child.try_wait().context("observing artifact compressor"));
            match observed {
                Ok(None) => {
                    std::thread::sleep(
                        Duration::from_millis(10).min(budget.saturating_sub(started.elapsed())),
                    );
                    None
                }
                other => Some(other),
            }
        })
        .transpose()?;
    let Some(status) = result.flatten() else {
        bail!("artifact compression time budget reached")
    };
    drop(owned.0.take());
    ensure!(status.success(), "artifact compression process failed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn given_running_child_when_budget_expires_then_child_is_killed_and_reaped() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let pid_path = fixture.path().join("compressor.pid");
        let started = std::time::Instant::now();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "echo $$ > \"$1\"; exec /bin/sleep 5", "owned-probe"])
            .arg(&pid_path);
        let result = run(command, 200);
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(4));
        let pid = std::fs::read_to_string(pid_path)?;
        let status = Command::new("/bin/ps")
            .args(["-p", pid.trim(), "-o", "comm="])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        assert!(!status.success());
        Ok(())
    }
}
