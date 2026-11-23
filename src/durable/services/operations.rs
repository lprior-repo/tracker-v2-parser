mod export;

use restate_sdk::prelude::*;

use super::AthleticOperations;
use crate::{
    config::Config,
    durable::{
        effects, model,
        operations::{self, Artifact, OperationRequest, OperationResult, OperationSpec, Workspace},
        page::{self, PageOutcome, PageScope},
    },
    model::{Capture, Target},
};

struct ReplayInput<'a> {
    operation_id: &'a str,
    config: &'a Config,
    target: &'a Target,
    capture: &'a Artifact,
}

#[restate_sdk::service(
    inactivity_timeout = "10m",
    abort_timeout = "2m",
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
        let workspace = self.bind_workspace(&ctx, &request).await?;
        let result = match &request.spec {
            OperationSpec::Export { config } => self.export(&ctx, &workspace, config).await,
            OperationSpec::WiaaDirectory { config } => {
                self.wiaa(&ctx, &workspace, config, None).await
            }
            OperationSpec::WiaaDirectoryRun {
                config,
                deadline_ms,
                cooldown_ms,
            } => {
                let bounds = Some((*deadline_ms, *cooldown_ms));
                self.wiaa(&ctx, &workspace, config, bounds).await
            }
            _ => self.import_or_replay(&ctx, &workspace, &request).await,
        };
        result.map(Json)
    }
}

impl AthleticOperations {
    async fn bind_workspace(
        &self,
        ctx: &Context<'_>,
        request: &OperationRequest,
    ) -> HandlerResult<Workspace> {
        model::identifier(&request.operation_id).terminal()?;
        effects::record!(ctx, "bind-operation-input", || async {
            Workspace::open(
                &self.runtime.artifact_root,
                &request.operation_id,
                &request.spec,
            )
        })
    }

    async fn import_or_replay(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        request: &OperationRequest,
    ) -> HandlerResult<OperationResult> {
        match &request.spec {
            OperationSpec::OfflineReplay {
                config,
                target,
                capture,
            } => {
                let input = ReplayInput {
                    operation_id: &request.operation_id,
                    config,
                    target,
                    capture,
                };
                self.replay(ctx, workspace, input).await
            }
            spec => self.import(ctx, workspace, spec).await,
        }
    }

    async fn replay(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        input: ReplayInput<'_>,
    ) -> HandlerResult<OperationResult> {
        let now = effects::now!(ctx)?;
        let outcome = effects::record!(ctx, "replay-retained-capture", || {
            self.replay_retained_capture(workspace, &input, now)
        })?;
        effects::record!(ctx, "complete-replay-receipt", || async {
            workspace.complete(&serde_json::to_value(outcome)?, None)
        })
    }

    async fn replay_retained_capture(
        &self,
        workspace: &Workspace,
        input: &ReplayInput<'_>,
        now: i64,
    ) -> anyhow::Result<PageOutcome> {
        let guard = effects::acquisition(&self.runtime.database).await?;
        let bytes = workspace.read(input.capture)?;
        let scope = input.scope(now)?;
        let result = page::offline_replay(
            &self.runtime.database,
            page::PageFiles::new(&self.runtime.artifact_root, &self.runtime.blocking),
            &scope,
            input.target.clone(),
            input.response(bytes),
        )
        .await;
        guard.rollback().await?;
        result
    }

    async fn import(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        spec: &OperationSpec,
    ) -> HandlerResult<OperationResult> {
        if let OperationSpec::DownloadPublicSchools { expected_sha256 } = spec {
            effects::record!(ctx, "download-verified-reference", || {
                self.download_reference(workspace, expected_sha256)
            })?;
        }
        let value = effects::record!(ctx, "commit-import-and-receipt", || {
            operations::execute_import(
                workspace,
                spec,
                &self.runtime.database_url,
                &self.runtime.blocking,
            )
        })?;
        effects::record!(ctx, "complete-import-receipt", || async {
            workspace.complete(&value, None)
        })
    }

    async fn download_reference(
        &self,
        workspace: &Workspace,
        expected_sha256: &str,
    ) -> anyhow::Result<Artifact> {
        let guard = effects::acquisition(&self.runtime.database).await?;
        let result = operations::download_reference(
            workspace,
            expected_sha256,
            self.runtime.database.pool().clone(),
            &self.runtime.blocking,
        )
        .await;
        guard.rollback().await?;
        result
    }
}

impl ReplayInput<'_> {
    fn scope(&self, now: i64) -> anyhow::Result<PageScope> {
        Ok(PageScope {
            inherited_retry_campaigns: Vec::new(),
            run_generation: self.operation_id.to_owned(),
            config_sha256: model::page_config_digest(self.config)?,
            deadline_ms: now.saturating_add(86_400_000),
            cooldown_ms: 15_000,
        })
    }

    fn response(&self, body: Vec<u8>) -> Capture {
        Capture {
            url: self.target.url.clone(),
            status: 200,
            content_type: "text/html".into(),
            body,
        }
    }
}
