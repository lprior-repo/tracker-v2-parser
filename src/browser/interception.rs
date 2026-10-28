use std::{
    collections::BTreeMap,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::{
        fetch::{self, EventRequestPaused, FulfillRequestParams, HeaderEntry},
        network::{ResourceType, SetBypassServiceWorkerParams, SetCacheDisabledParams},
    },
};
use futures::{StreamExt, TryStreamExt, stream};
use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinError, JoinSet},
    time::timeout,
};
use tracing::Instrument;

use super::{
    COMMAND_TIMEOUT,
    exchange::{self, DestinationPolicy},
};
use crate::model::{Capture, Target};

const MAX_PAGE_REQUESTS: usize = 512;
const CONCURRENT_REQUESTS: usize = 8;
const PENDING_REQUESTS: usize = 32;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const AUXILIARY_POLICY: &str = "sandbox allow-scripts allow-same-origin allow-forms; worker-src 'none'; frame-src 'none'; connect-src http: https:";
const AUDIT_SCRIPT: &str = r"(() => {
    const report = globalThis.__trackBlockedBinding;
    Object.defineProperty(globalThis, '__trackBlockedBinding', {value: report, writable: false, configurable: false});
    const mark = () => report('blocked');
    addEventListener('securitypolicyviolation', mark);
    const open = window.open;
    ['Worker', 'SharedWorker', 'WebSocket', 'WebTransport', 'RTCPeerConnection', 'webkitRTCPeerConnection'].forEach(name => {
        if (typeof globalThis[name] !== 'function') return;
        Object.defineProperty(globalThis, name, {value: function() {
            mark();
            throw new Error('Auxiliary network context blocked during browser acquisition');
        }, writable: false, configurable: false});
    });
    if (typeof ServiceWorkerContainer === 'function') {
        Object.defineProperty(ServiceWorkerContainer.prototype, 'register', {value: function() {
            mark();
            return Promise.reject(new Error('Service worker blocked during browser acquisition'));
        }, writable: false, configurable: false});
    }
    Object.defineProperty(window, 'open', {value: function(...args) { mark(); return open.apply(this, args); }, writable: false, configurable: false});
    addEventListener('click', event => {
        const link = event.target instanceof Element && event.target.closest('a,area');
        if (link && link.target && !['_self', '_top', '_parent'].includes(link.target)) mark();
    }, true);
    addEventListener('submit', event => {
        if (event.target.target && !['_self', '_top', '_parent'].includes(event.target.target)) mark();
    }, true);
})()";

pub(super) struct Interception {
    stop: Option<(oneshot::Sender<()>, oneshot::Sender<()>)>,
    tasks: JoinSet<Result<usize>>,
    blocked: Arc<AtomicBool>,
}

impl Interception {
    #[tracing::instrument(skip_all)]
    pub async fn install(
        page: &Page,
        target: &Target,
        headers: &BTreeMap<String, String>,
        endpoint: &str,
        #[cfg(test)] local_origin: Option<url::Origin>,
        #[cfg(test)] additional_origin: Option<url::Origin>,
    ) -> Result<Self> {
        page.execute(SetBypassServiceWorkerParams::new(true))
            .await?;
        page.execute(SetCacheDisabledParams::new(true)).await?;
        let events = page.event_listener::<EventRequestPaused>().await?;
        page.execute(fetch::EnableParams::default()).await?;
        let policy = DestinationPolicy::new(target, headers);
        #[cfg(test)]
        let policy = {
            let mut policy = policy;
            if let Some(origin) = local_origin {
                policy.allow_local_origin(origin);
            }
            if let Some(origin) = additional_origin {
                policy.allow_local_origin(origin);
            }
            policy
        };
        let policy = Arc::new(policy);
        let page = page.clone();
        let blocked = Arc::new(AtomicBool::new(false));
        let failures = Arc::clone(&blocked);
        let (stop, stopped) = oneshot::channel();
        let mut tasks = JoinSet::new();
        let audit_stop = super::audit::install(
            endpoint,
            page.target_id(),
            AUDIT_SCRIPT,
            &mut tasks,
            Arc::clone(&blocked),
        )
        .await?;
        let (pending, receiver) = mpsc::channel(PENDING_REQUESTS);
        tasks.spawn(
            async move {
                events
                    .take_until(stopped)
                    .take(MAX_PAGE_REQUESTS)
                    .map(Ok::<_, anyhow::Error>)
                    .try_fold(0_usize, |count, event| {
                        let sent = pending
                            .try_send(event)
                            .context("browser request admission budget exceeded");
                        async move { sent.map(|()| count.saturating_add(1)) }
                    })
                    .await
            }
            .instrument(tracing::info_span!("browser_request_admission")),
        );
        tasks.spawn(
            async move {
                stream::unfold(receiver, |mut receiver| async move {
                    receiver.recv().await.map(|event| (event, receiver))
                })
                .take(MAX_PAGE_REQUESTS)
                .map(|event| {
                    let page = page.clone();
                    let policy = Arc::clone(&policy);
                    let failures = Arc::clone(&failures);
                    async move { fulfill(&page, &policy, &event, &failures).await }
                })
                .buffer_unordered(CONCURRENT_REQUESTS)
                .try_fold(
                    0_usize,
                    |count, ()| async move { Ok(count.saturating_add(1)) },
                )
                .await
            }
            .instrument(tracing::info_span!("browser_request_interception")),
        );
        Ok(Self {
            stop: Some((stop, audit_stop)),
            tasks,
            blocked,
        })
    }

