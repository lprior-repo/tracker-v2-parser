use super::*;
use rust_xlsxwriter::Workbook;

#[test]
fn native_cell_oracle_reopens_exact_athletes_review_and_rejects_missing_pr_or_review_cell()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("proof.xlsx");
    let mut expected = vec![vec![String::new(); 18]; 2];
    expected
        .get_mut(0)
        .context("row")?
        .get_mut(9)
        .context("column")?
        .push_str("failed");
    expected
        .get_mut(1)
        .context("row")?
        .get_mut(9)
        .context("column")?
        .push_str("blocked");
    let mut book = Workbook::new();
    let athletes = book.add_worksheet().set_name("Athletes")?;
    for (column, (title, value)) in [
        ("Name", "Brice Fuller"),
        ("Graduation year", "2028"),
        ("Outdoor 1600m PR [all levels; milesplit]", "4:12.69"),
        ("Outdoor 1600m PR [ms; milesplit]", "4:33.74"),
        ("Indoor 3000m PR [hs; milesplit]", "8:26.83"),
        ("XC 5K PR [hs; milesplit]", "14:49.06"),
    ]
    .into_iter()
    .enumerate()
    {
        athletes.write_string(0, u16::try_from(column)?, title)?;
        athletes.write_string(1, u16::try_from(column)?, value)?;
    }
    let review = book.add_worksheet().set_name("Review")?;
    for (column, title) in review::HEADERS.iter().enumerate() {
        review.write_string(0, u16::try_from(column)?, *title)?;
    }
    for (row, values) in expected.iter().enumerate() {
        for (column, value) in values.iter().enumerate() {
            review.write_string(
                u32::try_from(row.checked_add(1).context("row overflow")?)?,
                u16::try_from(column)?,
                value,
            )?;
        }
    }
    book.save(&path)?;
    assert!(workbook(&path, &expected).is_ok());
    let mut altered = expected.clone();
    altered
        .get_mut(0)
        .context("row")?
        .get_mut(4)
        .context("column")?
        .push_str("different school");
    assert!(workbook(&path, &altered).is_err());
    book.worksheet_from_name("Athletes")?
        .write_string(1, 2, "4:99.99")?;
    book.save(&path)?;
    assert!(workbook(&path, &expected).is_err());
    Ok(())
}
