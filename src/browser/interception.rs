use std::{
    collections::BTreeMap,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::{
        fetch::{self, EventRequestPaused, FulfillRequestParams, HeaderEntry},
        network::{ResourceType, SetBypassServiceWorkerParams, SetCacheDisabledParams},
        page::{EventLifecycleEvent, SetLifecycleEventsEnabledParams},
    },
    listeners::EventStream,
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
    capture::PageCapture,
    exchange::{self, DestinationPolicy},
};
use crate::model::Target;

mod admission;
mod drain;

const MAX_PAGE_REQUESTS: usize = 512;
const CONCURRENT_REQUESTS: usize = 8;
const PENDING_REQUESTS: usize = 32;
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
    policy: Arc<DestinationPolicy>,
    admission: admission::Gate,
    audit_stop: Option<oneshot::Sender<()>>,
    tasks: JoinSet<Result<usize>>,
    guards: JoinSet<Result<usize>>,
    blocked: Arc<AtomicBool>,
    idle: EventStream<EventLifecycleEvent>,
}

impl Interception {
    #[tracing::instrument(skip_all)]
    pub async fn install(
        page: &Page,
        target: &Target,
        headers: &BTreeMap<String, String>,
        endpoint: &str,
        interval: std::time::Duration,
        persistent_pacing: Option<crate::durable::pacing::PersistentPacing>,
        #[cfg(test)] local_origin: Option<url::Origin>,
        #[cfg(test)] additional_origin: Option<url::Origin>,
    ) -> Result<Self> {
        page.execute(SetBypassServiceWorkerParams::new(true))
            .await?;
        page.execute(SetCacheDisabledParams::new(true)).await?;
        let events = page.event_listener::<EventRequestPaused>().await?;
        let idle = page.event_listener::<EventLifecycleEvent>().await?;
        page.execute(SetLifecycleEventsEnabledParams::new(true))
            .await?;
        page.execute(fetch::EnableParams::default()).await?;
        let policy = DestinationPolicy::new(target, headers, interval)
            .with_persistent_pacing(persistent_pacing);
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
        let retained_policy = Arc::clone(&policy);
        let page = page.clone();
        let blocked = Arc::new(AtomicBool::new(false));
        let failures = Arc::clone(&blocked);
        let mut tasks = JoinSet::new();
        let mut guards = JoinSet::new();
        let audit_stop = super::audit::install(
            endpoint,
            page.target_id(),
            AUDIT_SCRIPT,
            &mut guards,
            Arc::clone(&blocked),
        )
        .await?;
        let (pending, receiver) = mpsc::channel(PENDING_REQUESTS);
        let (admission, admitted) = admission::new(events, pending, Arc::clone(&blocked));
        guards.spawn(
            admitted
                .run()
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
            policy: retained_policy,
            admission,
            audit_stop: Some(audit_stop),
            tasks,
            guards,
            blocked,
            idle,
        })
    }

    pub fn preserve_stop<T>(&self, result: Result<T>) -> Result<T> {
        if result.as_ref().is_err_and(|error| {
            !error.is::<crate::transport::AccessStop>()
                && !error.is::<crate::transport::AccessStopCaptureError>()
        }) {
            self.policy.check_stop()?;
        }
        result
    }

    pub async fn run(
        &mut self,
        operation: impl Future<Output = Result<PageCapture>>,
    ) -> Result<PageCapture> {
        let result = tokio::select! {
            result = operation => result,
            stopped = self.tasks.join_next() => stopped_result(stopped),
            stopped = self.guards.join_next() => stopped_result(stopped),
        };
        let capture = result?;
        if !crate::transport::access_stop_status(capture.capture().status) {
            ensure!(
                !self.blocked.load(Ordering::Acquire),
                "browser acquisition incomplete: a requested destination was blocked"
            );
        }
        Ok(capture)
    }
}

fn stopped_result(result: Option<Result<Result<usize>, JoinError>>) -> Result<PageCapture> {
    match result {
        Some(Ok(Ok(count))) => Err(anyhow!(
            "browser interception task stopped after {count} events before acquisition completed"
        )),
        Some(Ok(Err(error))) => Err(error.context("browser interception failed")),
        Some(Err(error)) => Err(anyhow!(error).context("browser interception task failed")),
        None => Err(anyhow!("browser interception task disappeared")),
    }
}

#[tracing::instrument(skip_all)]
async fn fulfill(
    page: &Page,
    policy: &DestinationPolicy,
    event: &EventRequestPaused,
    blocked: &AtomicBool,
) -> Result<()> {
    let response = exchange::request(policy, page, event).await;
    let observed_stop = response
        .as_ref()
        .is_ok_and(|response| crate::transport::access_stop_status(response.status));
    let mut response = match response {
        Ok(response) => response,
        Err(error)
            if error.is::<crate::transport::AccessStop>()
                || error.is::<crate::transport::AccessStopCaptureError>() =>
        {
            return Err(error);
        }
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
    if observed_stop && event.resource_type != ResourceType::Document {
        return Err(crate::transport::AccessStop {
            origin: url::Url::parse(&event.request.url)?
                .origin()
                .ascii_serialization(),
            status: Some(response.status),
        }
        .into());
    }
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
