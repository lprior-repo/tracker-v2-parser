use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, ensure};
use chromiumoxide::{
    Connection, Method,
    cdp::browser_protocol::target::{SessionId, TargetId},
    types::{CallId, EventMessage, Message, MethodId},
};
use futures::{StreamExt, TryStreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::oneshot, task::JoinSet};
use tracing::Instrument;

const MAX_MESSAGES: usize = 8192;

#[derive(Debug, Deserialize)]
struct Event {
    method: String,
    #[serde(rename = "sessionId")]
    session: Option<String>,
    params: Value,
}

impl Method for Event {
    fn identifier(&self) -> MethodId {
        self.method.clone().into()
    }
}

impl EventMessage for Event {
    fn session_id(&self) -> Option<&str> {
        self.session.as_deref()
    }
}

struct Observer {
    connection: Connection<Event>,
    session: Option<SessionId>,
    target: String,
    blocked: Arc<AtomicBool>,
    destroyed: bool,
    detached: bool,
}

pub(super) async fn install(
    endpoint: &str,
    target: &TargetId,
    script: &str,
    tasks: &mut JoinSet<Result<usize>>,
    blocked: Arc<AtomicBool>,
) -> Result<oneshot::Sender<()>> {
    let mut observer = Observer {
        connection: Connection::connect(endpoint).await?,
        session: None,
        target: target.as_ref().to_owned(),
        blocked,
        destroyed: false,
        detached: false,
    };
    observer
        .command("Target.setDiscoverTargets", None, json!({"discover": true}))
        .await?;
    let attached = observer
        .command(
            "Target.attachToTarget",
            None,
            json!({"targetId": target, "flatten": true}),
        )
        .await?;
    observer.session = Some(SessionId::new(
        attached
            .get("sessionId")
            .and_then(Value::as_str)
            .context("audit attachment omitted session")?,
    ));
    observer
        .command("Page.enable", observer.session.clone(), json!({}))
        .await?;
    observer
        .command("Runtime.enable", observer.session.clone(), json!({}))
        .await?;
    observer
        .command("Network.enable", observer.session.clone(), json!({}))
        .await?;
    observer
        .command("Audits.enable", observer.session.clone(), json!({}))
        .await?;
    observer
        .command(
            "Runtime.addBinding",
            observer.session.clone(),
            json!({"name": "__trackBlockedBinding"}),
        )
        .await?;
    observer
        .command(
            "Page.addScriptToEvaluateOnNewDocument",
            observer.session.clone(),
            json!({"source": script}),
        )
        .await?;
    let (finish, finished) = oneshot::channel();
    tasks.spawn(
        observer
            .run(finished)
            .instrument(tracing::info_span!("ordered_browser_audit")),
    );
    Ok(finish)
}

impl Observer {
    async fn command(
        &mut self,
        method: &'static str,
        session: Option<SessionId>,
        params: Value,
    ) -> Result<Value> {
        let id = self
            .connection
            .submit_command(method.into(), session, params)?;
        let responses = stream::try_unfold((self, 0_usize), move |(observer, count)| async move {
            ensure!(
                count < MAX_MESSAGES,
                "audit command exceeded message budget"
            );
            let message = observer
                .connection
                .next()
                .await
                .context("audit connection ended before command response")??;
            let response = match message {
                Message::Response(response) if response.id == id => {
                    ensure!(response.error.is_none(), "audit command failed");
                    Some(response.result.context("audit command omitted result")?)
                }
                Message::Event(event) => {
                    observer.observe(&event);
                    None
                }
                Message::Response(_) => None,
            };
            Ok::<_, anyhow::Error>(Some((response, (observer, count.saturating_add(1)))))
        })
        .try_filter_map(|response| futures::future::ready(Ok(response)));
        futures::pin_mut!(responses);
        responses
            .try_next()
            .await?
            .context("audit command ended without response")
    }

    fn observe(&mut self, event: &Event) {
        let our_session = event.session.as_deref() == self.session.as_ref().map(AsRef::as_ref);
        let blocked = match event.method.as_str() {
            "Runtime.bindingCalled" => {
                our_session
                    && event.params.get("name").and_then(Value::as_str)
                        == Some("__trackBlockedBinding")
            }
            "Audits.issueAdded" => {
                our_session
                    && event.params.pointer("/issue/code").and_then(Value::as_str)
                        == Some("ContentSecurityPolicyIssue")
            }
            "Network.webSocketCreated" | "Network.webTransportCreated" => our_session,
            _ => false,
        };
        if blocked {
            self.blocked.store(true, Ordering::Release);
        }
        if event.method == "Target.targetDestroyed"
            && event.params.get("targetId").and_then(Value::as_str) == Some(self.target.as_str())
        {
            self.destroyed = true;
        }
        if event.method == "Target.detachedFromTarget"
            && event.params.get("sessionId").and_then(Value::as_str)
                == self.session.as_ref().map(AsRef::as_ref)
        {
            self.detached = true;
        }
    }

    async fn run(self, finished: oneshot::Receiver<()>) -> Result<usize> {
        let certified = Arc::new(AtomicBool::new(false));
        let state = Running {
            observer: self,
            finished,
            requested: false,
            fence: None,
            certified: Arc::clone(&certified),
        };
        let count = stream::try_unfold(state, Running::step)
            .take(MAX_MESSAGES)
            .try_fold(
                0_usize,
                |count, ()| async move { Ok(count.saturating_add(1)) },
            )
            .await?;
        ensure!(
            certified.load(Ordering::Acquire),
            "browser acquisition incomplete: ordered audit teardown was not certified"
        );
        Ok(count)
    }
}

struct Running {
    observer: Observer,
    finished: oneshot::Receiver<()>,
    requested: bool,
    fence: Option<CallId>,
    certified: Arc<AtomicBool>,
}

impl Running {
    async fn step(mut self) -> Result<Option<((), Self)>> {
        if self.requested
            && self.observer.destroyed
            && self.observer.detached
            && self.fence.is_none()
        {
            self.fence = Some(self.observer.connection.submit_command(
                "Browser.getVersion".into(),
                None,
                json!({}),
            )?);
        }
        let message = tokio::select! {
            signal = &mut self.finished, if !self.requested => {
                signal.context("browser audit owner abandoned teardown")?;
                self.requested = true;
                return Ok(Some(((), self)));
            }
            message = self.observer.connection.next() => message.context("browser acquisition incomplete: audit connection closed before teardown fence")??,
        };
        match message {
            Message::Event(event) => self.observer.observe(&event),
            Message::Response(response) if self.fence == Some(response.id) => {
                ensure!(
                    response.error.is_none(),
                    "browser audit teardown fence failed"
                );
                self.certified.store(true, Ordering::Release);
                return Ok(None);
            }
            Message::Response(_) => {}
        }
        Ok(Some(((), self)))
    }
}
