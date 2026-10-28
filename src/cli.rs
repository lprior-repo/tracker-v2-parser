use std::{env, path::PathBuf};

use anyhow::{Context, Result, anyhow, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    config::Config,
    database::Database,
    model::{PageContext, PageKind, Source, Sport, Target},
    runner::Runner,
    school_directory::{self, Directory},
};

#[derive(Parser)]
#[command(name = "track-scrapper-v2", version)]
pub struct Cli {
    #[arg(long, global = true)]
    database_url: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Scrape(Box<ScrapeArgs>),
    Export(ExportArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceArg {
    Athleticnet,
    Milesplit,
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Directory,
    Team,
    Roster,
    Athlete,
    Meet,
    Results,
    Contacts,
}

#[derive(Clone, Copy, ValueEnum)]
enum SportArg {
    Outdoor,
    Indoor,
    Xc,
}

#[derive(Args)]
struct ScrapeArgs {
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
    #[arg(long, conflicts_with_all = ["input", "refresh", "school_directory", "school_directory_manifest", "school_addresses", "school_addresses_sha256", "download_school_addresses", "private_school_addresses"])]
    wiaa_directory: bool,
    #[arg(long, conflicts_with_all = ["input", "refresh", "wiaa_directory", "school_directory", "school_directory_manifest", "school_addresses", "school_addresses_sha256", "download_school_addresses", "private_school_addresses"], help = "Import cooperative coach associations from hash-verified public WIAA captures")]
    wiaa_coop_manifest: Option<PathBuf>,
    #[arg(long, requires_all = ["source", "kind", "url"])]
    input: Option<PathBuf>,
    #[arg(long, requires = "input")]
    source: Option<SourceArg>,
    #[arg(long, requires = "input")]
    kind: Option<KindArg>,
    #[arg(long, requires = "input")]
    url: Option<String>,
    #[arg(long, requires = "input")]
    state: Option<String>,
    #[arg(long, requires = "input")]
    school_id: Option<String>,
    #[arg(long, requires = "input")]
    school_name: Option<String>,
    #[arg(long, requires = "input")]
    athlete_id: Option<String>,
    #[arg(long, requires = "input")]
    sport: Option<SportArg>,
    #[arg(long, requires = "input")]
    season: Option<i32>,
    #[arg(long, requires = "input")]
    subject_id: Option<String>,
    #[arg(long, requires = "input")]
    meet_name: Option<String>,
    #[arg(long, requires = "input")]
    meet_date: Option<String>,
    #[arg(long = "result-set-id", requires = "input")]
    result_set_ids: Vec<String>,
    #[arg(long, conflicts_with = "input")]
    refresh: bool,
    #[arg(long, requires = "school_directory_manifest", conflicts_with_all = ["input", "refresh"], help = "Offline reference school_directory.csv to enrich existing schools")]
    school_directory: Option<PathBuf>,
    #[arg(
        long,
        requires = "school_directory",
        help = "Manifest for the supplied reference directory"
    )]
    school_directory_manifest: Option<PathBuf>,
    #[arg(long, requires = "school_addresses_sha256", conflicts_with_all = ["school_directory", "input", "refresh", "download_school_addresses"], help = "Import a local published NCES school-address CSV")]
    school_addresses: Option<PathBuf>,
    #[arg(long, requires = "school_addresses_sha256", conflicts_with_all = ["school_directory", "input", "refresh", "school_addresses"], help = "Download the published CSV to this new file, then import it")]
    download_school_addresses: Option<PathBuf>,
    #[arg(long, requires = "school_addresses_sha256", conflicts_with_all = ["school_directory", "input", "refresh", "school_addresses", "download_school_addresses"], help = "Import a local NCES 2023-24 Private School Survey CSV")]
    private_school_addresses: Option<PathBuf>,
    #[arg(long, help = "Expected SHA-256 of the published CSV")]
    school_addresses_sha256: Option<String>,
}

#[derive(Args)]
struct ExportArgs {
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
    #[arg(long, default_value = "output.xlsx")]
    output: PathBuf,
    #[arg(long)]
    per_athlete: bool,
}

