use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result, ensure};
use tokio::{sync::{mpsc, oneshot}, time::timeout};

use super::{DEADLINE, http::{Method, Path}, observe::Denial};

pub(super) enum Command {
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

pub(in crate::browser) struct NativeStatus {
    pub(super) accepted: AtomicUsize,
    pub(super) denied: AtomicUsize,
    pub(super) probes: AtomicUsize,
    pub(super) rejected_connects: AtomicUsize,
    non_connects: AtomicUsize,
    invalid_connects: AtomicUsize,
    incomplete_connects: AtomicUsize,
    methods: [AtomicUsize; 4],
    paths: [AtomicUsize; 3],
    pub(super) stopped: AtomicBool,
    pub(super) control: mpsc::Sender<Command>,
}

impl NativeStatus {
    pub(super) fn new(control: mpsc::Sender<Command>) -> Self {
        Self {
            accepted: AtomicUsize::new(0),
            denied: AtomicUsize::new(0),
            probes: AtomicUsize::new(0),
            rejected_connects: AtomicUsize::new(0),
            non_connects: AtomicUsize::new(0),
            invalid_connects: AtomicUsize::new(0),
            incomplete_connects: AtomicUsize::new(0),
            methods: std::array::from_fn(|_| AtomicUsize::new(0)),
            paths: std::array::from_fn(|_| AtomicUsize::new(0)),
            stopped: AtomicBool::new(false),
            control,
        }
    }

    #[cfg(test)]
    pub async fn begin(&self, local_origins: Vec<url::Origin>) -> Result<usize> {
        self.request(|reply| Command::Begin { local_origins, reply }).await
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

    pub fn denial_kinds(&self) -> (usize, usize, usize) {
        (
            self.non_connects.load(Ordering::Acquire),
            self.invalid_connects.load(Ordering::Acquire),
            self.incomplete_connects.load(Ordering::Acquire),
        )
    }

    pub fn request_kinds(&self) -> ([usize; 4], [usize; 3]) {
        (
            self.methods.each_ref().map(|count| count.load(Ordering::Acquire)),
            self.paths.each_ref().map(|count| count.load(Ordering::Acquire)),
        )
    }

    pub(super) fn record_denial(&self, kind: Denial) {
        let counter = match kind {
            Denial::NonConnect(method, path) => {
                self.record_http(method, path);
                &self.non_connects
            }
            Denial::InvalidConnect => &self.invalid_connects,
            Denial::IncompleteConnect => &self.incomplete_connects,
        };
        counter.fetch_add(1, Ordering::AcqRel);
        self.denied.fetch_add(1, Ordering::AcqRel);
    }

    fn record_http(&self, method: Method, path: Path) {
        let [get, head, options, other] = &self.methods;
        let method = match method {
            Method::Get => get,
            Method::Head => head,
            Method::Options => options,
            Method::Other => other,
        };
        method.fetch_add(1, Ordering::AcqRel);
        let [favicon, probe, other] = &self.paths;
        let path = match path {
            Path::Favicon => favicon,
            Path::Probe => probe,
            Path::Other => other,
        };
        path.fetch_add(1, Ordering::AcqRel);
    }

    async fn request(&self, command: impl FnOnce(oneshot::Sender<Result<usize>>) -> Command) -> Result<usize> {
        ensure!(!self.stopped.load(Ordering::Acquire), "native browser transport sink is unavailable");
        timeout(DEADLINE, async {
            let (reply, received) = oneshot::channel();
            self.control.send(command(reply)).await.context("requesting native listener checkpoint")?;
            received.await.context("native listener stopped before drain acknowledgement")?
        })
        .await
        .context("native listener checkpoint timed out")?
    }
}
