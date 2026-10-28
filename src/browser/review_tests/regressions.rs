use super::*;

#[tokio::test]
async fn given_cross_authority_login_redirect_when_rejected_then_denied_authority_survives()
-> Result<()> {
    let login = Fixture::start(BTreeMap::new()).await?;
    let denied_url = format!("{}/login", login.origin);
    let origin = Fixture::start(BTreeMap::from([(
        "/redirect".into(),
        (
            302,
            "text/plain".into(),
            String::new(),
            format!("Location: {denied_url}\r\n"),
        ),
    )]))
    .await?;
    let profile = tempfile::tempdir()?;
    let mut browser = browser(&origin.origin, profile.path()).await?;
    browser.additional_origin = Some(url::Url::parse(&login.origin)?.origin());
    let initial_pages = browser.chromium.pages().await?.len();
    let result = browser
        .fetch(
            &target(&origin.origin, "/redirect"),
            &BTreeMap::new(),
            Duration::ZERO,
        )
        .await;
    let remaining_pages = browser.chromium.pages().await?.len();
    let origin_requests = origin.recorded()?;
    let login_requests = login.recorded()?;
    let login_connections = login.connections.load(Ordering::SeqCst);
    let closed = browser.close().await;
    let origin_closed = origin.close().await;
    let login_closed = login.close().await;
    closed.and(origin_closed).and(login_closed)?;
    assert!(
        origin_requests
            .iter()
            .any(|request| request.starts_with("GET /redirect "))
    );
    assert!(login_requests.is_empty());
    assert_eq!(login_connections, 0);
    assert_eq!(remaining_pages, initial_pages);
    let error = result.expect_err(
        "rejected redirect must retain a typed stop instead of synthetic origin capture",
    );
    assert!(error.is::<crate::transport::AccessStop>(), "{error:#}");
    assert!(
        format!("{error:#}")
            .contains(&url::Url::parse(&denied_url)?.origin().ascii_serialization()),
        "{error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn given_eight_scripts_and_redirect_when_acquired_then_wire_receives_are_paced() -> Result<()>
{
    paced_scripts_and_redirect(Duration::from_secs(1)).await
}

#[tokio::test]
async fn given_two_rps_and_eight_scripts_when_redirected_then_browser_wire_is_paced_at_the_selected_rate()
-> Result<()> {
    paced_scripts_and_redirect(Duration::from_millis(500)).await
}

#[tokio::test]
async fn given_three_rps_and_eight_scripts_when_redirected_then_browser_wire_is_paced_at_the_selected_rate()
-> Result<()> {
    paced_scripts_and_redirect(Duration::from_secs_f64(1.0 / 3.0)).await
}

async fn paced_scripts_and_redirect(interval: Duration) -> Result<()> {
    let scripts = (0..8)
        .map(|index| format!("<script src='/resource-{index}'></script>"))
        .collect::<String>();
    let fixture = Fixture::start(BTreeMap::from([
        ("/page".into(), html(scripts)),
        (
            "/redirect".into(),
            (
                302,
                "text/plain".into(),
                String::new(),
                "Location: /page\r\n".into(),
            ),
        ),
    ]))
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&fixture.origin, profile.path()).await?;
    let result = browser
        .fetch(
            &target(&fixture.origin, "/redirect"),
            &BTreeMap::new(),
            interval,
        )
        .await;
    let received = fixture
        .received
        .lock()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
        .clone();
    let closed = browser.close().await;
    let fixture_closed = fixture.close().await;
    closed.and(fixture_closed)?;
    assert_eq!(result?.status, 200);
    let observed = received
        .iter()
        .filter(|(path, _)| {
            path == "/redirect" || path == "/page" || path.starts_with("/resource-")
        })
        .collect::<Vec<_>>();
    assert_eq!(observed.len(), 10);
    for pair in observed.windows(2) {
        let elapsed = pair[1].1.duration_since(pair[0].1);
        assert!(
            elapsed >= interval.saturating_sub(Duration::from_millis(10)),
            "{} -> {} received only {elapsed:?} apart",
            pair[0].0,
            pair[1].0
        );
    }
    let total = observed[9].1.duration_since(observed[0].1);
    assert!(
        total < interval * 9 + Duration::from_millis(2500),
        "browser wire rate did not honor selected interval {interval:?}: {total:?}"
    );
    Ok(())
}

#[tokio::test]
async fn given_successful_document_and_denied_script_when_acquired_then_stop_remains_typed()
-> Result<()> {
    for status in [403, 429] {
        denied_script(status).await?;
    }
    Ok(())
}

async fn denied_script(status: u16) -> Result<()> {
    let fixture = Fixture::start(BTreeMap::from([
        (
            "/page".into(),
            html("<script src='/denied.js'></script><script src='/after.js'></script>".into()),
        ),
        (
            "/denied.js".into(),
            (
                status,
                "text/javascript".into(),
                String::new(),
                String::new(),
            ),
        ),
    ]))
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&fixture.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let result = browser
        .fetch(
            &target(&fixture.origin, "/page"),
            &BTreeMap::new(),
            Duration::from_millis(50),
        )
        .await;
    let remaining_pages = browser.chromium.pages().await?.len();
    let origin = fixture.origin.clone();
    let requests = fixture.recorded()?;
    let closed = browser.close().await;
    let fixture_closed = fixture.close().await;
    closed.and(fixture_closed)?;
    let error = result.expect_err("denied subresource must stop acquisition");
    let stop = error.downcast_ref::<crate::transport::AccessStop>();
    assert_eq!(stop.and_then(|stop| stop.status), Some(status), "{error:#}");
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /denied.js "))
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.starts_with("GET /after.js "))
    );
    let failure = crate::transport::failure(&error);
    assert!(failure.blocked);
    assert_eq!(failure.stopped_origin.as_deref(), Some(origin.as_str()));
    assert_eq!(remaining_pages, initial_pages);
    Ok(())
}

