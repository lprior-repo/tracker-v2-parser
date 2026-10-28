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
            Duration::from_secs(1),
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
            elapsed >= Duration::from_millis(990),
            "{} -> {} received only {elapsed:?} apart",
            pair[0].0,
            pair[1].0
        );
    }
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
