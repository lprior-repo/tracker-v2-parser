mod cleanup;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::{browser::BrowserContextId, target::CreateTargetParams},
};
use tokio::{
    sync::oneshot,
    task::{JoinError, JoinHandle, JoinSet},
    time::timeout,
};

use super::{
    Browser, COMMAND_TIMEOUT, capture::PageCapture, finish_page_fetch, interception::Interception,
};
use super::{native::NativeStatus, owned::OwnedChromium};
use crate::model::{Capture, Target};

pub(super) struct Acquisition {
    pub chromium: Arc<OwnedChromium>,
    pub context: BrowserContextId,
    pub target: Target,
    pub headers: BTreeMap<String, String>,
    pub interval: Duration,
    pub persistent_pacing: Option<crate::durable::pacing::PersistentPacing>,
    pub deadline: Duration,
    pub native_status: Arc<NativeStatus>,
    #[cfg(test)]
    pub test_controls: TestControls,
}

#[cfg(test)]
pub(super) struct TestControls {
    local_origin: Option<url::Origin>,
    additional_origin: Option<url::Origin>,
    fail_page_close: bool,
    capture_ready: Option<Arc<tokio::sync::Notify>>,
}

#[cfg(test)]
impl From<&Browser> for TestControls {
    fn from(browser: &Browser) -> Self {
        Self {
            local_origin: browser.local_origin.clone(),
            additional_origin: browser.additional_origin.clone(),
            fail_page_close: browser.fail_page_close,
            capture_ready: browser.capture_ready.clone(),
        }
    }
}

impl Acquisition {
    #[tracing::instrument(skip_all)]
    pub async fn run(self, mut reply: oneshot::Sender<Result<Capture>>) -> Result<()> {
        let baseline = self.begin_native().await?;
        let params = CreateTargetParams::builder()
            .url("about:blank")
            .browser_context_id(self.context.clone())
            .build()
            .map_err(|error| anyhow!(error))?;
        let result = self.acquire_page(params, &mut reply).await;
        let native = self.verify_native(baseline).await;
        Self::deliver(reply, finish_page_fetch(result, native));
        Ok(())
    }

    async fn begin_native(&self) -> Result<usize> {
        self.native_status
            .begin(
                #[cfg(test)]
                self.test_controls
                    .local_origin
                    .iter()
                    .chain(self.test_controls.additional_origin.iter())
                    .cloned()
                    .collect(),
            )
            .await
    }

    async fn acquire_page(
        &self,
        params: CreateTargetParams,
        reply: &mut oneshot::Sender<Result<Capture>>,
    ) -> Result<Capture> {
        let opened = timeout(
            COMMAND_TIMEOUT.saturating_mul(2),
            self.chromium.new_page(params),
        )
        .await
        .context("opening browser page timed out")
        .and_then(std::convert::identity);
        match opened {
            Ok(page) => self.capture_and_close(page, reply).await,
            Err(error) => {
                self.chromium.mark_unavailable();
                match self.dispose_or_terminate().await {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(error.context(format!(
                        "opening-page cleanup failed; context remains owned: {cleanup:#}"
                    ))),
                }
            }
        }
    }

    async fn verify_native(&self, baseline: usize) -> Result<()> {
        let count = self.native_status.checkpoint().await?;
        let (accepted, probes, rejected_connects) = self.native_status.counts();
        let (non_connects, invalid_connects, incomplete_connects) =
            self.native_status.denial_kinds();
        ensure!(
            count == baseline,
            "browser acquisition incomplete: native request was blocked (accepted={accepted}, benign_probes={probes}, rejected_connects={rejected_connects}, non_connects={non_connects}, invalid_connects={invalid_connects}, incomplete_connects={incomplete_connects}, denied_before={baseline}, denied_after={count})"
        );
        Ok(())
    }

    fn deliver(reply: oneshot::Sender<Result<Capture>>, result: Result<Capture>) {
        match reply.send(result) {
            Ok(()) => (),
            Err(Ok(_)) => {
                tracing::debug!("browser caller cancelled after acquisition cleanup completed");
            }
            Err(Err(error)) => {
                tracing::warn!(%error, "cancelled browser acquisition ended after owned cleanup");
            }
        }
    }

