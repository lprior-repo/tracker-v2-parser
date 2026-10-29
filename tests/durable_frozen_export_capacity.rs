use anyhow::Result;
use serde_json::json;
use track_scrapper_v2::{
    config::Config,
    durable::operations::{FrozenExport, OperationSpec, Workspace, frozen_plan},
    export::MAX_EXPORT_BYTES,
};

fn metadata_plan(
    bytes: u64,
    oversized_chunk: bool,
) -> Result<(tempfile::TempDir, Workspace, FrozenExport)> {
    let directory = tempfile::tempdir()?;
    let config =
        Config::parse("states=['WI']\ngraduation_years=[2027]\n[export]\nlayout='single_sheet'\n")?;
    let workspace = Workspace::open(
        directory.path(),
        "capacity",
        &OperationSpec::Export {
            config: config.clone(),
        },
    )?;
    let limit = if oversized_chunk {
        bytes
    } else {
        64 * 1024 * 1024
    };
    let chunks = (0..bytes.div_ceil(limit)).map(|index| -> Result<_> {
        let next = (bytes - index * limit).min(limit);
        Ok(json!({"artifact": {"path": workspace.path(&format!("absent-chunk-{index}.json"))?, "sha256": "0".repeat(64), "bytes": next}, "athlete_ids": [index + 1]}))
    }).collect::<Result<Vec<_>>>()?;
    let document = json!({"binding": workspace.binding(), "generation": 1, "config": config, "rows": chunks.len(), "chunks": chunks, "bytes": bytes});
    let frozen = FrozenExport {
        manifest: workspace.store("frozen-export.json", &serde_json::to_vec(&document)?)?,
    };
    Ok((directory, workspace, frozen))
}

#[test]
fn frozen_metadata_accepts_more_than_512_mib_and_exactly_eight_gib_without_large_files()
-> Result<()> {
    for bytes in [513 * 1024 * 1024, MAX_EXPORT_BYTES] {
        let (_directory, workspace, frozen) = metadata_plan(bytes, false)?;
        let plan = frozen_plan(&workspace, &frozen)?;
        assert_eq!(plan.bytes, bytes);
        assert_eq!(plan.chunks.len() as u64, bytes.div_ceil(64 * 1024 * 1024));
        assert!(
            plan.chunks
                .iter()
                .all(|chunk| chunk.artifact.bytes <= 64 * 1024 * 1024
                    && !chunk.artifact.path.exists())
        );
        assert!(frozen.manifest.bytes < 128 * 1024);
    }
    Ok(())
}

#[test]
fn frozen_metadata_rejects_eight_gib_plus_one_and_preserves_the_per_chunk_limit() -> Result<()> {
    let (_directory, workspace, frozen) = metadata_plan(MAX_EXPORT_BYTES + 1, false)?;
    assert!(
        frozen_plan(&workspace, &frozen)
            .err()
            .is_some_and(|error| error.to_string().contains("capacity"))
    );
    let (_directory, workspace, frozen) = metadata_plan(64 * 1024 * 1024 + 1, true)?;
    assert!(
        frozen_plan(&workspace, &frozen)
            .err()
            .is_some_and(|error| error.to_string().contains("64 MiB"))
    );
    Ok(())
}