#[tokio::test]
async fn given_bootstrap_script_and_api_when_acquired_then_all_wire_requests_are_paced()
-> Result<()> {
    let fixture = Fixture::start(BTreeMap::from([
        (
            "/bootstrap".into(),
            html("<script src='/boot.js'></script>".into()),
        ),
        (
            "/boot.js".into(),
            (200, "text/javascript".into(), String::new(), String::new()),
        ),
        (
            "/api".into(),
            (200, "application/json".into(), "{}".into(), String::new()),
        ),
    ]))
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&fixture.origin, profile.path()).await?;
    let mut target = target(&fixture.origin, "/api");
    target.browser_origin = Some(format!("{}/bootstrap", fixture.origin));
    let result = browser
        .fetch(&target, &BTreeMap::new(), Duration::from_secs(1))
        .await;
    let received = fixture
        .received
        .lock()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
        .clone();
    let closed = browser.close().await;
    let fixture_closed = fixture.close().await;
    closed.and(fixture_closed)?;
    assert_eq!(result?.status, 200);
    let received = received
        .iter()
        .filter(|(path, _)| ["/bootstrap", "/boot.js", "/api"].contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(received.len(), 3);
    for pair in received.windows(2) {
        assert!(
            pair[1].1.duration_since(pair[0].1) >= Duration::from_millis(990),
            "{} -> {}",
            pair[0].0,
            pair[1].0
        );
    }
    Ok(())
}

#[tokio::test]
async fn given_denial_headers_and_stalled_body_when_page_deadline_cancels_then_stop_survives()
-> Result<()> {
    for (path, status) in [("/hang-denied", 403), ("/hang-throttled", 429)] {
        let fixture = Fixture::start(BTreeMap::from([(
            "/page".into(),
            html(format!("<script src='{path}'></script>")),
        )]))
        .await?;
        let origin = fixture.origin.clone();
        let profile = tempfile::tempdir()?;
        let mut browser = browser(&origin, profile.path()).await?;
        browser.page_deadline = Some(Duration::from_millis(500));
        let initial_pages = browser.chromium.pages().await?.len();
        let result = timeout(
            Duration::from_secs(10),
            browser.fetch(&target(&origin, "/page"), &BTreeMap::new(), Duration::ZERO),
        )
        .await;
        let remaining_pages = browser.chromium.pages().await?.len();
        let requests = fixture.recorded()?;
        let closed = browser.close().await;
        let fixture_closed = fixture.close().await;
        closed.and(fixture_closed)?;
        assert!(
            requests
                .iter()
                .any(|request| request.starts_with(&format!("GET {path} ")))
        );
        assert_eq!(remaining_pages, initial_pages);
        let error = result
            .context("page-deadline fixture watchdog")?
            .expect_err("stalled denial cannot succeed");
        let stop = error
            .downcast_ref::<crate::transport::AccessStop>()
            .context(format!("typed stop lost: {error:#}"))?;
        assert_eq!(stop.status, Some(status));
        let failure = crate::transport::failure(&error);
        assert!(failure.blocked);
        assert_eq!(failure.stopped_origin.as_deref(), Some(origin.as_str()));
    }
    Ok(())
}

