use anyhow::{Context, Result, ensure};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::network::{
        self, EventLoadingFinished, EventResponseReceived, GetResponseBodyParams, ResourceType,
    },
};
use futures::StreamExt;
use tokio::time::timeout;

use super::{COMMAND_TIMEOUT, decode_body};
use crate::{
    model::Capture,
    transport::{
        AccessStopCaptureError, MAX_BODY_BYTES, access_stop_status, denied_capture, denied_url,
    },
};

pub(super) enum PageCapture {
    Navigation(Capture),
    Response(Capture),
}

impl PageCapture {
    pub const fn capture(&self) -> &Capture {
        match self {
            Self::Navigation(capture) | Self::Response(capture) => capture,
        }
    }

    pub fn into_capture(self) -> Capture {
        match self {
            Self::Navigation(capture) | Self::Response(capture) => capture,
        }
    }

    pub async fn finish(self, page: &Page) -> Result<Capture> {
        match self {
            Self::Navigation(capture) => render(page, capture).await,
            Self::Response(capture) => Ok(capture),
        }
    }
}

pub(super) async fn navigate(page: &Page, url: &str) -> Result<Capture> {
    render(page, document(page, url).await?).await
}

pub(super) async fn document(page: &Page, url: &str) -> Result<Capture> {
    let limit = i64::try_from(MAX_BODY_BYTES).context("browser buffer limit")?;
    page.execute(
        network::EnableParams::builder()
            .max_resource_buffer_size(limit)
            .max_total_buffer_size(limit.saturating_mul(2))
            .build(),
    )
    .await?;
    let frame = page
        .mainframe()
        .await?
        .context("browser page has no main frame")?;
    let responses = page.event_listener::<EventResponseReceived>().await?;
    let finished = page.event_listener::<EventLoadingFinished>().await?;
    page.goto(url).await.context("navigating browser page")?;
    let response = responses
        .filter(|event| {
            futures::future::ready(
                event.r#type == ResourceType::Document && event.frame_id.as_ref() == Some(&frame),
            )
        })
        .next()
        .await
        .context("browser navigation ended without a document response")?;
    let status =
        u16::try_from(response.response.status).context("invalid browser response status")?;
    let body_result = timeout(COMMAND_TIMEOUT, async {
        let request_id = response.request_id.clone();
        finished
            .filter(|event| futures::future::ready(event.request_id == request_id))
            .next()
            .await
            .context("browser document did not finish loading")?;
        let result = page
            .execute(GetResponseBodyParams::new(request_id))
            .await
            .context("capturing browser document")?
            .result;
        let body = if result.base64_encoded {
            decode_body(&result.body)?
        } else {
            result.body.into_bytes()
        };
        ensure!(
            body.len() <= MAX_BODY_BYTES,
            "source response exceeded 16 MiB limit"
        );
        Ok::<_, anyhow::Error>(body)
    })
    .await
    .context("capturing browser document timed out")
    .and_then(std::convert::identity);
    let body = body_result.map_err(|error| {
        if access_stop_status(status) || denied_url(&response.response.url) {
            AccessStopCaptureError { status, error }.into()
        } else {
            error
        }
    })?;
    Ok(Capture {
        url: response.response.url.clone(),
        status,
        content_type: response.response.mime_type.clone(),
        body,
    })
}

async fn render(page: &Page, capture: Capture) -> Result<Capture> {
    if !capture.content_type.contains("html")
        || !(200..300).contains(&capture.status)
        || denied_capture(&capture)
    {
        return Ok(capture);
    }
    let rendered: String = page
        .evaluate_function(RENDER_SCRIPT)
        .await
        .context("capturing rendered page")?
        .into_value()
        .context("decoding rendered page")?;
    Ok(Capture {
        body: decode_body(&rendered)?,
        ..capture
    })
}

const RENDER_SCRIPT: &str = r"async function() {
    if (document.readyState !== 'complete') await new Promise(resolve => window.addEventListener('load', resolve, {once: true}));
    const bytes = new TextEncoder().encode(document.documentElement.outerHTML);
    if (bytes.length > 16777216) throw new Error('source response exceeded 16 MiB limit');
    let binary = '';
    for (let offset = 0; offset < bytes.length; offset += 8192) binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
    return btoa(binary);
}";