    async fn capture_and_close(
        &self,
        page: Page,
        reply: &mut oneshot::Sender<Result<Capture>>,
    ) -> Result<Capture> {
        let installed = timeout(COMMAND_TIMEOUT, self.install_interception(&page))
            .await
            .context("installing browser destination policy timed out")
            .and_then(std::convert::identity);
        let (result, interceptor) = match installed {
            Ok(mut interceptor) => {
                let result = self
                    .capture_with_interception(&page, &mut interceptor, reply)
                    .await;
                (result, Some(interceptor))
            }
            Err(error) => (Err(error), None),
        };
        self.teardown(page, interceptor, result).await
    }

    async fn capture_with_interception(
        &self,
        page: &Page,
        interceptor: &mut Interception,
        reply: &mut oneshot::Sender<Result<Capture>>,
    ) -> Result<PageCapture> {
        let fetch = Browser::fetch_page(page, &self.target, &self.headers, self.interval);
        let operation = timeout(self.deadline, interceptor.run(fetch));
        tokio::select! {
            () = reply.closed() => Err(anyhow!("browser acquisition cancelled by caller")),
            result = operation => result.context("browser request timed out").and_then(std::convert::identity),
        }
    }

    async fn install_interception(&self, page: &Page) -> Result<Interception> {
        #[cfg(test)]
        if self.chromium.partial_install_failure() {
            page.execute(chromiumoxide::cdp::browser_protocol::fetch::EnableParams::default())
                .await?;
            return Err(anyhow!(
                "injected partial interception installation failure"
            ));
        }
        Interception::install(
            page,
            &self.target,
            &self.headers,
            self.chromium.endpoint(),
            self.interval,
            self.persistent_pacing.clone(),
            #[cfg(test)]
            self.test_controls.local_origin.clone(),
            #[cfg(test)]
            self.test_controls.additional_origin.clone(),
        )
        .await
    }

    async fn teardown(
        &self,
        page: Page,
        mut interceptor: Option<Interception>,
        result: Result<PageCapture>,
    ) -> Result<Capture> {
        #[cfg(test)]
        if let Some(ready) = &self.test_controls.capture_ready {
            ready.notify_one();
        }
        let result = Self::prepare_capture(&page, interceptor.as_mut(), result).await;
        let target = page.target_id().clone();
        let closed = Self::close_page(
            page,
            #[cfg(test)]
            self.test_controls.fail_page_close,
        )
        .await;
        self.finish_teardown(&target, interceptor, result, closed)
            .await
    }

    async fn dispose_or_terminate(&self) -> Result<()> {
        match timeout(
            COMMAND_TIMEOUT,
            self.chromium.dispose_browser_context(self.context.clone()),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => self
                .chromium
                .terminate()
                .await
                .context(format!("disposing failed browser context: {error:#}")),
            Err(_) => self
                .chromium
                .terminate()
                .await
                .context("failed browser context disposal timed out"),
        }
    }
}

pub(super) async fn drain(tasks: &mut JoinSet<Result<()>>) -> Result<()> {
    match timeout(COMMAND_TIMEOUT.saturating_mul(8), tasks.join_next()).await {
        Ok(Some(Ok(result))) => result,
        Ok(Some(Err(error))) => {
            Err(anyhow!(error).context("joining browser acquisition supervisor"))
        }
        Ok(None) => Ok(()),
        Err(_) => {
            tasks.abort_all();
            match tasks.join_next().await {
                Some(Err(error)) if error.is_cancelled() => {}
                Some(Err(error)) => {
                    return Err(anyhow!(error).context("draining aborted browser acquisition"));
                }
                Some(Ok(result)) => result?,
                None => {}
            }
            Err(anyhow!(
                "browser acquisition cleanup deadline exceeded; acquisition incomplete"
            ))
        }
    }
}

pub(super) async fn join_handler(mut handler: JoinHandle<Result<()>>) -> Result<()> {
    if let Ok(result) = timeout(COMMAND_TIMEOUT, &mut handler).await {
        handler_result(result)
    } else {
        handler.abort();
        handler_result(handler.await)?;
        Err(anyhow!("Chromium handler exceeded shutdown deadline"))
    }
}

fn handler_result(result: Result<Result<()>, JoinError>) -> Result<()> {
    match result {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(anyhow!(error).context("joining Chromium handler")),
    }
}
