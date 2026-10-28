use anyhow::{Context, Result};
use calamine::{Data, Reader, Xlsx, open_workbook};
use rust_xlsxwriter::{Color, Format, Url, Workbook, Worksheet};
use std::path::Path;

const MAX_DATA_ROWS: u32 = 1_048_575;
const MAX_HYPERLINKS: usize = 65_530;
const MAX_WORKSHEETS: usize = 65_530;
pub(super) const MAX_CELL_UNITS: usize = 32_767;
pub(super) const MAX_PR_KEYS: usize = 5_455;

struct CellDetail {
    worksheet: String,
    row: u32,
    column: u16,
    field: String,
    value: String,
}

pub(super) struct SheetCount {
    name: String,
    rows: u32,
    hyperlinks: usize,
    details: Vec<CellDetail>,
}

struct Sheet {
    worksheet: Worksheet,
    rows: u32,
    hyperlinks: usize,
    details: Vec<CellDetail>,
}

pub(super) fn write(
    workbook: &mut Workbook,
    base: &str,
    headers: &[String],
    rows: impl Iterator<Item = Vec<String>>,
    autofilter: bool,
    expected: &mut Vec<SheetCount>,
) -> Result<()> {
    write_fallible(workbook, base, headers, rows.map(Ok), autofilter, expected)
}

pub(super) fn write_fallible(
    workbook: &mut Workbook,
    base: &str,
    headers: &[String],
    mut rows: impl Iterator<Item = Result<Vec<String>>>,
    autofilter: bool,
    expected: &mut Vec<SheetCount>,
) -> Result<()> {
    let first = new_sheet(workbook, base, 1, headers)?;
    let (_, sheet) = rows.try_fold((1_u32, first), |(part, sheet), cells| {
        let cells = cells?;
        anyhow::ensure!(
            cells.len() == headers.len(),
            "worksheet row/header column count mismatch"
        );
        let hyperlinks = cells
            .iter()
            .zip(headers)
            .filter(|(text, header)| is_hyperlink(header, text))
            .count();
        anyhow::ensure!(
            hyperlinks <= MAX_HYPERLINKS,
            "row exceeds hyperlink capacity"
        );
        let combined = sheet
            .hyperlinks
            .checked_add(hyperlinks)
            .context("hyperlink count overflow")?;
        let (part, mut sheet) = if sheet.rows == MAX_DATA_ROWS || combined > MAX_HYPERLINKS {
            finish(workbook, sheet, headers.len(), autofilter, expected)?;
            let part = part
                .checked_add(1)
                .context("worksheet part number overflow")?;
            (part, new_sheet(workbook, base, part, headers)?)
        } else {
            (part, sheet)
        };
        write_row(&mut sheet, headers, cells, hyperlinks)?;
        Ok::<_, anyhow::Error>((part, sheet))
    })?;
    finish(workbook, sheet, headers.len(), autofilter, expected)
}

fn new_sheet(workbook: &mut Workbook, base: &str, part: u32, headers: &[String]) -> Result<Sheet> {
    anyhow::ensure!(
        headers.len() <= 16_384,
        "too many comparable event columns for Excel"
    );
    let name = if part == 1 {
        base.to_owned()
    } else {
        let name = format!("{base}_{part}");
        if name.len() > 31 {
            format!(
                "A_{}_{part}",
                base.strip_prefix("Athlete_")
                    .context("worksheet name exceeds Excel limit")?
            )
        } else {
            name
        }
    };
    let mut worksheet = workbook.new_worksheet_with_constant_memory()?;
    worksheet.set_name(name)?;
    let format = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0x17_36_5D))
        .set_font_color(Color::White)
        .set_text_wrap();
    headers.iter().enumerate().try_for_each(|(column, text)| {
        anyhow::ensure!(
            text.encode_utf16().count() <= MAX_CELL_UNITS,
            "worksheet header exceeds Excel character capacity"
        );
        worksheet.write_string_with_format(0, u16::try_from(column)?, text, &format)?;
        Ok::<(), anyhow::Error>(())
    })?;
    worksheet.set_freeze_panes(1, 0)?;
    worksheet.set_row_height(0, 32)?;
    if let Some(last) = headers.len().checked_sub(1) {
        worksheet.set_column_range_width(0, u16::try_from(last)?, 24)?;
    }
    Ok(Sheet {
        worksheet,
        rows: 0,
        hyperlinks: 0,
        details: Vec::new(),
    })
}

fn is_hyperlink(header: &str, text: &str) -> bool {
    (matches!(
        header,
        "source_profiles" | "profile_url" | "source_url" | "Profile"
    ) || header.ends_with("::source_url"))
        && (text.starts_with("https://") || text.starts_with("http://"))
        && !text.contains("; ")
        && text.encode_utf16().count() <= 2_080
}

