use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result, ensure};
use calamine::{DataRef, Reader, Xlsx, open_workbook};

use super::{PreparedChunk, ReviewRow, review, single_sheet};

pub(super) fn verify_source(
    path: &Path,
    chunk_count: usize,
    load: impl Fn(usize) -> Result<PreparedChunk>,
    review_rows: Option<&[ReviewRow]>,
) -> Result<(usize, usize)> {
    ensure!(
        chunk_count <= 10_000,
        "prepared verification exceeds 10000 chunks"
    );
    let mut columns = BTreeSet::new();
    let mut header_bytes = 0_usize;
    let count = (0..chunk_count).try_fold(0_usize, |count, index| -> Result<usize> {
        let chunk = load(index)?;
        ensure!(
            chunk.rows.len() <= 1000 && chunk.rows.len() == chunk.athlete_ids.len(),
            "prepared verification chunk exceeds row bound or identity count"
        );
        chunk.rows.iter().try_for_each(|row| -> Result<()> {
            ensure!(
                row.identity.len() == single_sheet::HEADERS.len(),
                "prepared identity column count mismatch"
            );
            row.prs.keys().try_for_each(|column| -> Result<()> {
                if columns.insert(column.clone()) {
                    header_bytes = header_bytes
                        .checked_add(column.len())
                        .context("prepared header size overflow")?;
                    ensure!(
                        header_bytes <= 16 * 1024 * 1024
                            && columns.len().saturating_add(single_sheet::HEADERS.len()) <= 16_384,
                        "prepared verification headers exceed capacity"
                    );
                }
                Ok(())
            })
        })?;
        count
            .checked_add(chunk.rows.len())
            .filter(|rows| *rows <= 1_048_575)
            .context("prepared row count exceeds Excel capacity")
    })?;
    let headers = single_sheet::headers(&columns);
    let rows = (0..chunk_count)
        .map(load)
        .flat_map(|chunk| {
            let (rows, error) = match chunk {
                Ok(chunk) => (chunk.rows, None),
                Err(error) => (Vec::new(), Some(error)),
            };
            rows.into_iter().map(Ok).chain(error.map(Err))
        })
        .map(|row| single_sheet::cells(row?, &columns));
    verify_rows(path, &headers, rows, count, review_rows)?;
    Ok((count, headers.len()))
}

pub(super) fn verify_rows(
    path: &Path,
    headers: &[String],
    rows: impl Iterator<Item = Result<Vec<String>>>,
    count: usize,
    review_rows: Option<&[ReviewRow]>,
) -> Result<()> {
    let mut workbook: Xlsx<_> = open_workbook(path)?;
    let names = if review_rows.is_some() {
        vec!["Athletes", "Review"]
    } else {
        vec!["Athletes"]
    };
    ensure!(
        workbook.sheet_names() == names,
        "verified export worksheet set/order differs from frozen contract"
    );
    if let Some(rows) = review_rows {
        review::verify(path, rows)?;
    }
    let mut reader = workbook.worksheet_cells_reader("Athletes")?;
    let mut pending = reader.next_cell()?;
    let actual_rows = std::iter::once(Ok(headers.to_vec())).chain(rows).enumerate().try_fold(0_usize, |total, (row_index, row)| -> Result<usize> {
        let values = row?;
        ensure!(values.len() == headers.len(), "prepared verification row width mismatch");
        values.iter().enumerate().try_for_each(|(column, expected)| -> Result<()> {
            let position = (u32::try_from(row_index)?, u32::try_from(column)?);
            if let Some(cell) = pending.as_ref() {
                ensure!(cell.get_position() >= position, "workbook contains unexpected cell ordering");
                if cell.get_position() == position {
                    let actual = match cell.get_value() {
                        DataRef::String(value) => value.as_str(),
                        DataRef::SharedString(value) => value,
                        DataRef::Empty => "",
                        _ => anyhow::bail!("workbook cell type differs from frozen prepared athlete row"),
                    };
                    ensure!(actual == expected, "workbook cell differs from frozen prepared athlete row at row {} column {}", row_index.saturating_add(1), column.saturating_add(1));
                    pending = reader.next_cell()?;
                    return Ok(());
                }
            }
            ensure!(expected.is_empty(), "workbook is missing a frozen prepared athlete cell");
            Ok(())
        })?;
        total.checked_add(1).context("verified row count overflow")
    })?;
    ensure!(
        actual_rows
            == count
                .checked_add(1)
                .context("prepared row count overflow")?
            && pending.is_none(),
        "workbook dimensions differ from frozen prepared rows"
    );
    Ok(())
}
