use super::*;

#[test]
fn previous_v3_archives_retain_the_exact_fourteen_table_scope_without_claiming_selector_records()
-> Result<()> {
    let source = temporary()?;
    bundle(source.path())?;
    let mut inventory: Inventory = files::load(
        &source.path().join("inventory.json"),
        super::super::manifest::SMALL,
    )?;
    inventory
        .tables
        .truncate(super::super::manifest::AUDITED_TABLES.len());
    std::fs::remove_file(source.path().join("inventory.json"))?;
    files::json(&source.path().join("inventory.json"), &inventory)?;
    let mut manifest: Manifest =
        files::load(&source.path().join("manifest.json"), 64 * 1024 * 1024)?;
    manifest.format = super::super::manifest::AUDITED.into();
    std::fs::remove_file(source.path().join("manifest.json"))?;
    manifest.files = files::list(source.path())?
        .iter()
        .map(|path| files::fingerprint(source.path(), path))
        .collect::<Result<Vec<_>>>()?;
    files::json(&source.path().join("manifest.json"), &manifest)?;
    let work = temporary()?;
    let archive = work.path().join("previous-v3.tar.gz");
    archive::write(source.path(), &archive)?;
    let target = temporary()?;
    let (manifest, actual) = archive::unpack(&archive, target.path())?;
    assert_eq!(manifest.format, super::super::manifest::AUDITED);
    assert_eq!(actual.tables.len(), 14);
    assert_eq!(
        actual.tables.last().map(|table| table.name.as_str()),
        Some("reviewed_result_links")
    );
    assert!(
        !actual
            .tables
            .iter()
            .any(|table| table.name == "selector_changes")
    );
    Ok(())
}
