use futures::{TryStreamExt, stream};
use restate_sdk::prelude::*;
use serde_json::json;

use super::AthleticOperations;
use crate::durable::{
    effects, model,
    operations::{self, OperationRequest, OperationResult, OperationSpec, Workspace},
};

#[restate_sdk::service(
    idempotency_retention = "90 days",
    journal_retention = "90 days",
    invocation_retry_policy(
        initial_interval = "1s",
        factor = 2.0,
        max_interval = "5m",
        max_attempts = 10000,
        on_max_attempts = "pause"
    )
)]
impl AthleticOperations {
    #[handler]
    async fn execute(
        &self,
        ctx: Context<'_>,
        Json(request): Json<OperationRequest>,
    ) -> HandlerResult<Json<OperationResult>> {
        model::identifier(&request.operation_id).terminal()?;
        let workspace = effects::record!(&ctx, "bind-operation-input", || async {
            Workspace::open(
                &self.runtime.artifact_root,
                &request.operation_id,
                &request.spec,
            )
        })?;
        let result = match &request.spec {
            OperationSpec::Export { config } => self.export(&ctx, &workspace, config).await?,
            OperationSpec::WiaaDirectory { config } => self.wiaa(&ctx, &workspace, config).await?,
            OperationSpec::OfflineReplay {
                config,
                target,
                capture,
            } => {
                let now = effects::now!(&ctx)?;
                let outcome = effects::record!(&ctx, "replay-retained-capture", || async {
                    let guard = effects::acquisition(&self.runtime.database).await?;
                    let bytes = workspace.read(capture)?;
                    let scope = crate::durable::page::PageScope {
                        run_generation: request.operation_id.clone(),
                        config_sha256: model::page_config_digest(config)?,
                        deadline_ms: now.saturating_add(86_400_000),
                        cooldown_ms: 15_000,
                    };
                    let result = crate::durable::page::offline_replay(
                        &self.runtime.database,
                        &self.runtime.artifact_root,
                        &scope,
                        target.as_ref().clone(),
                        crate::model::Capture {
                            url: target.url.clone(),
                            status: 200,
                            content_type: "text/html".into(),
                            body: bytes,
                        },
                    )
                    .await;
                    guard.rollback().await?;
                    result
                })?;
                effects::record!(&ctx, "complete-replay-receipt", || async {
                    workspace.complete(&serde_json::to_value(outcome)?, None)
                })?
            }
            spec => {
                if let OperationSpec::DownloadPublicSchools { expected_sha256 } = spec {
                    effects::record!(&ctx, "download-verified-reference", || async {
                        let guard = effects::acquisition(&self.runtime.database).await?;
                        let result = operations::download_reference(
                            &workspace,
                            expected_sha256,
                            self.runtime.database.pool().clone(),
                        )
                        .await;
                        guard.rollback().await?;
                        result
                    })?;
                }
                let value = effects::record!(&ctx, "commit-import-and-receipt", || {
                    operations::execute_import(&workspace, spec, &self.runtime.database_url)
                })?;
                effects::record!(&ctx, "complete-import-receipt", || async {
                    workspace.complete(&value, None)
                })?
            }
        };
        Ok(Json(result))
    }

    async fn export(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        config: &crate::config::Config,
    ) -> HandlerResult<OperationResult> {
        let frozen = effects::record!(ctx, "freeze-export-input", || operations::freeze_export(
            workspace,
            config,
            &self.runtime.database_url
        ))?;
        let count = effects::record!(ctx, "read-frozen-chunk-plan", || async {
            Ok(operations::frozen_plan(workspace, &frozen)?.chunks.len())
        })?;
        stream::iter((0..count).map(Ok::<_, HandlerError>))
            .try_for_each(|index| {
                let owned = workspace.clone();
                let frozen = frozen.clone();
                async move {
                    effects::record!(
                        ctx,
                        &format!("prepare-athlete-chunk-{index:06}"),
                        || async move {
                            self.runtime
                                .blocking
                                .run(move || {
                                    operations::prepare_export_chunk(&owned, &frozen, index)
                                })
                                .await
                        }
                    )?;
                    Ok(())
                }
            })
            .await?;
        let owned = workspace.clone();
        let plan = frozen.clone();
        let workbook = effects::record!(ctx, "assemble-workbook", || async move {
            self.runtime
                .blocking
                .run(move || operations::render_export(&owned, &plan))
                .await
        })?;
        let owned = workspace.clone();
        let verified = effects::record!(ctx, "verify-workbook", || async move {
            self.runtime
                .blocking
                .run(move || operations::verify_export(&owned, &frozen, &workbook))
                .await
        })?;
        let owned = workspace.clone();
        let published = effects::record!(ctx, "publish-verified-workbook", || async move {
            self.runtime
                .blocking
                .run(move || operations::publish_export(&owned, &verified))
                .await
        })?;
        if let Some(path) = &self.runtime.published_workbook {
            let path = path.clone();
            let export = published.clone();
            effects::record!(ctx, "deliver-verified-workbook", || async move {
                self.runtime
                    .blocking
                    .run(move || crate::durable::delivery::publish(&path, &export))
                    .await
            })?;
        }
        effects::record!(ctx, "complete-export-receipt", || async {
            workspace.complete(
                &json!({"publication": published}),
                Some(&published.workbook),
            )
        })
    }
}
