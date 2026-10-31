use super::*;

const CASES: [(&str, u16, &str); 5] = [
    (
        "/custom-success",
        218,
        "<p id=\"retained\">rendered-custom-success</p>",
    ),
    ("/custom-success-upper", 299, "custom-success-upper"),
    ("/upstream-timeout", 522, "upstream-timeout"),
    ("/upstream-certificate", 526, "upstream-certificate"),
    ("/custom-failure", 599, "custom-failure"),
];

fn routes() -> Routes {
    CASES
        .iter()
        .map(|(path, status, expected)| {
            let (kind, body) = if *status == 218 {
                (
                    "text/html",
                    "<html><body><p id='retained'>custom-success</p><script>document.getElementById('retained').textContent='rendered-custom-success'</script></body></html>",
                )
            } else {
                ("text/plain", *expected)
            };
            (
                (*path).into(),
                (*status, kind.into(), body.into(), String::new()),
            )
        })
        .collect()
}

#[tokio::test]
async fn given_nonstandard_upstream_status_when_fulfilling_browser_request_then_status_and_body_survive()
-> Result<()> {
    let fixture = Fixture::start(routes()).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&fixture.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        for (path, status, body) in CASES {
            let capture = browser
                .fetch(
                    &target(&fixture.origin, path),
                    &BTreeMap::new(),
                    Duration::ZERO,
                )
                .await
                .with_context(|| format!("preserving upstream status {status}"))?;
            assert_eq!(capture.status, status);
            assert!(String::from_utf8(capture.body)?.contains(body));
            assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
            assert_eq!(
                fixture
                    .recorded()?
                    .iter()
                    .filter(|request| request.starts_with(&format!("GET {path} ")))
                    .count(),
                1
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let fixture_closed = fixture.close().await;
    scenario.and(closed).and(fixture_closed)
}
