use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};

use crate::model::{PageContext, PageKind, Source, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Case {
    Healthy,
    Malformed,
    Unavailable,
    Truncated,
    Timeout,
}

impl Case {
    pub(super) fn for_index(index: usize, slow: bool) -> Self {
        match index % if slow { 5 } else { 4 } {
            0 => Self::Healthy,
            1 => Self::Malformed,
            2 => Self::Unavailable,
            3 => Self::Truncated,
            _ => Self::Timeout,
        }
    }
}

pub(super) fn target(index: usize) -> Target {
    let id = 90_000_000 + index;
    Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        format!("https://oh.milesplit.com/athletes/{id}-cadence-fixture"),
        PageContext {
            state: Some("OH".into()),
            athlete_id: Some(id.to_string()),
            ..PageContext::default()
        },
    )
}

fn body(index: usize, malformed: bool) -> String {
    let url = target(index).url;
    let script = if malformed {
        "<script type='application/ld+json'>{broken}</script>"
    } else {
        ""
    };
    format!(
        "<html><head><link rel='canonical' href='{url}'>{script}</head><body><h1 id='athleteName'>Cadence Fixture {index}</h1><div class='athlete-info'><span class='city-state'>Columbus, OH</span><span class='grad-year'>Class of 2028</span></div><div id='progression'></div></body></html>"
    )
}

pub(super) struct Fixture {
    url: url::Url,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<Vec<(usize, usize, String)>>>,
}

impl Fixture {
    pub(super) async fn start(count: usize, slow: bool) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = url::Url::parse(&format!("http://{}", listener.local_addr()?))?;
        let attempts: Arc<Vec<AtomicUsize>> =
            Arc::new((0..count).map(|_| AtomicUsize::new(0)).collect());
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            let mut observations = Vec::new();
            let mut accepted = 0;
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    joined = tasks.join_next(), if !tasks.is_empty() => {
                        observations.push(joined.context("fixture child absent")???)
                    }
                    connected = listener.accept() => {
                        let (socket, _) = connected?;
                        accepted += 1;
                        ensure!(accepted <= count * 3, "fixture exceeded bounded request count");
                        let attempts = attempts.clone();
                        drop(tasks.spawn(async move { serve(socket, attempts, slow).await }));
                    }
                }
            }
            while let Some(result) = tasks.join_next().await {
                observations.push(result??);
            }
            observations.sort_unstable();
            Ok(observations)
        });
        Ok(Self { url, stop, task })
    }

    pub(super) fn address(&self, index: usize) -> Result<url::Url> {
        Ok(self.url.join(&format!("/{index}"))?)
    }

    pub(super) async fn finish(mut self) -> Result<Vec<(usize, usize, String)>> {
        let signal = self.stop.send(());
        let result = match tokio::time::timeout(Duration::from_secs(10), &mut self.task).await {
            Ok(result) => result,
            Err(error) => {
                self.task.abort();
                let drained = self.task.await;
                anyhow::bail!("fixture shutdown timed out: {error}; task={drained:?}")
            }
        };
        let result = result.context("joining fixture")??;
        signal.map_err(|()| anyhow::anyhow!("fixture ended before shutdown"))?;
        Ok(result)
    }
}

async fn serve(
    mut socket: TcpStream,
    attempts: Arc<Vec<AtomicUsize>>,
    slow: bool,
) -> Result<(usize, usize, String)> {
    let mut bytes = [0u8; 8192];
    let size = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut bytes)).await??;
    ensure!(size > 0, "empty fixture request");
    let text = std::str::from_utf8(&bytes[..size])?;
    let index: usize = text
        .split_whitespace()
        .nth(1)
        .context("fixture path")?
        .trim_start_matches('/')
        .parse()?;
    let attempt = attempts
        .get(index)
        .context("fixture index")?
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    ensure!(attempt <= 3, "fixture page exceeded retry budget");
    let case = Case::for_index(index, slow);
    if case == Case::Timeout {
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n").await?;
        tokio::time::sleep(Duration::from_secs(9)).await;
        return Ok((index, attempt, "body_timeout".into()));
    }
    let body = body(index, case == Case::Malformed);
    let (status, length, response_body, label) = match (case, attempt) {
        (Case::Unavailable, 1) => ("503 Service Unavailable", 9, "temporary".to_owned(), "503"),
        (Case::Truncated, 1) => ("200 OK", body.len() + 10, body, "truncated"),
        (Case::Malformed, _) => ("200 OK", body.len(), body, "malformed"),
        _ => ("200 OK", body.len(), body, "healthy"),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{response_body}"
    );
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await?;
    Ok((index, attempt, label.into()))
}
