use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};

use super::{io, runtime::Runtime};

pub(super) const PAGE_SERVICE: &str = "/services/AthleticPage";
pub(super) const PAGE_SEND: &str = "/restate/send/AthleticPage/handle";

impl Runtime {
    pub async fn register(&self) -> Result<Value> {
        self.register_named("registration.json").await
    }

    pub async fn register_named(&self, label: &str) -> Result<Value> {
        let until = io::deadline(Duration::from_secs(90))?;
        let result = stream::iter(0..360)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(None, |found, _| async move {
                if found.is_some() {
                    return Ok(found);
                }
                ensure!(
                    tokio::time::Instant::now() < until,
                    "native registration exceeded90second deadline"
                );
                if let Ok((status, value)) = self
                    .http
                    .request(
                        &self.admin,
                        "/deployments",
                        Some(&json!({"uri":self.endpoint})),
                        None,
                    )
                    .await
                {
                    if matches!(status, 200 | 201) {
                        return Ok(Some(value));
                    }
                    ensure!(
                        matches!(status, 429 | 502 | 503 | 504),
                        "owned registration failed HTTP{status}"
                    );
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok(None)
            })
            .await?
            .context("owned worker registration exceeded bound")?;
        io::save(&self.args.evidence.join(label), &result)?;
        Ok(result)
    }

    pub async fn settings(&self) -> Result<Value> {
        let (status, value) = self
            .http
            .request(&self.admin, "/services/AthleticOperations", None, None)
            .await?;
        ensure!(
            status == 200
                && io::text(&value, "/inactivity_timeout")? == "10m"
                && io::text(&value, "/abort_timeout")? == "2m",
            "registered Operations timeout mismatch"
        );
        let page = self.verify_page_privacy().await?;
        Ok(
            json!({"service":"AthleticOperations","inactivity_timeout":"10m","abort_timeout":"2m",
            "page":page}),
        )
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn verify_page_privacy(&self) -> Result<Value> {
        ensure!(!self.page_public().await?, "AthleticPage must be private");
        let (status, rejection) = self
            .http
            .request(&self.ingress, PAGE_SEND, Some(&json!({})), None)
            .await?;
        ensure!(
            status == 400
                && io::text(&rejection, "/message")? == "the invoked service is not public",
            "direct AthleticPage submission was not rejected by Restate (HTTP {status})"
        );
        Ok(json!({"public":false,"direct_submit_status":status}))
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn page_public(&self) -> Result<bool> {
        let (status, service) = self
            .http
            .request(&self.admin, PAGE_SERVICE, None, None)
            .await?;
        ensure!(status == 200, "AthleticPage metadata HTTP {status}");
        service
            .get("public")
            .and_then(Value::as_bool)
            .context("AthleticPage visibility missing from metadata")
    }

    #[tracing::instrument(skip_all, fields(public))]
    pub(super) async fn set_page_public(&self, public: bool) -> Result<()> {
        let (status, service) = self
            .patch_owned(PAGE_SERVICE, Some(&json!({"public":public})))
            .await?;
        ensure!(
            status == 200 && service.get("public").and_then(Value::as_bool) == Some(public),
            "owned AthleticPage visibility update failed (HTTP {status})"
        );
        Ok(())
    }
}
