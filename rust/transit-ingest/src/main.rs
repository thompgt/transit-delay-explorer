//! CLI over the `transit_ingest` library.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use transit_ingest::{
    config::Config,
    error::ConfigError,
    gtfs::{validate, GtfsTime, StaticFeed},
};

#[derive(Parser)]
#[command(
    name = "transit-ingest",
    version,
    about = "GTFS ingest for Transit Delay Explorer"
)]
struct Cli {
    /// Agency registry. Defaults to the checked-in config at the repo root.
    #[arg(
        long,
        env = "TDE_CONFIG",
        default_value = "config/agencies.toml",
        global = true
    )]
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

    /// Parse a static archive and report its contents and integrity.
    Inspect {
        /// Agency id from the registry.
        agency: String,
        /// Archive to read. Defaults to `<data-dir>/raw/<agency>.zip`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("TDE_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
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

        Command::Inspect { agency, archive } => {
            let agency = config
                .agency(&agency)
                .ok_or(ConfigError::UnknownAgency { agency })?;

            let path = archive.unwrap_or_else(|| agency.archive_path(&cli.data_dir));
            let feed = StaticFeed::read(&path, agency)?;

            println!("{} — {}", agency.id, agency.name);
            println!("  routes          {:>9}", feed.routes.len());
            println!("  stops           {:>9}", feed.stops.len());
            println!("  trips           {:>9}", feed.trips.len());
            println!("  stop_times      {:>9}", feed.stop_times.len());
            println!("  calendar        {:>9}", feed.calendar.len());
            println!("  calendar_dates  {:>9}", feed.calendar_dates.len());

            let rollovers = feed
                .stop_times
                .iter()
                .filter(|st| {
                    GtfsTime::parse(&st.arrival_time, "stop_times.txt")
                        .ok()
                        .flatten()
                        .is_some_and(GtfsTime::rolls_over)
                })
                .count();
            println!(
                "  past 24:00:00   {:>9}  ({:.2}%)",
                rollovers,
                100.0 * rollovers as f64 / feed.stop_times.len().max(1) as f64
            );

            let violations = validate::check(&feed);
            if violations.is_empty() {
                println!("\n  referential integrity: clean");
            } else {
                // Exit non-zero so this is usable as a gate, but print
                // everything first -- a validator that stops at the first
                // problem makes fixing a feed an N-round trip.
                println!(
                    "\n  referential integrity: {} violation(s)",
                    violations.len()
                );
                for violation in &violations {
                    println!("    {violation}");
                }
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