impl Cli {
    async fn run(self) -> Result<()> {
        let runner = Runner::new();
        let database_url = resolve_database_url(self.database_url)?;
        match self.command {
            Command::Scrape(args) => {
                if let Some(path) = args.wiaa_coop_manifest {
                    return import_wiaa_coops(&args.config, &path, &database_url).await;
                }
                if args.wiaa_directory {
                    return enrich_wiaa(&args.config, &database_url).await;
                }
                if let Some(path) = args.private_school_addresses {
                    let expected = args.school_addresses_sha256.ok_or_else(|| {
                        anyhow!("private school import requires --school-addresses-sha256")
                    })?;
                    return import_private_schools(path, expected, &database_url).await;
                }
                if args.school_addresses.is_some() || args.download_school_addresses.is_some() {
                    let expected = args.school_addresses_sha256.ok_or_else(|| {
                        anyhow!("school address import requires --school-addresses-sha256")
                    })?;
                    let (path, origin) =
                        match (args.school_addresses, args.download_school_addresses) {
                            (Some(path), None) => (path, None),
                            (None, Some(path)) => {
                                crate::school_address_download::download(&expected, &path).await?;
                                (path, Some(crate::school_address_download::SOURCE))
                            }
                            _ => return Err(anyhow!("select one school address input")),
                        };
                    return import_published_schools(path, expected, origin, &database_url).await;
                }
                if args.school_addresses_sha256.is_some() {
                    return Err(anyhow!(
                        "--school-addresses-sha256 requires a school address input"
                    ));
                }
                if let Some(path) = args.school_directory {
                    let manifest = args
                        .school_directory_manifest
                        .ok_or_else(|| anyhow!("school directory requires a manifest"))?;
                    return enrich_schools(path, manifest, &database_url).await;
                }
                let config = Config::load(&args.config)?;
                if let Some(input) = args.input {
                    let source = match args.source {
                        Some(SourceArg::Athleticnet) => Source::AthleticNet,
                        Some(SourceArg::Milesplit) => Source::MileSplit,
                        None => return Err(anyhow!("replay requires --source")),
                    };
                    let kind = match args.kind {
                        Some(KindArg::Directory) => PageKind::Directory,
                        Some(KindArg::Team) => PageKind::Team,
                        Some(KindArg::Roster) => PageKind::Roster,
                        Some(KindArg::Athlete) => PageKind::Athlete,
                        Some(KindArg::Meet) => PageKind::Meet,
                        Some(KindArg::Results) => PageKind::Results,
                        Some(KindArg::Contacts) => PageKind::Contacts,
                        None => return Err(anyhow!("replay requires --kind")),
                    };
                    let sport = args.sport.map(|sport| match sport {
                        SportArg::Outdoor => Sport::Outdoor,
                        SportArg::Indoor => Sport::Indoor,
                        SportArg::Xc => Sport::CrossCountry,
                    });
                    let context = PageContext {
                        state: args.state,
                        acquisition_state: None,
                        school_id: args.school_id,
                        school_name: args.school_name,
                        athlete_id: args.athlete_id,
                        sport,
                        season: args.season,
                        subject_id: args.subject_id,
                        meet_name: args.meet_name,
                        meet_date: args.meet_date,
                        result_set_ids: args.result_set_ids,
                    };
                    let url = args.url.ok_or_else(|| anyhow!("replay requires --url"))?;
                    runner
                        .replay(
                            &config,
                            &database_url,
                            &input,
                            Target::get(source, kind, url, context),
                        )
                        .await
                } else {
                    runner
                        .scrape_with_refresh(&config, &database_url, args.refresh)
                        .await
                }
            }
            Command::Export(args) => {
                let mut config = Config::load(&args.config)?;
                config.export.per_athlete_sheets |= args.per_athlete;
                runner.export(&config, &args.output, &database_url).await
            }
        }
    }
}

