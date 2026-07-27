//! CLI over the `transit_ingest` library.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use transit_ingest::{config::Config, error::ConfigError};

#[derive(Parser)]
#[command(name = "transit-ingest", version, about = "GTFS ingest for Transit Delay Explorer")]
struct Cli {
    /// Agency registry. Defaults to the checked-in config at the repo root.
    #[arg(long, env = "TDE_CONFIG", default_value = "config/agencies.toml", global = true)]
    config: PathBuf,

    /// Working directory for downloaded feeds and Parquet output.
    #[arg(long, env = "TDE_DATA_DIR", default_value = "data", global = true)]
    data_dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List configured agencies and their feeds.
    Agencies,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("TDE_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;

    match cli.command {
        Command::Agencies => {
            for agency in &config.agencies {
                let feeds = agency
                    .realtime
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "{:<10} {:<28} {:<14} tz={} realtime=[{}]",
                    agency.id, agency.name, agency.mode, agency.timezone, feeds
                );
            }
        }
    }

    Ok(())
}

/// Kept so the unused-import lint does not fire before later phases use it.
#[allow(dead_code)]
fn unknown_agency(id: &str) -> ConfigError {
    ConfigError::UnknownAgency { agency: id.to_string() }
}
