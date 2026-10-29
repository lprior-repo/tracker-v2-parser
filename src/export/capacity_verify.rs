use std::path::Path;

use anyhow::{Context, Result, ensure};
use calamine::{DataRef, Reader, Xlsx, open_workbook};

use super::{MAX_CELL_UNITS, SheetCount};

pub fn verify(path: &Path, expected: &[SheetCount]) -> Result<()> {
    ensure!(
        std::fs::metadata(path)?.len() <= crate::export::MAX_EXPORT_BYTES,
        "XLSX exceeds 8 GiB workbook capacity"
    );
    let mut workbook: Xlsx<_> =
        open_workbook(path).context("reopening temporary XLSX for verification")?;
    ensure!(
        workbook.sheet_names().len() == expected.len(),
        "XLSX worksheet count mismatch"
    );
    expected.iter().try_for_each(|sheet| {
        let expected_rows = sheet
            .rows
            .checked_add(1)
            .context("worksheet count overflow")?;
        let maximum_cells = u64::from(expected_rows)
            .checked_mul(16_384)
            .context("worksheet cell count overflow")?;
        let mut reader = workbook.worksheet_cells_reader(&sheet.name)?;
        let mut ended = false;
        let mut seen = 0_u64;
        let height = std::iter::from_fn(|| {
            if ended {
                return None;
            }
            match reader.next_cell() {
                Ok(Some(cell)) => Some(Ok(cell)),
                Ok(None) => {
                    ended = true;
                    None
                }
                Err(error) => {
                    ended = true;
                    Some(Err(error))
                }
            }
        })
        .take(usize::try_from(maximum_cells)?.saturating_add(1))
        .try_fold(0_u32, |height, cell| -> Result<u32> {
            let cell = cell?;
            seen = seen
                .checked_add(1)
                .context("worksheet cell count overflow")?;
            ensure!(
                seen <= maximum_cells,
                "XLSX cell count exceeds worksheet capacity"
            );
            let (row, column) = cell.get_position();
            ensure!(
                row < expected_rows && column < 16_384,
                "XLSX worksheet position exceeds expected capacity"
            );
            let valid = match cell.get_value() {
                DataRef::String(value) => value.encode_utf16().count() <= MAX_CELL_UNITS,
                DataRef::SharedString(value) => value.encode_utf16().count() <= MAX_CELL_UNITS,
                _ => true,
            };
            ensure!(valid, "XLSX {} contains overlong cell text", sheet.name);
            Ok(height.max(row.saturating_add(1)))
        })?;
        ensure!(
            height == expected_rows,
            "XLSX {} row count mismatch",
            sheet.name
        );
        drop(reader);
        let hyperlinks = workbook.hyperlinks_by_sheet_name(&sheet.name)?;
        ensure!(
            hyperlinks.len() == sheet.hyperlinks,
            "XLSX {} hyperlink count mismatch",
            sheet.name
        );
        Ok(())
    })
}
