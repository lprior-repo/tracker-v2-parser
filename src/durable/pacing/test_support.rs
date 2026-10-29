use std::{
    panic::AssertUnwindSafe,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use futures::{FutureExt, future::BoxFuture};
use sqlx::{Connection, PgConnection, PgPool, postgres::PgPoolOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
    time::Instant,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) async fn with_database<F>(scenario: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a PgPool) -> BoxFuture<'a, Result<()>>,
{
    let configured = match std::env::var("TEST_DATABASE_URL") {
        Ok(value) => value,
        Err(_) => dotenvy::from_path_iter(Path::new(env!("CARGO_MANIFEST_DIR")).join(".env"))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find_map(|(key, value)| (key == "TEST_DATABASE_URL").then_some(value))
            .context("TEST_DATABASE_URL is required for wire pacing tests")?,
    };
    let schema = format!(
        "wire_{}_{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut admin = PgConnection::connect(&configured).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut admin)
        .await?;
    let mut url = url::Url::parse(&configured)?;
    let pairs = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(pairs);
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(url.as_str())
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    let result = AssertUnwindSafe(scenario(&pool)).catch_unwind().await;
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&mut admin)
        .await?;
    admin.close().await?;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

pub(crate) enum FixtureKind {
    Redirect,
    Browser,
    Page { status: u16, body: &'static str },
}

pub(crate) struct WireFixture {
    pub url: url::Url,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<Vec<(String, Instant)>>>,
}

impl WireFixture {
    pub async fn start(kind: FixtureKind) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = url::Url::parse(&format!("http://{}", listener.local_addr()?))?;
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut arrivals = Vec::new();
            for index in 0..16 {
                let accepted = tokio::select! {
                    _ = &mut stopped => return Ok(arrivals),
                    accepted = listener.accept() => accepted,
                };
                let (mut socket, _) = accepted?;
                let mut buffer = [0_u8; 8192];
                let count = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
                    .await??;
                ensure!(count > 0, "fixture request missing");
                let request = String::from_utf8_lossy(&buffer[..count]);
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .context("fixture request has no path")?
                    .to_owned();
                arrivals.push((path.clone(), Instant::now()));
                let (status, extra, body) = match kind {
                    FixtureKind::Redirect if index == 0 => {
                        ("302 Found", "Location: /athletes/fixture\r\n", "")
                    }
                    FixtureKind::Browser if path == "/" => (
                        "200 OK",
                        "",
                        "<html><head><link rel='icon' href='data:,'></head><body><img src='/asset'><script>fetch('/api')</script></body></html>",
                    ),
                    FixtureKind::Page { status, body } => (
                        match status {
                            403 => "403 Forbidden",
                            503 => "503 Service Unavailable",
                            _ => "200 OK",
                        },
                        "",
                        body,
                    ),
                    _ => ("200 OK", "", "ok"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n{extra}Content-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await?;
            }
            anyhow::bail!("wire fixture exceeded bounded request count")
        });
        Ok(Self { url, stop, task })
    }

    pub async fn finish(self) -> Result<Vec<(String, Instant)>> {
        self.stop
            .send(())
            .map_err(|()| anyhow::anyhow!("wire fixture ended before owned shutdown"))?;
        Ok(tokio::time::timeout(Duration::from_secs(10), self.task).await???)
    }
}
