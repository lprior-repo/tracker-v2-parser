use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};

use super::{io, runtime::Runtime};

impl Runtime {
    pub async fn register(&self) -> Result<Value> {
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
        io::save(&self.args.evidence.join("registration.json"), &result)?;
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
        Ok(json!({"service":"AthleticOperations","inactivity_timeout":"10m","abort_timeout":"2m"}))
    }
}
