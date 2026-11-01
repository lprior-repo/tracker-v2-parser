use anyhow::{Context, Result, ensure};
use sqlx::PgConnection;

use crate::durable::{page::PageRequest, retry_lineage};

pub(super) async fn attempts_used(
    connection: &mut PgConnection,
    request: &PageRequest,
) -> Result<i64> {
    let scope = &request.binding.scope;
    retry_lineage::validate(
        retry_lineage::campaign(&scope.run_generation),
        &scope.inherited_retry_campaigns,
    )?;
    let roots = std::iter::once(retry_lineage::campaign(&scope.run_generation))
        .chain(scope.inherited_retry_campaigns.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let families = retry_lineage::families(roots.iter().copied());
    let (attempts, missing_ancestor): (i64, bool) = sqlx::query_as(
        "WITH related AS MATERIALIZED (SELECT operation_id,binding FROM durable_page_operations d WHERE d.page_id=$1 AND (d.run_generation=ANY($2::text[]) OR COALESCE(d.binding->'scope'->'inherited_retry_campaigns','[]'::jsonb) ?| $3::text[])) SELECT (SELECT count(*) FROM durable_page_attempts a JOIN related USING(operation_id)), EXISTS(SELECT 1 FROM related parent CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(parent.binding->'scope'->'inherited_retry_campaigns','[]'::jsonb)) inherited(campaign) WHERE NOT inherited.campaign=ANY($3::text[]))"
    )
        .bind(request.binding.page_id)
        .bind(&families)
        .bind(&roots)
        .fetch_one(connection)
        .await
        .context("counting inherited durable acquisition attempts")?;
    ensure!(
        !missing_ancestor,
        "retry lineage omits a retained ancestor campaign"
    );
    Ok(attempts)
}
