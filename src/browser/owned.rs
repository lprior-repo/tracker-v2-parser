use super::{COMMAND_TIMEOUT, interception::Interception};
use anyhow::{Context, Result, ensure};
use chromiumoxide::{
    Browser as Chromium, Page,
    cdp::browser_protocol::{
        browser::BrowserContextId,
        target::TargetId,
        target::{
            CreateBrowserContextParams, CreateTargetParams, FilterEntry, GetTargetsParams,
            TargetFilter, TargetInfo,
        },
    },
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{sync::Mutex, time::timeout};

pub(super) struct OwnedChromium {
    endpoint: String,
    state: Mutex<State>,
    retained: Mutex<Option<Interception>>,
    page: Mutex<Option<Page>>,
    unavailable: AtomicBool,
    shutdown_requested: Arc<AtomicBool>,
    #[cfg(test)]
    faults: Faults,
}

struct State {
    chromium: Chromium,
    context: Option<BrowserContextId>,
    owned_context: Option<BrowserContextId>,
    terminated: bool,
}

pub(super) struct CleanupScope {
    page: Option<Page>,
    interceptor: Option<Interception>,
}

impl CleanupScope {
    pub async fn finish(self, access_stop: bool) -> Result<()> {
        let drained = match self.interceptor {
            Some(interceptor) => interceptor.finish(access_stop).await,
            None => Ok(()),
        };
        drop(self.page);
        drained
    }
}

#[cfg(test)]
#[derive(Default)]
struct Faults {
    partial_install: AtomicBool,
    dispose: AtomicBool,
    terminate: AtomicBool,
}

struct PageOpening<'a> {
    unavailable: &'a AtomicBool,
    complete: bool,
}

impl Drop for PageOpening<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.unavailable.store(true, Ordering::Release);
        }
    }
}

