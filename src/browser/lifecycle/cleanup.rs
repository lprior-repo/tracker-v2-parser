use anyhow::{Context, Result};
use chromiumoxide::{Page, cdp::browser_protocol::target::TargetId};
use tokio::time::timeout;

use super::{Acquisition, COMMAND_TIMEOUT, Interception, PageCapture, finish_page_fetch};
use crate::{
    model::Capture,
    transport::{access_stop_status, denied_capture},
};

impl Acquisition {
    pub(super) async fn prepare_capture(
        page: &Page,
        mut interceptor: Option<&mut Interception>,
        result: Result<PageCapture>,
    ) -> Result<Capture> {
        let prepared = match interceptor.as_mut() {
            Some(interceptor) => {
                let complete = result.as_ref().is_ok_and(capture_is_complete);
                interceptor.prepare_close(page, complete).await
            }
            None => Ok(()),
        };
        let result = match result {
            Ok(capture) if prepared.is_ok() => capture.finish(page).await,
            Ok(capture) => Ok(capture.into_capture()),
            Err(error) => Err(error),
        };
        let result = finish_page_fetch(result, prepared);
        match interceptor {
            Some(interceptor) => interceptor.preserve_stop(result),
            None => result,
        }
    }

    pub(super) async fn close_page(page: Page, #[cfg(test)] fail_page_close: bool) -> Result<()> {
        let closing = async {
            #[cfg(test)]
            if fail_page_close {
                return Err(anyhow::anyhow!("injected page-close failure"));
            }
            page.close().await.map_err(anyhow::Error::from)
        };
        timeout(COMMAND_TIMEOUT, closing)
            .await
            .context("closing browser page timed out")
            .and_then(std::convert::identity)
    }

    pub(super) async fn finish_teardown(
        &self,
        target: &TargetId,
        interceptor: Option<Interception>,
        result: Result<Capture>,
        closed: Result<()>,
    ) -> Result<Capture> {
        let interceptor = if closed.is_err() {
            self.recover_page_close(interceptor).await?
        } else {
            interceptor
        };
        let access_stop = result
            .as_ref()
            .is_ok_and(|capture| access_stop_status(capture.status));
        let drained = match interceptor {
            Some(interceptor) => interceptor.finish(access_stop).await,
            None => Ok(()),
        };
        self.verify_target_closed(target, result, closed.and(drained))
            .await
    }

    async fn recover_page_close(
        &self,
        interceptor: Option<Interception>,
    ) -> Result<Option<Interception>> {
        let Err(emergency) = self.dispose_or_terminate().await else {
            return Ok(interceptor);
        };
        self.chromium.mark_unavailable();
        if let Some(interceptor) = interceptor {
            self.chromium.retain_interception(interceptor).await?;
        }
        Err(emergency.context("browser page remains owned pending owned-process cleanup"))
    }

    async fn verify_target_closed(
        &self,
        target: &TargetId,
        result: Result<Capture>,
        cleanup: Result<()>,
    ) -> Result<Capture> {
        let confirmed = self.chromium.confirm_page_closed(target).await;
        if confirmed.is_err() {
            self.chromium.mark_unavailable();
            self.dispose_or_terminate()
                .await
                .context("native target verification failed and emergency cleanup failed; scope remains owned")?;
        }
        finish_page_fetch(result, cleanup.and(confirmed))
    }
}

fn capture_is_complete(page_capture: &PageCapture) -> bool {
    let capture = page_capture.capture();
    !access_stop_status(capture.status) && !denied_capture(capture)
}