fn write_row(
    sheet: &mut Sheet,
    headers: &[String],
    cells: Vec<String>,
    hyperlinks: usize,
) -> Result<()> {
    let row = sheet
        .rows
        .checked_add(1)
        .context("worksheet row overflow")?;
    cells
        .into_iter()
        .zip(headers)
        .enumerate()
        .try_for_each(|(column, (text, header))| {
            let column = u16::try_from(column)?;
            if text.encode_utf16().count() > MAX_CELL_UNITS {
                sheet.worksheet.write_string(
                    row,
                    column,
                    format!(
                        "Complete text: Cell_details; worksheet={} row={} column={}",
                        sheet.worksheet.name(),
                        row.saturating_add(1),
                        u32::from(column).saturating_add(1)
                    ),
                )?;
                sheet.details.push(CellDetail {
                    worksheet: sheet.worksheet.name(),
                    row,
                    column,
                    field: header.clone(),
                    value: text,
                });
            } else if is_hyperlink(header, &text) {
                sheet
                    .worksheet
                    .write_url(row, column, Url::new(text.as_str()))?;
            } else {
                sheet.worksheet.write_string(row, column, &text)?;
            }
            Ok::<(), anyhow::Error>(())
        })?;
    sheet.rows = row;
    sheet.hyperlinks = sheet
        .hyperlinks
        .checked_add(hyperlinks)
        .context("hyperlink count overflow")?;
    Ok(())
}

fn finish(
    workbook: &mut Workbook,
    mut sheet: Sheet,
    columns: usize,
    autofilter: bool,
    expected: &mut Vec<SheetCount>,
) -> Result<()> {
    anyhow::ensure!(
        expected.len() < MAX_WORKSHEETS,
        "workbook exceeds {MAX_WORKSHEETS} worksheets; reduce rows_per_file or disable per_athlete_sheets"
    );
    if let Some(last) = columns.checked_sub(1).filter(|_| autofilter) {
        sheet
            .worksheet
            .autofilter(0, 0, sheet.rows, u16::try_from(last)?)?;
    }
    expected.push(SheetCount {
        name: sheet.worksheet.name(),
        rows: sheet.rows,
        hyperlinks: sheet.hyperlinks,
        details: sheet.details,
    });
    workbook.push_worksheet(sheet.worksheet);
    Ok(())
}

pub(super) fn write_details(workbook: &mut Workbook, expected: &mut Vec<SheetCount>) -> Result<()> {
    let details = expected
        .iter_mut()
        .flat_map(|sheet| std::mem::take(&mut sheet.details))
        .collect::<Vec<_>>();
    if details.is_empty() {
        return Ok(());
    }
    let headers = ["worksheet", "row", "column", "field", "part", "text"].map(str::to_owned);
    let rows = details.into_iter().flat_map(|detail| {
        chunks(&detail.value)
            .enumerate()
            .map(|(part, text)| {
                vec![
                    detail.worksheet.clone(),
                    detail.row.saturating_add(1).to_string(),
                    u32::from(detail.column).saturating_add(1).to_string(),
                    detail.field.clone(),
                    part.saturating_add(1).to_string(),
                    text.to_owned(),
                ]
            })
            .collect::<Vec<_>>()
    });
    write(workbook, "Cell_details", &headers, rows, false, expected)
}

pub(super) fn chunks(value: &str) -> impl Iterator<Item = &str> {
    let mut remaining = value;
    std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        let boundary = remaining.floor_char_boundary(8_000);
        let (chunk, rest) = remaining.split_at(boundary);
        remaining = rest;
        Some(chunk)
    })
}

pub(super) fn verify(path: &Path, expected: &[SheetCount]) -> Result<()> {
    let mut workbook: Xlsx<_> =
        open_workbook(path).context("reopening temporary XLSX for verification")?;
    anyhow::ensure!(
        workbook.sheet_names().len() == expected.len(),
        "XLSX worksheet count mismatch"
    );
    expected.iter().try_for_each(|sheet| {
        let range = workbook
            .worksheet_range(&sheet.name)
            .with_context(|| format!("verifying {} sheet", sheet.name))?;
        let rows = usize::try_from(sheet.rows)?
            .checked_add(1)
            .context("worksheet count overflow")?;
        anyhow::ensure!(
            range.height() == rows,
            "XLSX {} row count mismatch",
            sheet.name
        );
        anyhow::ensure!(
            range.rows().flatten().all(|cell| match cell {
                Data::String(value) => value.encode_utf16().count() <= MAX_CELL_UNITS,
                _ => true,
            }),
            "XLSX {} contains overlong cell text",
            sheet.name
        );
        let hyperlinks = workbook.hyperlinks_by_sheet_name(&sheet.name)?;
        anyhow::ensure!(
            hyperlinks.len() == sheet.hyperlinks,
            "XLSX {} hyperlink count mismatch",
            sheet.name
        );
        Ok(())
    })
}
