use std::{fs, path::Path};

use anyhow::Result;
use calamine::Reader;

use super::*;
use crate::{
    config::Config,
    durable::{
        delivery::publish_configured,
        operations::{
            OperationSpec,
            export::{Batch, FrozenChunk, FrozenPlan, render_export, verify_export},
        },
    },
    export::ExportLayout,
    model::{AthleteRecord, School},
};

fn verified(
    root: &Path,
    operation: &str,
    state: &str,
    year: i32,
    name: Option<&str>,
) -> Result<(Workspace, VerifiedExport)> {
    let mut config = Config::default();
    config.states = vec![state.into()];
    config.graduation_years = vec![year];
    config.export.layout = ExportLayout::SingleSheet;
    config.export.name = name.map(str::to_owned);
    config.validate()?;
    let workspace = Workspace::open(
        root,
        operation,
        &OperationSpec::Export {
            config: config.clone(),
        },
    )?;
    let record = AthleteRecord {
        id: 1,
        name: format!("{state} {year} {operation}"),
        gender: None,
        grade: None,
        graduation_year: Some(year),
        needs_review: false,
        schools: vec![School {
            id: "101".into(),
            name: format!("{state} School"),
            state: Some(state.into()),
            ..Default::default()
        }],
        coaches: Vec::new(),
        profiles: Vec::new(),
        performances: Vec::new(),
        profile_details: Vec::new(),
    };
    let artifact = workspace.record(
        "frozen-batch.json",
        &Batch {
            records: vec![record],
            review: Vec::new(),
            issues: Vec::new(),
        },
    )?;
    let plan = FrozenPlan {
        binding: workspace.binding().into(),
        generation: 1,
        config,
        rows: 1,
        bytes: artifact.bytes,
        chunks: vec![FrozenChunk {
            artifact,
            athlete_ids: vec![1],
        }],
        review_rows: None,
        review_bytes: None,
    };
    let frozen = FrozenExport {
        manifest: workspace.record("frozen-export.json", &plan)?,
    };
    let workbook = render_export(&workspace, &frozen)?;
    let verified = verify_export(&workspace, &frozen, &workbook)?;
    Ok((workspace, verified))
}

fn workbook_name(path: &Path) -> Result<String> {
    let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path)?;
    assert_eq!(workbook.sheet_names(), ["Athletes"]);
    Ok(workbook
        .worksheet_range("Athletes")?
        .get_value((1, 0))
        .expect("one athlete row")
        .to_string())
}

#[test]
fn interleaved_named_exports_keep_each_state_and_year_pointer_and_delivery_isolated() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let base = directory.path().canonicalize()?;
    let root = base.join("artifacts");
    let legacy = base.join("wisconsin-2027.xlsx");
    let (wi, verified_wi) = verified(&root, "wisconsin", "WI", 2027, None)?;
    let published_wi = publish_export(&wi, &verified_wi)?;
    assert!(publish_configured(&legacy, &wi, &published_wi)?);
    let legacy_bytes = fs::read(&legacy)?;
    let legacy_pointer = fs::read(root.join("current-export.json"))?;
    let (mi, verified_mi) = verified(&root, "michigan-first", "MI", 2028, Some("michigan-2028"))?;
    let published_mi = publish_export(&mi, &verified_mi)?;
    assert!(publish_configured(&legacy, &mi, &published_mi)?);
    let mi_path = base.join("michigan-2028.xlsx");
    let mi_bytes = fs::read(&mi_path)?;
    let mi_pointer = fs::read(&published_mi.canonical_pointer)?;
    let (mn, verified_mn) = verified(&root, "minnesota-first", "MN", 2029, Some("minnesota-2029"))?;
    let published_mn = publish_export(&mn, &verified_mn)?;
    assert!(publish_configured(&legacy, &mn, &published_mn)?);
    assert_eq!(
        published_mi.canonical_pointer,
        root.join("current-named-michigan-2028.json")
    );
    assert_ne!(
        published_mi.canonical_pointer,
        published_mn.canonical_pointer
    );
    assert_eq!(fs::read(&mi_path)?, mi_bytes);
    assert_eq!(fs::read(&published_mi.canonical_pointer)?, mi_pointer);
    assert_eq!(workbook_name(&mi_path)?, "MI 2028 michigan-first");
    assert_eq!(
        workbook_name(&base.join("minnesota-2029.xlsx"))?,
        "MN 2029 minnesota-first"
    );
    assert_eq!(
        publish_export(&mi, &verified_mi)?.workbook,
        published_mi.workbook
    );
    assert!(publish_configured(&legacy, &mi, &published_mi)?);
    assert!(publish_configured(&legacy, &mn, &published_mn)?);
    let (updated, verified_updated) =
        verified(&root, "michigan-updated", "MI", 2028, Some("michigan-2028"))?;
    let published_updated = publish_export(&updated, &verified_updated)?;
    assert!(publish_configured(&legacy, &updated, &published_updated)?);
    assert!(!publish_configured(&legacy, &mi, &published_mi)?);
    assert_eq!(workbook_name(&mi_path)?, "MI 2028 michigan-updated");
    assert_eq!(fs::read(&legacy)?, legacy_bytes);
    assert_eq!(fs::read(root.join("current-export.json"))?, legacy_pointer);
    assert_eq!(fs::read(&published_mi.workbook.path)?, mi_bytes);
    Ok(())
}