fn resolve_database_url(argument: Option<String>) -> Result<String> {
    match argument {
        Some(url) if !url.is_empty() => Ok(url),
        Some(_) => Err(anyhow!("database URL is empty")),
        None => database_url(),
    }
}

async fn import_wiaa_coops(
    config_path: &std::path::Path,
    manifest_path: &std::path::Path,
    database_url: &str,
) -> Result<()> {
    let config = Config::load(config_path)?;
    ensure!(
        config.states.iter().any(|state| state == "WI") && config.acquisition.includes_contacts(),
        "WIAA cooperative import requires Wisconsin scope and contact collection"
    );
    let report = crate::wiaa_coop_import::import(database_url, manifest_path).await?;
    println!(
        "WIAA cooperatives: {} associations; {} programs; {} coaches; {:?} unconfirmed programs",
        report.associations_imported,
        report.programs_imported,
        report.coaches_imported,
        report.unconfirmed_programs
    );
    Ok(())
}

async fn enrich_wiaa(path: &std::path::Path, database_url: &str) -> Result<()> {
    let config = Config::load(path)?;
    let report = crate::wiaa_enrichment::enrich(&config, database_url).await?;
    println!(
        "WIAA: {} directory schools; {} matched; {} contact pages enqueued; {} missing, {} uncorroborated, {} ambiguous",
        report.directory_schools,
        report.matched,
        report.enqueued,
        report.missing,
        report.uncorroborated,
        report.ambiguous
    );
    Ok(())
}

async fn import_private_schools(path: PathBuf, expected: String, database_url: &str) -> Result<()> {
    let directory = tokio::task::spawn_blocking(move || {
        school_directory::pss::PrivateDirectory::load(&path, &expected)
    })
    .await??;
    let database = Database::connect(database_url).await?;
    let result = school_directory::pss::import(database.pool(), &directory).await;
    database.close().await;
    let report = result?;
    println!(
        "private schools: {} inserted; {} updated; {} enriched, {} conflicts, {} uncorroborated",
        report.inserted,
        report.updated,
        report.enrichment.updated,
        report
            .conflicts
            .checked_add(report.enrichment.conflicts)
            .context("school conflict count overflow")?,
        report.enrichment.uncorroborated
    );
    Ok(())
}

async fn import_published_schools(
    path: PathBuf,
    expected: String,
    origin: Option<&'static str>,
    database_url: &str,
) -> Result<()> {
    let directory = tokio::task::spawn_blocking(move || {
        school_directory::published::PublishedDirectory::load_with_origin(&path, &expected, origin)
    })
    .await??;
    let database = Database::connect(database_url).await?;
    let result = school_directory::published::import(database.pool(), &directory).await;
    database.close().await;
    let report = result?;
    println!(
        "published schools: {} inserted; {} updated; {} enriched, {} conflicts, {} uncorroborated",
        report.inserted,
        report.updated,
        report.enrichment.updated,
        report
            .conflicts
            .checked_add(report.enrichment.conflicts)
            .context("school conflict count overflow")?,
        report.enrichment.uncorroborated
    );
    Ok(())
}

async fn enrich_schools(path: PathBuf, manifest: PathBuf, database_url: &str) -> Result<()> {
    let directory =
        tokio::task::spawn_blocking(move || Directory::load(&path, &manifest)).await??;
    let database = Database::connect(database_url).await?;
    let result = school_directory::enrich(database.pool(), &directory).await;
    database.close().await;
    let report = result?;
    println!(
        "school directory: {} updated, {} conflicts, {} uncorroborated",
        report.updated, report.conflicts, report.uncorroborated
    );
    Ok(())
}

fn database_url() -> Result<String> {
    match env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => Ok(url),
        Ok(_) => Err(anyhow!("DATABASE_URL is set but empty")),
        Err(env::VarError::NotPresent) => {
            Err(anyhow!("DATABASE_URL environment variable is not set"))
        }
        Err(env::VarError::NotUnicode(_)) => Err(anyhow!("DATABASE_URL contains non-UTF-8 bytes")),
    }
}

pub async fn run() -> Result<()> {
    Cli::parse().run().await
}
