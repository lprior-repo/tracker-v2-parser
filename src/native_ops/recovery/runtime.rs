use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{
    RecoveryArgs, backup,
    io::{self, Http},
    process::{self, Owned},
};

pub(super) struct Runtime {
    pub args: RecoveryArgs,
    pub pool: PgPool,
    pub database: String,
    pub root: PathBuf,
    pub config: PathBuf,
    pub http: Http,
    pub admin: String,
    pub ingress: String,
    pub endpoint: String,
    ports: [u16; 4],
    server: Option<Owned>,
    worker: Option<Owned>,
}

impl Runtime {
    pub fn new(args: RecoveryArgs, pool: PgPool, database: String, identity: &str) -> Result<Self> {
        let mut chosen = BTreeSet::new();
        for _ in 0..64 {
            chosen.insert(
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
                    .local_addr()?
                    .port(),
            );
            if chosen.len() == 4 {
                break;
            }
        }
        let ports: [u16; 4] = chosen
            .into_iter()
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| anyhow::anyhow!("could not reserve four loopback ports"))?;
        let [fabric, admin, ingress, worker] = ports;
        let root = args.evidence.join("artifacts");
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        let config = args.evidence.join("restate.toml");
        let body = server_config(
            &args.evidence.join("server-state"),
            fabric,
            admin,
            ingress,
            identity,
        )?;
        io::write(&config, body.as_bytes())?;
        Ok(Self {
            args,
            pool,
            database,
            root,
            config,
            http: Http::new()?,
            admin: format!("http://127.0.0.1:{admin}"),
            ingress: format!("http://127.0.0.1:{ingress}"),
            endpoint: format!("http://127.0.0.1:{worker}"),
            ports,
            server: None,
            worker: None,
        })
    }

    pub async fn start_server(&mut self) -> Result<()> {
        let arguments = vec![
            OsString::from("--config-file"),
            self.config.as_os_str().to_owned(),
            OsString::from("--no-logo"),
        ];
        let mut child = Owned::start(
            &self.args.server,
            &arguments,
            &self.args.evidence,
            &self.args.evidence.join("server.log"),
            None,
        )?;
        child
            .ready(*self.ports.first().context("missing fabric port")?)
            .await?;
        self.server = Some(child);
        self.http.health(&self.admin, "/health").await?;
        self.http.health(&self.ingress, "/restate/health").await
    }

    pub async fn start_worker(&mut self) -> Result<()> {
        let bind = self
            .endpoint
            .strip_prefix("http://")
            .context("invalid worker endpoint")?;
        let mut arguments = process::args(&["durable", "serve", "--bind", bind, "--artifacts"]);
        arguments.extend([
            self.root.as_os_str().to_owned(),
            OsString::from("--published-workbook"),
            self.args.evidence.join("delivered.xlsx").into_os_string(),
        ]);
        let mut child = Owned::start(
            &self.args.worker,
            &arguments,
            &self.args.evidence,
            &self.args.evidence.join("worker.log"),
            Some(&self.database),
        )?;
        child
            .ready(*self.ports.last().context("missing worker port")?)
            .await?;
        self.worker = Some(child);
        Ok(())
    }

    pub async fn restart_pending(&mut self, request: &Value) -> Result<String> {
        self.worker
            .as_ref()
            .context("worker absent before interrupt")?
            .signal(rustix::process::Signal::STOP)?;
        let (id, first) = self.http.submit(&self.ingress, request).await?;
        let (duplicate, second) = self.http.submit(&self.ingress, request).await?;
        ensure!(
            id == duplicate,
            "duplicate pending exports created different invocations"
        );
        ensure!(
            self.server
                .as_mut()
                .context("server absent before restart")?
                .stop(true)
                .await?,
            "owned server did not stop gracefully"
        );
        self.server = None;
        ensure!(
            self.worker
                .as_mut()
                .context("worker absent before restart")?
                .stop(false)
                .await?,
            "owned worker kill failed"
        );
        self.worker = None;
        let restored = backup::restore(&self.args.evidence, &self.config, &id)?;
        self.start_server().await?;
        self.start_worker().await?;
        io::save(
            &self.args.evidence.join("restart-proof.json"),
            &json!({"invocation_id":id,"first":first,"duplicate":second,"worker_stopped_during_acceptance":true,"worker_abruptly_terminated":true,"server_gracefully_stopped":true,"backup_restore":restored,"started_from_verified_backup_clone":true,"same_worker_artifact_root":true}),
        )?;
        Ok(id)
    }

    pub async fn cleanup(&mut self) -> Value {
        let worker = match &mut self.worker {
            Some(child) => child.stop(true).await.is_ok_and(|value| value),
            None => true,
        };
        let server = match &mut self.server {
            Some(child) => child.stop(true).await.is_ok_and(|value| value),
            None => true,
        };
        self.worker = None;
        self.server = None;
        self.pool.close().await;
        json!({"worker_graceful":worker,"server_graceful":server})
    }

    pub async fn query(&self, statement: &str, label: &str) -> Result<Vec<Value>> {
        let (status, value) = self
            .http
            .request(
                &self.admin,
                "/query",
                Some(&json!({"query":statement})),
                None,
            )
            .await?;
        io::save(
            &self.args.evidence.join(format!("{label}.json")),
            &json!({"status":status,"response":value}),
        )?;
        ensure!(
            status == 200,
            "native Restate introspection failed HTTP {status}"
        );
        Ok(io::array(&value, "/rows")?.clone())
    }
}

pub(super) fn server_config(
    directory: &std::path::Path,
    fabric: u16,
    admin: u16,
    ingress: u16,
    identity: &str,
) -> Result<String> {
    let base = serde_json::to_string(directory)?;
    Ok(format!(
        r#"roles = ["http-ingress", "admin", "worker", "log-server", "metadata-server"]
node-name = "recovery-{identity}"
cluster-name = "recovery-{identity}"
auto-provision = true
default-num-partitions = 1
default-replication = 1
base-dir = {base}
listen-mode = "tcp"
bind-ip = "127.0.0.1"
bind-port = {fabric}
advertised-address = "http://127.0.0.1:{fabric}/"
shutdown-timeout = "10s"
disable-telemetry = true
rocksdb-total-memory-size = "256 MiB"
default-journal-retention = "90d"
default-idempotency-retention = "90d"
default-workflow-completion-retention = "180d"
log-format = "json"
log-disable-ansi-codes = true
[admin]
bind-port = {admin}
advertised-address = "http://127.0.0.1:{admin}/"
[admin.query-engine]
memory-size = "256 MiB"
[ingress]
bind-port = {ingress}
advertised-address = "http://127.0.0.1:{ingress}/"
[bifrost]
record-cache-memory-size = "128 MiB"
[worker.invoker]
memory-limit = "256 MiB"
[default-retry-policy]
max-attempts = 3
on-max-attempts = "pause"
"#
    ))
}
