use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result, anyhow, ensure};
use futures::{TryStreamExt, stream};
use reqwest::{Client, Method, Response, header};
use url::Url;

use crate::model::{Capture, Target};

use super::{AccessStopCaptureError, MAX_BODY_BYTES, Transport, access_stop_status, policy};

enum Hop {
    Next(Url),
    Complete(Capture),
}

impl Transport {
    #[tracing::instrument(skip_all)]
    pub(super) async fn fetch_http(
        &mut self,
        target: &Target,
        headers: &BTreeMap<String, String>,
    ) -> Result<Capture> {
        tokio::time::timeout(Duration::from_secs(45), self.http_request(target, headers))
            .await
            .context("source HTTP request timed out")?
    }

    async fn http_request(
        &mut self,
        target: &Target,
        headers: &BTreeMap<String, String>,
    ) -> Result<Capture> {
        let url = policy::source_url(target, &target.url)?;
        let (_, state) = stream::iter((0..6).map(Ok::<_, anyhow::Error>))
            .try_fold((self, Hop::Next(url)), |(transport, state), _| async move {
                let next = match state {
                    Hop::Complete(capture) => Hop::Complete(capture),
                    Hop::Next(url) => transport.hop(target, headers, &url).await?,
                };
                Ok((transport, next))
            })
            .await?;
        match state {
            Hop::Complete(capture) => Ok(capture),
            Hop::Next(_) => Err(anyhow!("source exceeded five redirect limit")),
        }
    }

    #[tracing::instrument(skip_all)]
    async fn hop(
        &mut self,
        target: &Target,
        headers: &BTreeMap<String, String>,
        url: &Url,
    ) -> Result<Hop> {
        let client = self.client_for(url).await?;
        let method = Method::from_bytes(target.method.as_deref().map_or(b"GET", str::as_bytes))
            .context("parsing source HTTP method")?;
        ensure!(
            matches!(method, Method::GET | Method::POST),
            "source only supports GET and POST requests"
        );
        self.pace(target).await?;
        let mut request = client.request(method.clone(), url.clone());
        request = headers.iter().fold(request, |request, (name, value)| {
            request.header(name, value)
        });
        if let Some(body) = &target.body {
            request = request.json(body);
        }
        let response = request.send().await.context("fetching source response")?;
        if response.status().is_redirection() {
            return redirect(target, url, &response, &method, headers);
        }
        let status = response.status().as_u16();
        if access_stop_status(status) {
            return capture(response)
                .await
                .map(Hop::Complete)
                .map_err(|error| AccessStopCaptureError { status, error }.into());
        }
        capture(response).await.map(Hop::Complete)
    }

    #[tracing::instrument(skip_all)]
    async fn client_for(&mut self, url: &Url) -> Result<Client> {
        let origin = url.origin().ascii_serialization();
        if let Some(client) = self.clients.get(&origin) {
            return Ok(client.clone());
        }
        #[cfg(test)]
        if self.stall_dns {
            futures::future::pending::<()>().await;
        }
        let addresses = policy::public_addresses(url).await?;
        let host = url.host_str().context("source URL has no host")?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(45))
            .connect_timeout(Duration::from_secs(15))
            .user_agent("track-scrapper-v2/0.1 public athletics research")
            .resolve_to_addrs(host, &addresses)
            .build()
            .context("building pinned public HTTP client")?;
        self.clients.insert(origin, client.clone());
        Ok(client)
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use crate::{
        model::{PageContext, PageKind, Source},
        transport::{Mode, Options},
    };

    #[tokio::test(start_paused = true)]
    async fn given_stalled_dns_when_http_fetches_then_total_deadline_includes_resolution()
    -> Result<()> {
        let mut transport = Transport::new(Options {
            mode: Mode::Http,
            driver_url: None,
            profile: std::path::PathBuf::new(),
            headless: true,
            rps: 1.0,
        })?;
        transport.stall_dns = true;
        let target = Target::get(
            Source::MileSplit,
            PageKind::Athlete,
            "https://www.milesplit.com/athletes/fixture".into(),
            PageContext::default(),
        );
        let start = tokio::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(46), transport.fetch(&target)).await;
        let elapsed = start.elapsed();
        transport.close().await?;
        let failure = result
            .expect("transport's own 45 second deadline must beat fixture watchdog")
            .expect_err("stalled resolution must time out");
        assert_eq!(elapsed, Duration::from_secs(45));
        assert!(!failure.blocked);
        assert!(failure.capture.is_none());
        assert!(failure.detail.contains("timed out"), "{}", failure.detail);
        Ok(())
    }
}

fn redirect(
    target: &Target,
    previous: &Url,
    response: &Response,
    method: &Method,
    headers: &BTreeMap<String, String>,
) -> Result<Hop> {
    ensure!(
        method == Method::GET,
        "refusing to replay a redirected POST request"
    );
    let location = response
        .headers()
        .get(header::LOCATION)
        .context("redirect omitted Location")?
        .to_str()?;
    let next = previous
        .join(location)
        .context("resolving source redirect")?;
    policy::source_url(target, next.as_str())?;
    ensure!(
        previous.scheme() != "https" || next.scheme() == "https",
        "refusing HTTPS downgrade redirect"
    );
    ensure!(
        headers.is_empty() || next.host_str() == previous.host_str(),
        "refusing authenticated redirect to another host"
    );
    ensure!(
        !next.path_segments().is_some_and(|parts| parts
            .take(4)
            .any(|part| matches!(part, "login" | "signin" | "subscribe"))),
        "source redirected to authentication or subscription; human review required"
    );
    Ok(Hop::Next(next))
}

#[tracing::instrument(skip_all)]
async fn capture(response: Response) -> Result<Capture> {
    let url = response.url().to_string();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map_or(Ok(""), |value| value.to_str())?
        .to_owned();
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= 16_777_216),
        "source response exceeded 16 MiB limit"
    );
    let body = response
        .bytes_stream()
        .map_err(anyhow::Error::from)
        .try_fold(Vec::new(), |mut body, chunk| async move {
            let length = body
                .len()
                .checked_add(chunk.len())
                .context("source body length overflow")?;
            ensure!(
                length <= MAX_BODY_BYTES,
                "source response exceeded 16 MiB limit"
            );
            body.try_reserve(chunk.len())
                .context("allocating bounded source response")?;
            body.extend_from_slice(&chunk);
            Ok(body)
        })
        .await?;
    Ok(Capture {
        url,
        status,
        content_type,
        body,
    })
}