impl OwnedChromium {
    pub fn new(chromium: Chromium) -> Self {
        Self {
            endpoint: chromium.websocket_address().clone(),
            state: Mutex::new(State {
                chromium,
                context: None,
                owned_context: None,
                terminated: false,
            }),
            retained: Mutex::new(None),
            page: Mutex::new(None),
            unavailable: AtomicBool::new(false),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            faults: Faults::default(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn shutdown_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown_requested)
    }

    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    pub async fn new_page(&self, params: CreateTargetParams) -> Result<Page> {
        ensure!(
            !self.unavailable.load(Ordering::Acquire),
            "owned Chromium is unavailable after failed cleanup"
        );
        let mut page_guard = self.page.lock().await;
        ensure!(
            page_guard.is_none(),
            "owned Chromium page capacity exceeded"
        );
        ensure!(
            self.retained.lock().await.is_none(),
            "owned Chromium has a retained failed acquisition"
        );
        let mut opening = PageOpening {
            unavailable: &self.unavailable,
            complete: false,
        };
        let state = self.state.lock().await;
        ensure!(!state.terminated, "owned Chromium was terminated");
        ensure!(
            state.context.is_some() && state.context.as_ref() == params.browser_context_id.as_ref(),
            "requested page context is not owned by this browser"
        );
        let page = state.chromium.new_page(params).await?;
        drop(state);
        *page_guard = Some(page.clone());
        opening.complete = true;
        drop(opening);
        drop(page_guard);
        Ok(page)
    }

    pub async fn create_browser_context(
        &self,
        params: CreateBrowserContextParams,
    ) -> Result<BrowserContextId> {
        ensure!(
            !self.unavailable.load(Ordering::Acquire),
            "owned Chromium is unavailable after failed cleanup"
        );
        let mut state = self.state.lock().await;
        ensure!(
            !state.terminated && state.context.is_none(),
            "owned Chromium context is unavailable"
        );
        let context = state.chromium.create_browser_context(params).await?;
        state.context = Some(context.clone());
        state.owned_context = Some(context.clone());
        drop(state);
        Ok(context)
    }

    pub async fn dispose_browser_context(&self, context: BrowserContextId) -> Result<()> {
        #[cfg(test)]
        ensure!(
            !self.faults.dispose.load(Ordering::Acquire),
            "injected context disposal failure"
        );
        let mut state = self.state.lock().await;
        if state.terminated || state.context.as_ref() != Some(&context) {
            return Ok(());
        }
        state.chromium.dispose_browser_context(context).await?;
        state.context = None;
        drop(state);
        drop(self.page.lock().await.take());
        Ok(())
    }

    pub async fn terminate(&self) -> Result<()> {
        #[cfg(test)]
        ensure!(
            !self.faults.terminate.load(Ordering::Acquire),
            "injected owned-process termination failure"
        );
        let mut state = self.state.lock().await;
        if state.terminated {
            return Ok(());
        }
        self.request_shutdown();
        timeout(COMMAND_TIMEOUT, state.chromium.kill())
            .await
            .context("terminating owned Chromium timed out")?
            .context("owned Chromium has no child process")??;
        state.context = None;
        state.terminated = true;
        drop(state);
        drop(self.page.lock().await.take());
        Ok(())
    }

    pub fn into_inner(self) -> (Chromium, Option<CleanupScope>) {
        let page = self.page.into_inner();
        let interceptor = self.retained.into_inner();
        let scope = if page.is_some() || interceptor.is_some() {
            Some(CleanupScope { page, interceptor })
        } else {
            None
        };
        (self.state.into_inner().chromium, scope)
    }

    pub fn mark_unavailable(&self) {
        self.unavailable.store(true, Ordering::Release);
    }

    pub async fn confirm_page_closed(&self, target: &TargetId) -> Result<()> {
        let targets = self.live_targets().await?;
        ensure!(
            !targets.iter().any(|entry| &entry.target_id == target),
            "owned Chromium target {target:?} remained alive after teardown"
        );
        let mut page = self.page.lock().await;
        if page.as_ref().is_some_and(|page| page.target_id() == target) {
            drop(page.take());
        }
        drop(page);
        Ok(())
    }

    async fn live_targets(&self) -> Result<Vec<TargetInfo>> {
        let state = self.state.lock().await;
        if state.terminated {
            return Ok(Vec::new());
        }
        let params = GetTargetsParams::builder()
            .filter(TargetFilter::new(vec![FilterEntry::default()]))
            .build();
        let targets = state.chromium.execute(params).await?.result.target_infos;
        drop(state);
        Ok(targets)
    }

    #[cfg(test)]
    pub fn partial_install_failure(&self) -> bool {
        self.faults.partial_install.load(Ordering::Acquire)
    }

    pub async fn retain_interception(&self, interceptor: Interception) -> Result<()> {
        self.mark_unavailable();
        let mut retained = self.retained.lock().await;
        ensure!(
            retained.is_none(),
            "retained browser acquisition capacity exceeded"
        );
        *retained = Some(interceptor);
        drop(retained);
        Ok(())
    }

    pub async fn release_retained(&self) -> Result<()> {
        if !self.unavailable.load(Ordering::Acquire) && self.retained.lock().await.is_none() {
            return Ok(());
        }
        self.terminate().await?;
        let interceptor = self.retained.lock().await.take();
        match interceptor {
            Some(interceptor) => interceptor.finish(false).await,
            None => Ok(()),
        }
    }

    #[cfg(test)]
    pub async fn pages(&self) -> Result<Vec<TargetInfo>> {
        let context = self.state.lock().await.owned_context.clone();
        let Some(context) = context else {
            return Ok(Vec::new());
        };
        Ok(self
            .live_targets()
            .await?
            .into_iter()
            .filter(|entry| entry.browser_context_id.as_ref() == Some(&context))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        browser::{Browser, lifecycle::join_handler},
        model::{PageContext, PageKind, Source, Target},
    };
    use std::{collections::BTreeMap, time::Duration};

    fn inject_cleanup_failures(owner: &OwnedChromium, enabled: bool) {
        owner
            .faults
            .partial_install
            .store(enabled, Ordering::Release);
        owner.faults.dispose.store(enabled, Ordering::Release);
        owner.faults.terminate.store(enabled, Ordering::Release);
    }

    async fn assert_failed_partial_scope(browser: &Browser, target: &Target) -> Result<()> {
        let error = browser
            .fetch(target, &BTreeMap::new(), Duration::ZERO)
            .await
            .err()
            .context("partial installation and failed cleanup must not capture successfully")?;
        ensure!(
            format!("{error:#}").contains("page remains owned"),
            "cleanup failure did not retain its ownership diagnostic"
        );
        ensure!(
            browser.chromium.unavailable.load(Ordering::Acquire),
            "cleanup failure did not poison Chromium"
        );
        ensure!(
            browser.chromium.retained.lock().await.is_none(),
            "scenario unexpectedly completed interception installation"
        );
        let target_id = browser
            .chromium
            .page
            .lock()
            .await
            .as_ref()
            .map(|page| page.target_id().clone())
            .context("partial installation lost its opened-page guard")?;
        ensure!(
            browser
                .chromium
                .pages()
                .await?
                .iter()
                .any(|page| page.target_id == target_id),
            "guarded page was not actually alive during injected cleanup failures"
        );
        ensure!(
            browser.fetches.lock().await.is_empty(),
            "acquisition supervisor was not joined"
        );
        let params = CreateTargetParams::builder()
            .url("about:blank")
            .browser_context_id(browser.context.clone().context("owned context missing")?)
            .build()
            .map_err(anyhow::Error::msg)?;
        let rejected = browser
            .chromium
            .new_page(params)
            .await
            .err()
            .context("poisoned browser accepted another page")?;
        ensure!(
            format!("{rejected:#}").contains("unavailable after failed cleanup"),
            "new acquisition was not rejected by the terminal unavailable state"
        );
        Ok(())
    }

    async fn assert_reclaimed(owner: &OwnedChromium) -> Result<()> {
        owner.release_retained().await?;
        ensure!(
            owner.page.lock().await.is_none(),
            "confirmed termination retained its page guard"
        );
        ensure!(
            owner.retained.lock().await.is_none(),
            "confirmed termination retained interception tasks"
        );
        let mut state = owner.state.lock().await;
        ensure!(
            state.terminated && state.context.is_none(),
            "owned process was not marked terminated"
        );
        let exited = state.chromium.wait().await?;
        drop(state);
        ensure!(exited.is_some(), "owned Chromium was not reaped");
        Ok(())
    }

    #[tokio::test]
    async fn given_partial_install_and_all_cleanup_failures_when_acquiring_then_page_is_retained_and_browser_is_unavailable()
    -> Result<()> {
        let canary = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", canary.local_addr()?);
        let profile = tempfile::tempdir()?;
        let mut browser = Browser::start(None, profile.path(), true).await?;
        browser.local_origin = Some(url::Url::parse(&origin)?.origin());
        browser.fail_page_close = true;
        inject_cleanup_failures(&browser.chromium, true);
        let target = Target::get(
            Source::AthleticNet,
            PageKind::Contacts,
            format!("{origin}/partial"),
            PageContext::default(),
        );
        let scenario = assert_failed_partial_scope(&browser, &target).await;
        inject_cleanup_failures(&browser.chromium, false);
        let reclaimed = assert_reclaimed(&browser.chromium).await;
        let terminated = browser.chromium.terminate().await;
        browser.handler.abort();
        let handler = join_handler(browser.handler).await;
        let native = browser.native_sink.finish().await;
        scenario
            .and(reclaimed)
            .and(terminated)
            .and(handler)
            .and(native)
    }
}

#[cfg(test)]
mod inventory_tests {
    use super::*;
    use crate::{
        browser::Browser,
        model::{PageContext, PageKind, Source, Target},
    };
    use std::collections::BTreeMap;

    async fn assert_authoritative_inventory(browser: &Browser) -> Result<()> {
        let control = {
            let state = browser.chromium.state.lock().await;
            state.chromium.new_page("about:blank").await?
        };
        let control_id = control.target_id().clone();
        ensure!(
            browser
                .chromium
                .live_targets()
                .await?
                .iter()
                .any(|entry| entry.target_id == control_id),
            "native inventory omitted the live default-context control"
        );
        ensure!(
            browser.chromium.pages().await?.is_empty(),
            "default-context control was attributed to the acquisition context"
        );
        let params = CreateTargetParams::builder()
            .url("about:blank")
            .browser_context_id(browser.context.clone().context("owned context missing")?)
            .build()
            .map_err(anyhow::Error::msg)?;
        let page = browser.chromium.new_page(params).await?;
        let target_id = page.target_id().clone();
        ensure!(
            browser
                .chromium
                .pages()
                .await?
                .iter()
                .any(|entry| entry.target_id == target_id),
            "native owned inventory omitted the opened acquisition page"
        );
        let target = Target::get(
            Source::AthleticNet,
            PageKind::Athlete,
            "https://www.athletic.net/".to_owned(),
            PageContext::default(),
        );
        let interceptor = Interception::install(
            &page,
            &target,
            &BTreeMap::new(),
            browser.chromium.endpoint(),
            None,
            None,
        )
        .await?;
        page.close().await?;
        interceptor.finish(false).await?;
        browser.chromium.confirm_page_closed(&target_id).await?;
        let actual = browser.chromium.live_targets().await?;
        ensure!(
            !actual.iter().any(|entry| entry.target_id == target_id),
            "actual Chromium retained the closed acquisition target"
        );
        ensure!(
            actual.iter().any(|entry| entry.target_id == control_id),
            "acquisition cleanup closed the unrelated control target"
        );
        ensure!(
            browser.chromium.pages().await?.is_empty(),
            "actual owned acquisition targets remain alive"
        );
        control.close().await?;
        Ok(())
    }

    #[tokio::test]
    async fn given_unrelated_default_page_when_acquisition_closes_then_native_inventory_proves_only_owned_target_is_absent()
    -> Result<()> {
        let profile = tempfile::tempdir()?;
        let browser = Browser::start(None, profile.path(), true).await?;
        let scenario = assert_authoritative_inventory(&browser).await;
        let closed = browser.close().await;
        scenario.and(closed)
    }
}
