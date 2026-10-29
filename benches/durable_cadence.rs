use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, ensure};
use futures::{StreamExt, stream};
use sqlx::{Connection, PgConnection, PgPool, postgres::PgPoolOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
    time::Instant,
};
use track_scrapper_v2::{
    durable::pacing::{PersistentPacing, WireRequest},
    model::Source,
};

const REQUESTS: u32 = 4;
const INTERVAL: Duration = Duration::from_micros(333_333);
const HEADER_DELAY: Duration = Duration::from_millis(300);

struct Fixture {
    url: url::Url,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<Vec<Instant>>>,
}

impl Fixture {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = url::Url::parse(&format!("http://{}", listener.local_addr()?))?;
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut arrivals = Vec::new();
            for _ in 0..=REQUESTS {
                let accepted = tokio::select! {
                    _ = &mut stopped => return Ok(arrivals),
                    value = listener.accept() => value,
                };
                let (mut socket, _) = accepted?;
                let mut buffer = [0_u8; 8192];
                let bytes = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
                    .await??;
                ensure!(bytes > 0, "benchmark request missing");
                arrivals.push(Instant::now());
                tokio::time::sleep(HEADER_DELAY).await;
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await?;
            }
            Err(anyhow!("benchmark fixture exceeded request bound"))
        });
        Ok(Self { url, stop, task })
    }

    async fn finish(mut self) -> Result<Vec<Instant>> {
        let signalled = self.stop.send(());
        let joined = match tokio::time::timeout(Duration::from_secs(5), &mut self.task).await {
            Ok(joined) => joined,
            Err(error) => {
                self.task.abort();
                let drained = self.task.await;
                return Err(anyhow!(
                    "fixture shutdown timed out: {error}; drained={:?}",
                    drained.err()
                ));
            }
        };
        let arrivals = joined.context("joining owned benchmark fixture")??;
        signalled.map_err(|()| anyhow!("fixture stopped before owned shutdown"))?;
        Ok(arrivals)
    }
}

struct Measurement {
    span: Duration,
    min_gap: Duration,
    max_gap: Duration,
    rps: f64,
}

fn measure(arrivals: &[Instant]) -> Result<Measurement> {
    ensure!(
        arrivals.len() == usize::try_from(REQUESTS)?,
        "benchmark request count mismatch"
    );
    let first = arrivals.first().context("first arrival")?;
    let last = arrivals.last().context("last arrival")?;
    let gaps = arrivals
        .windows(2)
        .filter_map(|pair| {
            pair.first()
                .zip(pair.get(1))
                .map(|(first, last)| last.duration_since(*first))
        })
        .collect::<Vec<_>>();
    let min_gap = *gaps.iter().min().context("minimum arrival gap")?;
    let max_gap = *gaps.iter().max().context("maximum arrival gap")?;
    ensure!(
        min_gap >= INTERVAL.saturating_sub(Duration::from_millis(10)),
        "benchmark observed a rate-limit burst: {min_gap:?}"
    );
    let span = last.duration_since(*first);
    ensure!(!span.is_zero(), "benchmark arrival span is zero");
    Ok(Measurement {
        span,
        min_gap,
        max_gap,
        rps: f64::from(REQUESTS - 1) / span.as_secs_f64(),
    })
}

async fn scenario(pool: &PgPool, starts: bool) -> Result<Measurement> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let fixture = Fixture::start().await?;
    let responses = stream::iter(0..REQUESTS)
        .map(|index| {
            let client = &client;
            let address = fixture.url.clone();
            async move {
                let gate = PersistentPacing::new(pool.clone());
                let gate = if starts {
                    gate.with_start_spacing()
                } else {
                    gate
                };
                let operation = format!("{:064x}", index + if starts { 100 } else { 1 });
                let gate = gate
                    .with_wire_context(&operation, &"b".repeat(64))?
                    .with_wire_context(&operation, &"b".repeat(64))?;
                let wire = WireRequest::new(
                    Source::MileSplit,
                    "https://wi.milesplit.com/athletes/benchmark",
                    "GET",
                    b"",
                )?;
                gate.send(wire, INTERVAL, async {
                    Ok(client.get(address).send().await?)
                })
                .await
            }
        })
        .buffer_unordered(3)
        .collect::<Vec<_>>()
        .await;
    let arrivals = fixture.finish().await;
    let requests = responses
        .into_iter()
        .try_for_each(|response| -> Result<()> {
            ensure!(
                response?.status().as_u16() == 200,
                "benchmark HTTP status differs"
            );
            Ok(())
        });
    match (requests, arrivals) {
        (Ok(()), Ok(arrivals)) => measure(&arrivals),
        (Err(error), Ok(_)) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("fixture cleanup failed: {cleanup:#}")))
        }
    }
}

async fn compare(pool: &PgPool) -> Result<()> {
    let baseline = scenario(pool, false).await?;
    let starts = scenario(pool, true).await?;
    for (mode, value) in [("completed_headers", &baseline), ("start_spacing", &starts)] {
        println!(
            "mode={mode} requests={REQUESTS} concurrency=3 interval_us={} header_delay_ms={} span_ms={:.3} min_gap_ms={:.3} max_gap_ms={:.3} actual_rps={:.4}",
            INTERVAL.as_micros(),
            HEADER_DELAY.as_millis(),
            value.span.as_secs_f64() * 1000.0,
            value.min_gap.as_secs_f64() * 1000.0,
            value.max_gap.as_secs_f64() * 1000.0,
            value.rps
        );
    }
    ensure!(starts.rps >= 2.5, "start spacing did not reach 2.5 RPS");
    let target = INTERVAL.as_secs_f64().recip();
    ensure!(
        starts.rps >= target * 0.90,
        "start spacing regressed more than 10% from configured cadence"
    );
    ensure!(
        starts.rps >= baseline.rps * 0.90,
        "start spacing regressed more than 10% from baseline"
    );
    Ok(())
}

fn configured_url() -> Result<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(value) => Ok(value),
        Err(_) => dotenvy::from_path_iter(Path::new(env!("CARGO_MANIFEST_DIR")).join(".env"))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find_map(|(key, value)| (key == "TEST_DATABASE_URL").then_some(value))
            .context("TEST_DATABASE_URL is required for the isolated benchmark schema"),
    }
}

async fn benchmark() -> Result<()> {
    let configured = configured_url()?;
    let schema = format!(
        "bench_cadence_{}_{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let mut admin = PgConnection::connect(&configured).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut admin)
        .await?;
    let result = async {
        let mut url = url::Url::parse(&configured)?;
        let pairs = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        let mut options = pairs
            .iter()
            .filter(|(key, _)| key == "options")
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        options.push_str(&format!(" -csearch_path={schema}"));
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(
                pairs
                    .iter()
                    .filter(|(key, _)| key != "options")
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .append_pair("options", &options);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(url.as_str())
            .await?;
        let measured = async {
            sqlx::migrate!("./migrations").run(&pool).await?;
            compare(&pool).await
        }
        .await;
        pool.close().await;
        measured
    }
    .await;
    let cleanup = sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&mut admin)
        .await;
    let closed = admin.close().await;
    match (result, cleanup, closed) {
        (Ok(()), Ok(_), Ok(())) => Ok(()),
        (result, cleanup, closed) => Err(anyhow!(
            "benchmark failed or cleanup incomplete: measurement={:?}; schema_cleanup={:?}; connection_close={:?}",
            result.err(),
            cleanup.err(),
            closed.err()
        )),
    }
}

fn main() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(benchmark())
}
