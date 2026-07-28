//! CLI over the `transit_ingest` library.

use std::path::PathBuf;

use chrono::{Duration, NaiveDate};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use transit_ingest::{
    config::{Agency, Config},
    dataset,
    error::ConfigError,
    fetch,
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

    /// Download static GTFS archives into `<data-dir>/raw/`.
    Fetch {
        /// Agency id from the registry. Omit to fetch every configured agency.
        agency: Option<String>,
        /// Re-download even when an archive is already present.
        #[arg(long)]
        force: bool,
    },

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
        /// Write N calendar days from the start of the window. Defaults the
        /// window to the dates every selected agency covers, so the result is
        /// comparable across them.
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

        Command::Fetch { agency, force } => {
            let timeout = std::time::Duration::from_secs(config.defaults.request_timeout_seconds);

            for agency in select(&config, agency.as_deref())? {
                let fetched = fetch::static_archive(agency, &cli.data_dir, timeout, force)?;
                println!(
                    "{:<10} {:>7.1} MB  {}{}",
                    fetched.agency_id,
                    fetched.bytes as f64 / (1024.0 * 1024.0),
                    fetched.path.display(),
                    if fetched.reused {
                        "  (already present; --force to re-download)"
                    } else {
                        ""
                    }
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

            // The feed's real coverage, which is what a build window has to sit
            // inside — and not what feed_info.txt claims, that field being
            // optional and frequently stale.
            let calendar = ServiceCalendar::build(&feed)?;
            match calendar.coverage() {
                Some((first, last)) => {
                    println!("  services        {:>9}", calendar.service_count());
                    println!("  service dates   {:>9}", calendar.active_dates().len());
                    println!("  coverage         {first} .. {last}");
                }
                None => println!("  coverage             none — no service on any date"),
            }

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
            if agency.is_none() && archive.is_some() {
                anyhow::bail!("--archive names one file, so it needs one agency");
            }

            let agencies = select(&config, agency.as_deref())?;
            let archive_of = |agency: &Agency| {
                archive
                    .clone()
                    .unwrap_or_else(|| agency.archive_path(&cli.data_dir))
            };

            let window = resolve_window(&agencies, &archive_of, from, to, days)?;
            if let Some((start, end)) = window {
                println!("building {start} .. {end}\n");
            }

            for agency in agencies {
                let path = archive_of(agency);
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

                let summary = dataset::build(&feed, agency, &cli.data_dir, window)?;
                print_summary(&summary);
            }
        }
    }

    Ok(())
}

/// One named agency, or all of them. Commands that operate per-agency all
/// accept the id as optional and default to the whole registry.
fn select<'a>(config: &'a Config, agency: Option<&str>) -> anyhow::Result<Vec<&'a Agency>> {
    match agency {
        Some(id) => Ok(vec![config.agency(id).ok_or(
            ConfigError::UnknownAgency {
                agency: id.to_string(),
            },
        )?]),
        None => Ok(config.agencies.iter().collect()),
    }
}

/// Resolve one date window that every agency being built actually covers.
///
/// The three MTA feeds are published on their own schedules and their coverage
/// barely overlaps — taking each feed's own first week gave the Subway late
/// May, Metro-North mid-July and the LIRR the end of July, with not one day in
/// common. Every cross-agency question the cube exists to answer is
/// unanswerable on a dataset like that, and nothing about it looks wrong until
/// you slice by agency and one of them is empty.
///
/// So the default window is the intersection of coverage, not the union.
/// `--from` and `--to` override it when a specific window is wanted.
fn resolve_window(
    agencies: &[&Agency],
    archive_of: &dyn Fn(&Agency) -> PathBuf,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    days: Option<u32>,
) -> anyhow::Result<Option<(NaiveDate, NaiveDate)>> {
    if from.is_none() && to.is_none() && days.is_none() {
        return Ok(None);
    }
    if days == Some(0) {
        anyhow::bail!("--days must be at least 1");
    }

    // Fully specified: no need to open anything.
    if let (Some(start), Some(end)) = (from, to) {
        if end < start {
            anyhow::bail!("--to {end} is before --from {start}");
        }
        return Ok(Some((start, end)));
    }

    // Only the calendar files are read, so this stays cheap even though it
    // touches every archive before the first one is built.
    let mut shared: Option<(NaiveDate, NaiveDate)> = None;
    for agency in agencies {
        let path = archive_of(agency);
        let calendar = ServiceCalendar::read(&path)?;
        let Some((first, last)) = calendar.coverage() else {
            anyhow::bail!("{} has no service dates at all", agency.id);
        };

        println!("{:<10} covers {first} .. {last}", agency.id);
        shared = Some(match shared {
            Some((start, end)) => (start.max(first), end.min(last)),
            None => (first, last),
        });
    }

    let (covered_start, covered_end) = shared.expect("select() never returns an empty agency list");

    if covered_start > covered_end {
        anyhow::bail!(
            "the selected agencies share no service dates; \
             build them one at a time, or pass --from and --to explicitly"
        );
    }

    let start = from.unwrap_or(covered_start);
    let end = match (to, days) {
        (Some(to), _) => to,
        // Calendar days rather than service dates: the window has to mean the
        // same span for every agency, and they do not have service on the same
        // set of days.
        (None, Some(days)) => (start + Duration::days(days as i64 - 1)).min(covered_end),
        (None, None) => covered_end,
    };

    if end < start {
        anyhow::bail!("the requested window {start} .. {end} is empty");
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
