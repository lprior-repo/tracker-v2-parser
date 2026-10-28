mod observe;
mod routing;

use std::{
    collections::BTreeMap,
    net::TcpListener as StdListener,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use futures::{
    StreamExt, TryStreamExt,
    stream::{self, FuturesUnordered},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::timeout,
};
use tracing::Instrument;

use observe::{Observation, PendingSocket};

const MAX_CONNECTIONS: usize = 512;
const MAX_COMMANDS: usize = 4096;
const DEADLINE: Duration = Duration::from_secs(5);
const ROUTE_DEADLINE: Duration = Duration::from_secs(3);

enum Command {
    #[cfg(test)]
    Begin {
        local_origins: Vec<url::Origin>,
        reply: oneshot::Sender<Result<usize>>,
    },
    Drain(oneshot::Sender<Result<usize>>),
    Stop,
    #[cfg(test)]
    Hold {
        ready: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
}

pub(super) struct NativeStatus {
    accepted: AtomicUsize,
    denied: AtomicUsize,
    probes: AtomicUsize,
    rejected_connects: AtomicUsize,
    stopped: AtomicBool,
    control: mpsc::Sender<Command>,
}

impl NativeStatus {
    #[cfg(test)]
    pub async fn begin(&self, local_origins: Vec<url::Origin>) -> Result<usize> {
        self.request(|reply| Command::Begin {
            local_origins,
            reply,
        })
        .await
    }

    pub async fn checkpoint(&self) -> Result<usize> {
        self.request(Command::Drain).await
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.accepted.load(Ordering::Acquire),
            self.probes.load(Ordering::Acquire),
            self.rejected_connects.load(Ordering::Acquire),
        )
    }

    async fn request(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<usize>>) -> Command,
    ) -> Result<usize> {
        ensure!(
            !self.stopped.load(Ordering::Acquire),
            "native browser transport sink is unavailable"
        );
        timeout(DEADLINE, async {
            let (reply, received) = oneshot::channel();
            self.control
                .send(command(reply))
                .await
                .context("requesting native listener checkpoint")?;
            received
                .await
                .context("native listener stopped before drain acknowledgement")?
        })
        .await
        .context("native listener checkpoint timed out")?
    }
}

pub(super) struct NativeSink {
    listener: Arc<StdListener>,
    status: Arc<NativeStatus>,
    tasks: JoinSet<Result<usize>>,
}

struct Actor {
    listener: TcpListener,
    ready: Arc<StdListener>,
    status: Arc<NativeStatus>,
    commands: mpsc::Receiver<Command>,
    pending: FuturesUnordered<PendingSocket>,
    connects: BTreeMap<String, usize>,
    #[cfg(test)]
    local_origins: Vec<url::Origin>,
}

impl NativeSink {
    pub fn start(listener: TcpListener) -> Result<Self> {
        let ready = Arc::new(listener.into_std()?);
        let listener = TcpListener::from_std(ready.try_clone()?)?;
        let (control, commands) = mpsc::channel(1);
        let status = Arc::new(NativeStatus {
            accepted: AtomicUsize::new(0),
            denied: AtomicUsize::new(0),
            probes: AtomicUsize::new(0),
            rejected_connects: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            control,
        });
        let actor = Actor {
            listener,
            ready: Arc::clone(&ready),
            status: Arc::clone(&status),
            commands,
            pending: FuturesUnordered::new(),
            connects: BTreeMap::new(),
            #[cfg(test)]
            local_origins: Vec::new(),
        };
        let observed = Arc::clone(&status);
        let mut tasks = JoinSet::new();
        tasks.spawn(
            async move {
                let result = stream::try_unfold(actor, |mut actor| async move {
                    actor
                        .step()
                        .await
                        .map(|active| active.then_some(((), actor)))
                })
                .take(MAX_COMMANDS)
                .try_fold(
                    0_usize,
                    |count, ()| async move { Ok(count.saturating_add(1)) },
                )
                .await;
                observed.stopped.store(true, Ordering::Release);
                result
            }
            .instrument(tracing::info_span!("serialized_browser_native_sink")),
        );
        Ok(Self {
            listener: ready,
            status,
            tasks,
        })
    }

    pub fn status(&self) -> Arc<NativeStatus> {
        Arc::clone(&self.status)
    }

    pub async fn finish(mut self) -> Result<()> {
        let stopping = timeout(DEADLINE, self.status.control.send(Command::Stop))
            .await
            .context("stopping native transport sink timed out")
            .and_then(|result| result.map_err(anyhow::Error::from));
        let joined = match timeout(DEADLINE, self.tasks.join_next()).await {
            Ok(Some(Ok(result))) => {
                result.map(|count| tracing::debug!(commands = count, "native listener drained"))
            }
            Ok(Some(Err(error))) => Err(anyhow!(error).context("joining native transport sink")),
            Ok(None) => Ok(()),
            Err(_) => {
                self.tasks.abort_all();
                match self.tasks.join_next().await {
                    Some(Err(error)) if error.is_cancelled() => {}
                    Some(Err(error)) => {
                        return Err(anyhow!(error).context("draining aborted native transport sink"));
                    }
                    Some(Ok(result)) => {
                        result?;
                    }
                    None => {}
                }
                Err(anyhow!("native transport sink exceeded shutdown deadline"))
            }
        };
        drop(self.listener);
        stopping.and(joined)
    }

