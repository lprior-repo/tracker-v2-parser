use anyhow::{Context, Result};
use calamine::{Reader, Xlsx, open_workbook};
use rust_xlsxwriter::{Color, Format, Url, Workbook, Worksheet};
use std::path::Path;

const MAX_DATA_ROWS: u32 = 1_048_575;
const MAX_HYPERLINKS: usize = 65_530;
const MAX_WORKSHEETS: usize = 65_530;

pub(super) struct SheetCount {
    name: String,
    rows: u32,
    hyperlinks: usize,
}

struct Sheet {
    worksheet: Worksheet,
    rows: u32,
    hyperlinks: usize,
}

pub(super) fn write(
    workbook: &mut Workbook,
    base: &str,
    headers: &[String],
    mut rows: impl Iterator<Item = Vec<String>>,
    autofilter: bool,
    expected: &mut Vec<SheetCount>,
) -> Result<()> {
    let (_, sheet) = rows.try_fold(
        (1_u32, new_sheet(base, 1, headers)?),
        |(part, sheet), cells| {
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
                (part, new_sheet(base, part, headers)?)
            } else {
                (part, sheet)
            };
            write_row(&mut sheet, headers, &cells, hyperlinks)?;
            Ok::<_, anyhow::Error>((part, sheet))
        },
    )?;
    finish(workbook, sheet, headers.len(), autofilter, expected)
}

fn new_sheet(base: &str, part: u32, headers: &[String]) -> Result<Sheet> {
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
    let mut worksheet = Worksheet::new();
    worksheet.set_name(name)?;
    let format = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0x17_36_5D))
        .set_font_color(Color::White)
        .set_text_wrap();
    headers.iter().enumerate().try_for_each(|(column, text)| {
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
    })
}

fn is_hyperlink(header: &str, text: &str) -> bool {
    (matches!(header, "source_profiles" | "profile_url" | "source_url")
        || header.ends_with("::source_url"))
        && (text.starts_with("https://") || text.starts_with("http://"))
        && !text.contains("; ")
}

fn write_row(
    sheet: &mut Sheet,
    headers: &[String],
    cells: &[String],
    hyperlinks: usize,
) -> Result<()> {
    let row = sheet
        .rows
        .checked_add(1)
        .context("worksheet row overflow")?;
    cells
        .iter()
        .zip(headers)
        .enumerate()
        .try_for_each(|(column, (text, header))| {
            let column = u16::try_from(column)?;
            if is_hyperlink(header, text) {
                sheet
                    .worksheet
                    .write_url(row, column, Url::new(text.as_str()))?;
            } else {
                sheet.worksheet.write_string(row, column, text)?;
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
    });
    workbook.push_worksheet(sheet.worksheet);
    Ok(())
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
        let hyperlinks = workbook.hyperlinks_by_sheet_name(&sheet.name)?;
        anyhow::ensure!(
            hyperlinks.len() == sheet.hyperlinks,
            "XLSX {} hyperlink count mismatch",
            sheet.name
        );
        Ok(())
    })
}
