use super::super::manifest::{CORE_TABLES, Inventory, PREVIOUS_TABLES, TABLES, Table};
use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use std::collections::BTreeSet;

#[tracing::instrument(skip_all)]
pub async fn collect(connection: &mut PgConnection, count: usize) -> Result<Inventory> {
    let scope = tables_for_count(count)?;
    check_tables(connection, count).await?;
    let tables = stream::iter(scope.iter().copied().map(Ok::<_, anyhow::Error>))
        .try_fold(
            (connection, Vec::new()),
            |(connection, mut tables), (name, key)| async move {
                let statement = format!(
                    "COPY (SELECT to_jsonb(t)::text FROM public.{name} t ORDER BY {key}) TO STDOUT"
                );
                let (digest, rows) = connection
                    .copy_out_raw(&statement)
                    .await?
                    .map_err(anyhow::Error::from)
                    .try_fold(
                        (Sha256::new(), 0_u64),
                        |(mut digest, rows), block| async move {
                            digest.update(&block);
                            let count = block
                                .split(|byte| *byte == b'\n')
                                .count()
                                .checked_sub(1)
                                .context("invalid COPY chunk")?;
                            let rows = rows
                                .checked_add(u64::try_from(count)?)
                                .context("table row count overflow")?;
                            Ok::<_, anyhow::Error>((digest, rows))
                        },
                    )
                    .await?;
                tables.push(Table {
                    name: name.to_owned(),
                    rows,
                    sha256: format!("{:x}", digest.finalize()),
                });
                Ok((connection, tables))
            },
        )
        .await?
        .1;
    Ok(Inventory { tables })
}

#[tracing::instrument(skip_all)]
pub async fn check_tables(connection: &mut PgConnection, count: usize) -> Result<()> {
    let scope = tables_for_count(count)?;
    let actual = sqlx::query_scalar::<_, String>(
        "SELECT tablename::text FROM pg_tables WHERE schemaname='public'",
    )
    .fetch_all(connection)
    .await?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let expected = scope
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    let known = [
        CORE_TABLES
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect::<BTreeSet<_>>(),
        PREVIOUS_TABLES
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect::<BTreeSet<_>>(),
        TABLES
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect::<BTreeSet<_>>(),
    ];
    ensure!(
        actual == expected || (count == 7 && known.contains(&actual)),
        "public table set differs from known application schema; refusing unverified database scope"
    );
    Ok(())
}

fn tables_for_count(count: usize) -> Result<&'static [(&'static str, &'static str)]> {
    match count {
        7 => Ok(&CORE_TABLES),
        13 => Ok(&PREVIOUS_TABLES),
        14 => Ok(&TABLES),
        _ => anyhow::bail!("unsupported inventory scope"),
    }
}
