use futures::{TryStreamExt, stream};
use restate_sdk::prelude::*;
use serde_json::json;

use crate::{
    config::Config,
    durable::{
        effects,
        operations::{
            self, FrozenExport, OperationResult, PublishedExport, VerifiedExport, Workspace,
        },
        services::AthleticOperations,
    },
};

impl AthleticOperations {
    pub(super) async fn export(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        config: &Config,
    ) -> HandlerResult<OperationResult> {
        let frozen = effects::record!(ctx, "freeze-export-input", || operations::freeze_export(
            workspace,
            config,
            &self.runtime.database_url,
            &self.runtime.blocking,
        ))?;
        self.prepare_export_chunks(ctx, workspace, &frozen).await?;
        let verified = self
            .assemble_verified_export(ctx, workspace, frozen)
            .await?;
        self.publish_export_receipt(ctx, workspace, verified).await
    }

    async fn prepare_export_chunks(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        frozen: &FrozenExport,
    ) -> HandlerResult<()> {
        let count = effects::record!(ctx, "read-frozen-chunk-plan", || async {
            Ok(operations::frozen_plan(workspace, frozen)?.chunks.len())
        })?;
        stream::iter((0..count).map(Ok::<_, HandlerError>))
            .try_for_each(|index| self.prepare_export_chunk(ctx, workspace, frozen, index))
            .await
    }

    async fn prepare_export_chunk(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        frozen: &FrozenExport,
        index: usize,
    ) -> HandlerResult<()> {
        let owned = workspace.clone();
        let frozen = frozen.clone();
        effects::record!(
            ctx,
            &format!("prepare-athlete-chunk-{index:06}"),
            || async move {
                self.runtime
                    .blocking
                    .run(move || operations::prepare_export_chunk(&owned, &frozen, index))
                    .await
            }
        )?;
        Ok(())
    }

    async fn assemble_verified_export(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        frozen: FrozenExport,
    ) -> HandlerResult<VerifiedExport> {
        let owned = workspace.clone();
        let plan = frozen.clone();
        let workbook = effects::record!(ctx, "assemble-workbook", || async move {
            self.runtime
                .blocking
                .run(move || operations::render_export(&owned, &plan))
                .await
        })?;
        let owned = workspace.clone();
        effects::record!(ctx, "verify-workbook", || async move {
            self.runtime
                .blocking
                .run(move || operations::verify_export(&owned, &frozen, &workbook))
                .await
        })
    }

    async fn publish_export_receipt(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        verified: VerifiedExport,
    ) -> HandlerResult<OperationResult> {
        let owned = workspace.clone();
        let published = effects::record!(ctx, "publish-verified-workbook", || async move {
            self.runtime
                .blocking
                .run(move || operations::publish_export(&owned, &verified))
                .await
        })?;
        self.deliver_published_workbook(ctx, workspace, &published)
            .await?;
        effects::record!(ctx, "complete-export-receipt", || async {
            workspace.complete(
                &json!({"publication": published}),
                Some(&published.workbook),
            )
        })
    }

    async fn deliver_published_workbook(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        published: &PublishedExport,
    ) -> HandlerResult<()> {
        let Some(path) = &self.runtime.published_workbook else {
            return Ok(());
        };
        let path = path.clone();
        let export = published.clone();
        let owned = workspace.clone();
        effects::record!(ctx, "deliver-verified-workbook", || async move {
            self.runtime
                .blocking
                .run(move || crate::durable::delivery::publish_configured(&path, &owned, &export))
                .await
        })?;
        Ok(())
    }
}
