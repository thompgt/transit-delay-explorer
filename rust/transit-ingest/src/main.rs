//! CLI over the `transit_ingest` library.

use std::path::PathBuf;

use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use transit_ingest::{
    config::{Agency, Config},
    dataset,
    error::ConfigError,
    gtfs::{validate, GtfsTime, ServiceCalendar, StaticFeed},
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

    /// Resolve a static archive into a partitioned Parquet dataset.
    Build {
        /// Agency id from the registry. Omit to build every configured agency.
        agency: Option<String>,
        /// Archive to read. Defaults to `<data-dir>/raw/<agency>.zip`.
        #[arg(long)]
        archive: Option<PathBuf>,
        /// First service date to write, `YYYY-MM-DD`. Defaults to the start of
        /// the feed's own coverage.
        #[arg(long)]
        from: Option<NaiveDate>,
        /// Last service date to write, inclusive.
        #[arg(long)]
        to: Option<NaiveDate>,
        /// Write only the first N days from `--from`. A convenience for the
        /// common case of wanting one service week rather than a whole feed.
        #[arg(long, conflicts_with = "to")]
        days: Option<u32>,
        /// Write the dataset even if the feed has integrity violations.
        #[arg(long)]
        allow_violations: bool,
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

        Command::Build {
            agency,
            archive,
            from,
            to,
            days,
            allow_violations,
        } => {
            let agencies: Vec<&Agency> = match &agency {
                Some(id) => vec![config
                    .agency(id)
                    .ok_or(ConfigError::UnknownAgency { agency: id.clone() })?],
                None => {
                    if archive.is_some() {
                        anyhow::bail!("--archive names one file, so it needs one agency");
                    }
                    config.agencies.iter().collect()
                }
            };

            for agency in agencies {
                let path = archive
                    .clone()
                    .unwrap_or_else(|| agency.archive_path(&cli.data_dir));
                let feed = StaticFeed::read(&path, agency)?;

                // A dataset built from a feed with dangling keys produces a
                // cube with silently missing slices, which is worse than no
                // cube. Overridable, because sometimes you want to look at it
                // anyway.
                let violations = validate::check(&feed);
                if !violations.is_empty() {
                    println!("{}: {} violation(s)", agency.id, violations.len());
                    for violation in &violations {
                        println!("  {violation}");
                    }
                    if !allow_violations {
                        anyhow::bail!(
                            "{} failed validation; pass --allow-violations to build anyway",
                            agency.id
                        );
                    }
                }

                let range = date_range(&feed, from, to, days)?;
                let summary = dataset::build(&feed, agency, &cli.data_dir, range)?;
                print_summary(&summary);
            }
        }
    }

    Ok(())
}

/// Resolve the requested window against what the feed actually covers.
///
/// `--days` counts service dates from the start of the window rather than
/// calendar days, so "one week" means seven days that have service on them.
/// On a feed with weekday-only service those are not the same span, and the
/// useful reading is the one that yields seven partitions.
fn date_range(
    feed: &StaticFeed,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    days: Option<u32>,
) -> anyhow::Result<Option<(NaiveDate, NaiveDate)>> {
    if from.is_none() && to.is_none() && days.is_none() {
        return Ok(None);
    }

    let calendar = ServiceCalendar::build(feed)?;
    let Some((first, last)) = calendar.coverage() else {
        anyhow::bail!("feed has no service dates at all");
    };

    let start = from.unwrap_or(first);

    let end = match (to, days) {
        (Some(to), _) => to,
        (None, Some(days)) if days > 0 => calendar
            .active_dates()
            .into_iter()
            .filter(|d| *d >= start)
            .nth(days as usize - 1)
            // Fewer service dates remain than were asked for; take what there
            // is rather than failing on a window that is merely optimistic.
            .unwrap_or(last),
        (None, Some(_)) => anyhow::bail!("--days must be at least 1"),
        (None, None) => last,
    };

    if end < start {
        anyhow::bail!("--to {end} is before --from {start}");
    }

    Ok(Some((start, end)))
}

fn print_summary(summary: &dataset::Summary) {
    let span = match (summary.first_date, summary.last_date) {
        (Some(first), Some(last)) => format!("{first} .. {last}"),
        _ => "no dates in range".to_string(),
    };

    println!("{} — {}", summary.agency_id, span);
    println!("  partitions      {:>9}", summary.dates_written);
    println!("  events          {:>9}", summary.events);
    println!("  trips resolved  {:>9}", summary.trips);
    println!("  routes          {:>9}", summary.routes);
    println!("  stops           {:>9}", summary.stops);
    println!("  past 24:00:00   {:>9}", summary.crossing_midnight);

    // Only worth printing when non-zero: these are the two counts that explain
    // a row total coming in lower than expected.
    if summary.untimed_stop_times > 0 {
        println!("  untimed, dropped{:>9}", summary.untimed_stop_times);
    }
    if summary.dates_empty > 0 {
        println!("  empty dates     {:>9}", summary.dates_empty);
    }
}