#[test]
fn named_scope_cannot_be_reused_for_another_state_or_cohort_or_legacy_delivery() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let base = directory.path().canonicalize()?;
    let root = base.join("artifacts");
    let legacy = base.join("legacy.xlsx");
    fs::write(&legacy, b"previous Wisconsin workbook")?;
    let (original, proof) = verified(&root, "original", "MI", 2028, Some("selected"))?;
    let published = publish_export(&original, &proof)?;
    assert!(publish_configured(&legacy, &original, &published)?);
    let pointer = fs::read(&published.canonical_pointer)?;
    let bytes = fs::read(base.join("selected.xlsx"))?;
    for (operation, state, year) in [("other-state", "IL", 2028), ("other-year", "MI", 2029)] {
        let (workspace, proof) = verified(&root, operation, state, year, Some("selected"))?;
        let failure = publish_export(&workspace, &proof).expect_err("scope alias must reject");
        assert!(
            failure
                .to_string()
                .contains("different state or graduation-year scope")
        );
        assert_eq!(fs::read(&published.canonical_pointer)?, pointer);
        assert_eq!(fs::read(base.join("selected.xlsx"))?, bytes);
    }
    let (workspace, proof) = verified(&root, "colliding-delivery", "IL", 2028, Some("LEGACY"))?;
    let published = publish_export(&workspace, &proof)?;
    assert!(
        publish_configured(&legacy, &workspace, &published)
            .expect_err("legacy filename must stay protected")
            .to_string()
            .contains("legacy workbook")
    );
    assert_eq!(fs::read(&legacy)?, b"previous Wisconsin workbook");
    Ok(())
}

#[test]
fn export_names_are_bounded_ascii_basenames_and_absent_name_preserves_serialization() -> Result<()>
{
    let default = Config::default();
    assert!(
        serde_json::to_value(&default)?["export"]
            .get("name")
            .is_none()
    );
    for name in [
        "",
        "../other",
        "sub/file",
        ".",
        "two words",
        "日本",
        "a.xlsx",
        &"a".repeat(81),
    ] {
        let mut config = default.clone();
        config.export.name = Some(name.into());
        assert!(
            config
                .validate()
                .expect_err("invalid name must reject")
                .to_string()
                .contains("export name")
        );
    }
    for name in ["export", "MI-2028_2029", &"A".repeat(80)] {
        let mut config = default.clone();
        config.export.name = Some(name.into());
        config.validate()?;
        assert_eq!(config.export.validated_name()?, Some(name));
    }
    Ok(())
}
