use std::{env, path::PathBuf};

use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    config::Config,
    model::{PageContext, PageKind, Source, Sport, Target},
    runner::Runner,
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
    #[arg(long, conflicts_with = "input")]
    refresh: bool,
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
        let database_url = match self.database_url {
            Some(url) if !url.is_empty() => url,
            Some(_) => return Err(anyhow!("database URL is empty")),
            None => database_url()?,
        };
        match self.command {
            Command::Scrape(args) => {
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
                        school_id: args.school_id,
                        school_name: args.school_name,
                        athlete_id: args.athlete_id,
                        sport,
                        season: args.season,
                        subject_id: args.subject_id,
                        meet_name: args.meet_name,
                        meet_date: args.meet_date,
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
