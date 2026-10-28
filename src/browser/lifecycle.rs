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
use crate::{
    model::{Capture, Target},
    transport::access_stop_status,
};

pub(super) struct Acquisition {
    pub chromium: Arc<OwnedChromium>,
    pub context: BrowserContextId,
    pub target: Target,
    pub headers: BTreeMap<String, String>,
    pub interval: Duration,
    pub deadline: Duration,
    pub native_status: Arc<NativeStatus>,
    #[cfg(test)]
    pub local_origin: Option<url::Origin>,
    #[cfg(test)]
    pub additional_origin: Option<url::Origin>,
    #[cfg(test)]
    pub fail_page_close: bool,
    #[cfg(test)]
    pub capture_ready: Option<Arc<tokio::sync::Notify>>,
}

impl Acquisition {
    #[tracing::instrument(skip_all)]
    pub async fn run(self, mut reply: oneshot::Sender<Result<Capture>>) -> Result<()> {
        #[cfg(test)]
        let baseline = self
            .native_status
            .begin(
                self.local_origin
                    .iter()
                    .chain(self.additional_origin.iter())
                    .cloned()
                    .collect(),
            )
            .await?;
        #[cfg(not(test))]
        let baseline = self.native_status.begin().await?;
        let params = CreateTargetParams::builder()
            .url("about:blank")
            .browser_context_id(self.context.clone())
            .build()
            .map_err(|error| anyhow!(error))?;
        let opened = timeout(
            COMMAND_TIMEOUT.saturating_mul(2),
            self.chromium.new_page(params),
        )
        .await
        .context("opening browser page timed out")
        .and_then(std::convert::identity);
        let result = match opened {
            Ok(page) => self.capture_and_close(page, &mut reply).await,
            Err(error) => {
                self.chromium.mark_unavailable();
                match self.dispose_or_terminate().await {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(error.context(format!(
                        "opening-page cleanup failed; context remains owned: {cleanup:#}"
                    ))),
                }
            }
        };
        let native = self.native_status.checkpoint().await.and_then(|count| {
            let (accepted, probes, rejected_connects) = self.native_status.counts();
            let (non_connects, invalid_connects, incomplete_connects) = self.native_status.denial_kinds();
            ensure!(
                count == baseline,
                "browser acquisition incomplete: native request was blocked (accepted={accepted}, benign_probes={probes}, rejected_connects={rejected_connects}, non_connects={non_connects}, invalid_connects={invalid_connects}, incomplete_connects={incomplete_connects}, denied_before={baseline}, denied_after={count})"
            );
            Ok(())
        });
        let result = finish_page_fetch(result, native);
        match reply.send(result) {
            Ok(()) => Ok(()),
            Err(Ok(_)) => {
                tracing::debug!("browser caller cancelled after acquisition cleanup completed");
                Ok(())
            }
            Err(Err(error)) => {
                tracing::warn!(%error, "cancelled browser acquisition ended after owned cleanup");
                Ok(())
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
            .and_then(|result| result);
        let (result, interceptor) = match installed {
            Ok(mut interceptor) => {
                let operation = timeout(
                    self.deadline,
                    interceptor.run(Browser::fetch_page(
                        &page,
                        &self.target,
                        &self.headers,
                        self.interval,
                    )),
                );
                let result = tokio::select! {
                    () = reply.closed() => Err(anyhow!("browser acquisition cancelled by caller")),
                    result = operation => result.context("browser request timed out").and_then(|result| result),
                };
                (result, Some(interceptor))
            }
            Err(error) => (Err(error), None),
        };
        self.teardown(page, interceptor, result).await
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
            #[cfg(test)]
            self.local_origin.clone(),
            #[cfg(test)]
            self.additional_origin.clone(),
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
        if let Some(ready) = &self.capture_ready {
            ready.notify_one();
        }
        let prepared = match interceptor.as_mut() {
            Some(interceptor) => {
                let complete = result.as_ref().is_ok_and(|capture| {
                    let capture = capture.capture();
                    !access_stop_status(capture.status)
                        && !crate::transport::denied_capture(capture)
                });
                interceptor.prepare_close(&page, complete).await
            }
            None => Ok(()),
        };
        let result = match result {
            Ok(capture) if prepared.is_ok() => capture.finish(&page).await,
            Ok(capture) => Ok(capture.into_capture()),
            Err(error) => Err(error),
        };
        let result = finish_page_fetch(result, prepared);
        let result = match interceptor.as_ref() {
            Some(interceptor) => interceptor.preserve_stop(result),
            None => result,
        };
        let target = page.target_id().clone();
        let closing = async {
            #[cfg(test)]
            if self.fail_page_close {
                return Err(anyhow!("injected page-close failure"));
            }
            page.close().await.map_err(anyhow::Error::from)
        };
        let closed = timeout(COMMAND_TIMEOUT, closing)
            .await
            .context("closing browser page timed out")
            .and_then(|result| result);
        if closed.is_err()
            && let Err(emergency) = self.dispose_or_terminate().await
        {
            self.chromium.mark_unavailable();
            if let Some(interceptor) = interceptor {
                self.chromium.retain_interception(interceptor).await?;
            }
            return Err(
                emergency.context("browser page remains owned pending owned-process cleanup")
            );
        }
        let access_stop = result
            .as_ref()
            .is_ok_and(|capture| access_stop_status(capture.status));
        let drained = match interceptor {
            Some(interceptor) => interceptor.finish(access_stop).await,
            None => Ok(()),
        };
        let cleanup = closed.and(drained);
        let confirmed = self.chromium.confirm_page_closed(&target).await;
        if confirmed.is_err() {
            self.chromium.mark_unavailable();
            self.dispose_or_terminate().await.context("native target verification failed and emergency cleanup failed; scope remains owned")?;
        }
        finish_page_fetch(result, cleanup.and(confirmed))
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