#[tokio::test]
async fn given_file_login_redirect_when_policy_rejects_then_original_authority_remains_durable()
-> Result<()> {
    let fixture = Fixture::start(BTreeMap::from([(
        "/redirect".into(),
        (
            302,
            "text/plain".into(),
            String::new(),
            "Location: file:///login\r\n".into(),
        ),
    )]))
    .await?;
    let origin = fixture.origin.clone();
    let profile = tempfile::tempdir()?;
    let browser = browser(&origin, profile.path()).await?;
    let result = browser
        .fetch(
            &target(&origin, "/redirect"),
            &BTreeMap::new(),
            Duration::ZERO,
        )
        .await;
    let requests = fixture.recorded()?;
    let closed = browser.close().await;
    let fixture_closed = fixture.close().await;
    closed.and(fixture_closed)?;
    let failure = match result {
        Ok(capture) => crate::transport::check_response(capture)
            .expect_err("rejected file redirect must block"),
        Err(error) => crate::transport::failure(&error),
    };
    assert!(failure.blocked);
    assert_ne!(failure.stopped_origin.as_deref(), Some("null"));
    let address = failure
        .stopped_origin
        .as_deref()
        .or_else(|| failure.capture.as_ref().map(|capture| capture.url.as_str()))
        .context("durable stop must identify an HTTP authority")?;
    assert_eq!(
        url::Url::parse(address)?.origin().ascii_serialization(),
        origin
    );
    assert_eq!(requests.len(), 1);
    Ok(())
}

#[tokio::test]
async fn given_robots_redirect_when_authority_has_trailing_dot_then_stop_is_normalized()
-> Result<()> {
    let peer = Fixture::start(BTreeMap::new()).await?;
    let port = url::Url::parse(&peer.origin)?
        .port()
        .context("fixture port")?;
    let address = format!("http://www.milesplit.com.:{port}/api/v1/test");
    let failure = robots_redirect(&address, Some(url::Url::parse(&address)?.origin())).await?;
    let requests = peer.recorded()?;
    let connections = peer.connections.load(Ordering::SeqCst);
    peer.close().await?;
    assert!(failure.blocked);
    assert_eq!(
        failure.stopped_origin,
        Some(format!("http://www.milesplit.com:{port}").into_boxed_str())
    );
    assert!(requests.is_empty());
    assert_eq!(connections, 0);
    Ok(())
}

#[tokio::test]
async fn given_non_http_robots_redirect_when_rejected_then_http_stop_remains_durable() -> Result<()>
{
    for address in [
        "ftp://www.milesplit.com/api/v1/test",
        "file://www.milesplit.com/api/v1/test",
    ] {
        let failure = robots_redirect(address, None).await?;
        assert!(failure.blocked);
        let origin = failure.stopped_origin.context("HTTP stop authority")?;
        let url = url::Url::parse(&origin)?;
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
    }
    Ok(())
}

async fn robots_redirect(
    address: &str,
    peer: Option<url::Origin>,
) -> Result<crate::transport::Failure> {
    let fixture = Fixture::start(BTreeMap::from([(
        "/redirect".into(),
        (
            302,
            "text/plain".into(),
            String::new(),
            format!("Location: {address}\r\n"),
        ),
    )]))
    .await?;
    let profile = tempfile::tempdir()?;
    let mut browser = browser(&fixture.origin, profile.path()).await?;
    browser.additional_origin = peer;
    let result = browser
        .fetch(
            &target(&fixture.origin, "/redirect"),
            &BTreeMap::new(),
            Duration::ZERO,
        )
        .await;
    let requests = fixture.recorded()?;
    let closed = browser.close().await;
    let fixture_closed = fixture.close().await;
    closed.and(fixture_closed)?;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /redirect "));
    let error = result.expect_err("robots redirect must preserve typed stop");
    assert!(error.is::<crate::transport::AccessStop>(), "{error:#}");
    Ok(crate::transport::failure(&error))
}
