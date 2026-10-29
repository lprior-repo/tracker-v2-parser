mod directory;

use anyhow::{Context as _, Result, ensure};
use futures::{TryStreamExt, stream};
use restate_sdk::prelude::*;
use serde_json::{Value, json};

use super::{
    effects, model,
    operations::{OperationResult, Workspace},
    page::{self, PageOutcome, PageRequest, PageScope},
    services::{AthleticOperations, AthleticPageClient, PageExecution},
};
use crate::{
    config::{Config, TransportMode},
    model::{PageContext, PageKind, ParsedPage, Source, Target},
};

pub(super) use directory::{is_index, parse_index};

impl AthleticOperations {
    pub(super) async fn wiaa(
        &self,
        ctx: &Context<'_>,
        workspace: &Workspace,
        config: &Config,
    ) -> HandlerResult<OperationResult> {
        ensure_wisconsin(config).terminal()?;
        effects::record!(ctx, "check-wiaa-access-stops", || {
            crate::wiaa_enrichment::check_stops(&self.runtime.database)
        })?;
        let mut config = config.clone();
        config.sources.milesplit.transport = TransportMode::Http;
        config.sources.athleticnet.transport = TransportMode::Http;
        let now = effects::now!(ctx)?;
        let scope = PageScope {
            run_generation: model::digest(&(workspace.binding(), "wiaa-directory")).terminal()?,
            config_sha256: model::digest(&config).terminal()?,
            deadline_ms: now
                .checked_add(86_400_000)
                .ok_or_else(|| TerminalError::new("WIAA deadline overflow"))?,
            cooldown_ms: 60_000,
        };
        let source = config
            .enabled_sources()
            .into_iter()
            .find(|source| *source == Source::MileSplit)
            .or_else(|| config.enabled_sources().first().copied())
            .ok_or_else(|| TerminalError::new("WIAA requires an enabled provider"))?;
        let root = Target::get(
            source,
            PageKind::Contacts,
            "https://schools.wiaawi.org/Directory/School/List".into(),
            PageContext {
                state: Some("WI".into()),
                season: Some(config.season),
                subject_id: Some("wiaa-directory-index".into()),
                ..Default::default()
            },
        );
        let first = self.wiaa_index(ctx, &config, &scope, root, 0).await?;
        if !matches!(first.1, PageOutcome::Completed { .. }) {
            return effects::record!(ctx, "complete-wiaa-incomplete-root", || async {
                workspace.complete(&json!({"complete":false,"outcomes":[first.1]}), None)
            });
        }
        let root_page = effects::record!(ctx, "read-wiaa-published-letters", || self
            .wiaa_page(&first.0))?;
        let (_, outcomes) = stream::iter(
            root_page
                .next
                .into_iter()
                .enumerate()
                .map(Ok::<_, HandlerError>),
        )
        .try_fold(
            (1_usize, vec![first]),
            |(ordinal, mut outcomes), (_, target)| {
                let config = &config;
                let scope = &scope;
                async move {
                    outcomes.push(self.wiaa_index(ctx, config, scope, target, ordinal).await?);
                    Ok((
                        ordinal
                            .checked_add(1)
                            .ok_or_else(|| TerminalError::new("WIAA index count overflow"))?,
                        outcomes,
                    ))
                }
            },
        )
        .await?;
        let report = effects::record!(ctx, "commit-wiaa-directory-mapping", || self
            .wiaa_mapping(workspace, &config, &outcomes))?;
        effects::record!(ctx, "complete-wiaa-directory-receipt", || async {
            workspace.complete(&report, None)
        })
    }

    async fn wiaa_index(
        &self,
        ctx: &Context<'_>,
        config: &Config,
        scope: &PageScope,
        target: Target,
        index: usize,
    ) -> HandlerResult<(PageRequest, PageOutcome)> {
        let request = effects::record!(ctx, &format!("bind-wiaa-index-{index:02}"), || {
            page::bind_target(&self.runtime.database, target.clone(), scope)
        })?;
        let outcome = stream::try_unfold(IndexState::Running(0), |state| {
            let request = &request;
            async move {
                let IndexState::Running(attempt) = state else {
                    return Ok::<_, HandlerError>(None);
                };
                if attempt >= 10_000 {
                    return Err(TerminalError::new("WIAA index invocation limit reached").into());
                }
                let Json(outcome) = ctx
                    .service_client::<AthleticPageClient>()
                    .handle(Json(PageExecution {
                        request: request.clone(),
                        config: config.clone(),
                    }))
                    .idempotency_key(format!("{}-{index:02}-{attempt:05}", scope.run_generation))
                    .call()
                    .await?;
                let next = match &outcome {
                    PageOutcome::Deferred { retry_at_ms, .. } => {
                        let now = effects::now!(ctx)?;
                        ctx.sleep(model::delay_until(now, *retry_at_ms).terminal()?)
                            .await?;
                        IndexState::Running(
                            attempt
                                .checked_add(1)
                                .ok_or_else(|| TerminalError::new("WIAA retry counter overflow"))?,
                        )
                    }
                    _ => IndexState::Complete,
                };
                Ok(Some((outcome, next)))
            }
        })
        .try_fold(None, |_, outcome| async { Ok(Some(outcome)) })
        .await?;
        Ok((
            request,
            outcome.ok_or_else(|| TerminalError::new("WIAA index has no outcome"))?,
        ))
    }

    async fn wiaa_page(&self, request: &PageRequest) -> Result<ParsedPage> {
        let parsed: serde_json::Value =
            sqlx::query_scalar("SELECT parsed FROM durable_page_receipts WHERE operation_id=$1")
                .bind(&request.binding.operation_id)
                .fetch_one(self.runtime.database.pool())
                .await?;
        let reference: page::ParsedRef = serde_json::from_value(parsed)?;
        let bytes = page::artifacts::read(
            &self.runtime.artifact_root,
            &reference.body,
            page::artifacts::MAX_PARSED_BYTES,
        )
        .await?;
        serde_json::from_slice(&bytes).context("reading retained WIAA directory parse")
    }

    async fn wiaa_mapping(
        &self,
        workspace: &Workspace,
        config: &Config,
        outcomes: &[(PageRequest, PageOutcome)],
    ) -> Result<Value> {
        ensure!(
            outcomes.len() == 27,
            "WIAA directory requires the published 27 index captures"
        );
        if outcomes
            .iter()
            .any(|(_, outcome)| !matches!(outcome, PageOutcome::Completed { .. }))
        {
            return Ok(
                json!({"complete":false,"outcomes":outcomes.iter().map(|(_,outcome)|outcome).collect::<Vec<_>>()}),
            );
        }
        let pages = stream::iter(outcomes.iter().map(Ok::<_, anyhow::Error>))
            .and_then(|(request, _)| self.wiaa_page(request))
            .try_collect::<Vec<_>>()
            .await?;
        let entries = directory::entries(&pages)?;
        let plan =
            directory::frozen_plan(workspace, &self.runtime.database, config, &entries).await?;
        let targets: Vec<Target> = serde_json::from_slice(&workspace.read(&plan.targets)?)?;
        self.runtime
            .database
            .durable_enqueue_receipt(
                &workspace.operation_key()?,
                &plan.targets.sha256,
                &targets,
                plan.report,
            )
            .await
    }
}

enum IndexState {
    Running(u32),
    Complete,
}

fn ensure_wisconsin(config: &Config) -> Result<()> {
    ensure!(
        config.states.iter().any(|state| state == "WI") && config.acquisition.includes_contacts(),
        "WIAA enrichment requires Wisconsin and contact collection"
    );
    Ok(())
}