    pub async fn run(
        &mut self,
        operation: impl Future<Output = Result<Capture>>,
    ) -> Result<Capture> {
        let result = tokio::select! {
            result = operation => result,
            stopped = self.tasks.join_next() => {
                match stopped {
                    Some(Ok(Ok(count))) => Err(anyhow!("browser interception stopped after {count} requests before acquisition completed")),
                    Some(Ok(Err(error))) => Err(error.context("browser interception failed")),
                    Some(Err(error)) => Err(anyhow!(error).context("browser interception task failed")),
                    None => Err(anyhow!("browser interception task disappeared")),
                }
            }
        };
        let capture = result?;
        if !crate::transport::access_stop_status(capture.status) {
            ensure!(
                !self.blocked.load(Ordering::Acquire),
                "browser acquisition incomplete: a requested destination was blocked"
            );
        }
        Ok(capture)
    }

    pub async fn finish(mut self, access_stop: bool) -> Result<()> {
        if let Some((stop, audit_stop)) = self.stop.take() {
            if stop.send(()).is_err() {
                tracing::debug!("browser interceptor already stopped");
            }
            if audit_stop.send(()).is_err() {
                tracing::debug!("browser acquisition audit already stopped");
            }
        }
        let result = match timeout(DRAIN_TIMEOUT, drain(&mut self.tasks, false)).await {
            Ok(result) => result,
            Err(_) => self.abort_and_drain().await,
        };
        result?;
        ensure!(
            access_stop || !self.blocked.load(Ordering::Acquire),
            "browser acquisition incomplete: a requested destination was blocked"
        );
        Ok(())
    }

    async fn abort_and_drain(&mut self) -> Result<()> {
        let aborted = self.tasks.len();
        self.tasks.abort_all();
        let result = drain(&mut self.tasks, true).await;
        tracing::warn!(
            aborted,
            remaining = self.tasks.len(),
            "browser interception exceeded drain deadline"
        );
        result?;
        Err(anyhow!(
            "browser interception exceeded drain deadline; acquisition incomplete"
        ))
    }
}

async fn drain(tasks: &mut JoinSet<Result<usize>>, cancelled: bool) -> Result<()> {
    let first = joined(tasks.join_next().await, cancelled);
    let second = joined(tasks.join_next().await, cancelled);
    let third = joined(tasks.join_next().await, cancelled);
    first.and(second).and(third)
}

fn joined(result: Option<Result<Result<usize>, JoinError>>, cancelled: bool) -> Result<()> {
    match result {
        Some(Ok(result)) => {
            let completed = result.context("browser acquisition incomplete: interception task failed")?;
            tracing::debug!(completed, "browser interception task drained");
            Ok(())
        }
        Some(Err(error)) if cancelled && error.is_cancelled() => Ok(()),
        Some(Err(error)) => Err(anyhow!(error).context("joining browser interception")),
        None => Ok(()),
    }
}

#[tracing::instrument(skip_all)]
async fn fulfill(
    page: &Page,
    policy: &DestinationPolicy,
    event: &EventRequestPaused,
    blocked: &AtomicBool,
) -> Result<()> {
    let response = timeout(COMMAND_TIMEOUT, exchange::request(policy, page, event))
        .await
        .context("pinned browser exchange timed out")
        .and_then(|result| result);
    let mut response = match response {
        Ok(response) => response,
        Err(error)
            if event.resource_type == ResourceType::Document
                && !error.is::<exchange::RejectedDestination>() =>
        {
            return Err(error.context("pinned browser document request failed"));
        }
        Err(error) => {
            tracing::warn!(%error, "browser request failed closed before destination continuation");
            blocked.store(true, Ordering::Release);
            exchange::Response {
                status: 403,
                headers: vec![HeaderEntry::new("content-type", "text/plain")],
                body: b"Browser destination blocked; human review required".to_vec(),
            }
        }
    };
    if crate::transport::access_stop_status(response.status) {
        blocked.store(true, Ordering::Release);
    }
    if event.resource_type == ResourceType::Document {
        response.headers.push(HeaderEntry::new(
            "content-security-policy",
            AUXILIARY_POLICY,
        ));
    }
    let params = FulfillRequestParams::builder()
        .request_id(event.request_id.clone())
        .response_code(response.status)
        .response_headers(response.headers)
        .body(exchange::encoded_body(&response.body))
        .build()
        .map_err(|error| anyhow!(error))?;
    timeout(COMMAND_TIMEOUT, page.execute(params))
        .await
        .context("fulfilling browser request timed out")??;
    Ok(())
}

#[cfg(test)]
mod review_tests;