    #[cfg(test)]
    pub fn denied_connections(&self) -> usize {
        self.status.denied.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn address(&self) -> Result<std::net::SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    #[cfg(test)]
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

impl Actor {
    async fn step(&mut self) -> Result<bool> {
        tokio::select! {
            biased;
            command = self.commands.recv() => match command.context("native sink owner dropped command channel")? {
                #[cfg(test)]
                Command::Begin { local_origins, reply } => {
                    self.reply_checkpoint(reply).await?;
                    self.local_origins = local_origins;
                }
                Command::Drain(reply) => self.reply_checkpoint(reply).await?,
                Command::Stop => { self.checkpoint().await?; return Ok(false); }
                #[cfg(test)]
                Command::Hold { ready, release } => {
                    ready.send(()).map_err(|()| anyhow!("native sink pause requester cancelled"))?;
                    timeout(DEADLINE, release).await.context("native sink test pause expired")??;
                }
            },
            observed = self.pending.next(), if !self.pending.is_empty() => {
                self.record(observed.context("native observation disappeared")??);
            }
            accepted = self.listener.accept() => {
                let (socket, _) = accepted?;
                self.accept(socket)?;
            }
        }
        Ok(true)
    }

    fn accept(&self, socket: TcpStream) -> Result<()> {
        let previous = self.status.accepted.fetch_add(1, Ordering::AcqRel);
        ensure!(
            previous < MAX_CONNECTIONS,
            "native browser connection budget exceeded"
        );
        self.pending.push(PendingSocket::new(socket));
        Ok(())
    }

    fn record(&mut self, observation: Observation) {
        match observation {
            Observation::Probe => {
                self.status.probes.fetch_add(1, Ordering::AcqRel);
            }
            Observation::Request => {
                self.status.denied.fetch_add(1, Ordering::AcqRel);
            }
            Observation::Connect(authority) => {
                let count = self.connects.entry(authority).or_insert(0);
                *count = count.saturating_add(1);
            }
        }
    }

    async fn reply_checkpoint(&mut self, reply: oneshot::Sender<Result<usize>>) -> Result<()> {
        let drained = self.checkpoint().await;
        let failed = drained.is_err();
        if reply.send(drained).is_err() {
            tracing::debug!("native checkpoint requester cancelled");
        }
        ensure!(!failed, "native listener checkpoint failed");
        Ok(())
    }

    async fn checkpoint(&mut self) -> Result<usize> {
        let ready = Arc::clone(&self.ready);
        std::iter::from_fn(|| match ready.accept() {
            Ok((socket, _)) => Some(Ok(socket)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(error) => Some(Err(error)),
        })
        .take(MAX_CONNECTIONS.saturating_add(1))
        .try_for_each(|socket| {
            let socket = socket?;
            socket.set_nonblocking(true)?;
            self.accept(TcpStream::from_std(socket)?)
        })?;
        self.pending.iter_mut().for_each(PendingSocket::quiesce);
        let count = self.pending.len();
        let (_, outcome) = stream::iter(0..count)
            .fold(
                (&mut *self, Ok::<(), anyhow::Error>(())),
                |(actor, result), _| async move {
                    let observation = actor
                        .pending
                        .next()
                        .await
                        .context("native checkpoint observation disappeared")
                        .and_then(std::convert::identity);
                    let recorded = observation.map(|observation| actor.record(observation));
                    if let Err(error) = &recorded {
                        tracing::warn!(%error, "native routing observation failed");
                    }
                    (actor, result.and(recorded))
                },
            )
            .await;
        outcome?;
        self.validate_connects().await?;
        let denied = self.status.denied.load(Ordering::Acquire);
        let (accepted, probes, rejected_connects) = self.status.counts();
        tracing::debug!(
            accepted,
            probes,
            denied,
            rejected_connects,
            "native routing checkpoint completed"
        );
        Ok(denied)
    }

    async fn validate_connects(&mut self) -> Result<()> {
        let routes = std::mem::take(&mut self.connects);
        #[cfg(test)]
        let local_origins = self.local_origins.as_slice();
        let result = stream::iter(routes)
            .map(|(authority, count)| async move {
                let checked = routing::validate_connect(
                    &authority,
                    #[cfg(test)]
                    local_origins,
                )
                .await;
                let allowed = match checked {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::debug!(%error, "native CONNECT route rejected without forwarding");
                        false
                    }
                };
                (allowed, count)
            })
            .buffer_unordered(8)
            .fold(
                (0_usize, 0_usize),
                |(probes, denied), (allowed, count)| async move {
                    if allowed {
                        (probes.saturating_add(count), denied)
                    } else {
                        (probes, denied.saturating_add(count))
                    }
                },
            );
        let (probes, denied) = timeout(ROUTE_DEADLINE, result)
            .await
            .context("native CONNECT policy resolution exceeded deadline")?;
        self.status.probes.fetch_add(probes, Ordering::AcqRel);
        self.status.denied.fetch_add(denied, Ordering::AcqRel);
        self.status
            .rejected_connects
            .fetch_add(denied, Ordering::AcqRel);
        Ok(())
    }
}
