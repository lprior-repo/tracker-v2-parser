use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use calamine::{Data, Range, Reader, open_workbook_auto};
use clap::Parser;
use track_scrapper_v2::{
    config::Config,
    durable::operations::{
        OperationSpec, Workspace, freeze_export, frozen_plan, prepare_export_chunk, publish_export,
        render_export, verify_export,
    },
    runner::Runner,
};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    artifacts: PathBuf,
    #[arg(long)]
    operation_id: String,
}

fn sheet(path: &std::path::Path) -> Result<Range<Data>> {
    let mut workbook = open_workbook_auto(path)?;
    ensure!(
        workbook.sheet_names() == ["Athletes"],
        "qualification requires exactly one Athletes sheet"
    );
    workbook.worksheet_range("Athletes").map_err(Into::into)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(
        std::fs::read_dir(&arguments.artifacts)?.next().is_none(),
        "qualification requires an existing empty artifact directory"
    );
    let config = Config::load(&arguments.config)?;
    let database = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let baseline = arguments.artifacts.join("unprojected-baseline.xlsx");
    Runner.export(&config, &baseline, &database).await?;
    let workspace = Workspace::open(
        &arguments.artifacts,
        &arguments.operation_id,
        &OperationSpec::Export {
            config: config.clone(),
        },
    )?;
    let frozen = freeze_export(&workspace, &config, &database).await?;
    let plan = frozen_plan(&workspace, &frozen)?;
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
        plan.chunks.iter().enumerate().try_for_each(|(index, _)| {
            prepare_export_chunk(&workspace, &frozen, index).map(|_| ())
        })?;
        let workbook = render_export(&workspace, &frozen)?;
        let verified = verify_export(&workspace, &frozen, &workbook)?;
        ensure!(
            sheet(&baseline)? == sheet(&workbook.path)?,
            "projected durable export differs from unprojected native export"
        );
        let publication = publish_export(&workspace, &verified)?;
        Ok(serde_json::json!({
            "rows": verified.rows,
            "columns": verified.columns,
            "chunks": plan.chunks.len(),
            "frozen_bytes": plan.bytes,
            "unprojected_cell_parity": "every cell matched",
            "publication": publication
        }))
    })
    .await
    .context("joining owned export qualification")??;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
