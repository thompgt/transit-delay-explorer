//! Writing a whole agency's Parquet dataset, partitioned by service date.
//!
//! The layout is Hive-style:
//!
//! ```text
//! <data-dir>/parquet/
//!   scheduled_events/service_date=2026-04-01/MTA_NYCT.parquet
//!   scheduled_events/service_date=2026-04-02/MTA_NYCT.parquet
//!   routes/MTA_NYCT.parquet
//!   stops/MTA_NYCT.parquet
//! ```
//!
//! `service_date=` is not decoration. Arrow, pandas, Spark and Atoti all read
//! that directory name as a partition column and prune on it, so a query for
//! one day reads one file instead of the whole table. Writing the date as a
//! plain directory name would make every such query a full scan.
//!
//! One file per agency per date, rather than one merged file, because the three
//! MTA feeds are downloaded and re-ingested independently. Re-ingesting the
//! Subway must not require rewriting a file that also holds LIRR rows.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use tracing::{debug, info};

use crate::config::Agency;
use crate::dataset::{dimensions, facts};
use crate::error::Result;
use crate::gtfs::archive::StaticFeed;
use crate::schedule::Schedule;

/// Where each table lives under a data directory.
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("parquet"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// One agency's slice of one service date.
    pub fn fact_partition(&self, service_date: NaiveDate, agency_id: &str) -> PathBuf {
        self.root
            .join("scheduled_events")
            .join(format!("service_date={service_date}"))
            .join(format!("{agency_id}.parquet"))
    }

    pub fn dimension(&self, table: &str, agency_id: &str) -> PathBuf {
        self.root.join(table).join(format!("{agency_id}.parquet"))
    }
}

/// What a build produced, for the CLI to print and for tests to assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub agency_id: String,
    /// Dates that produced at least one event, and so at least one file.
    pub dates_written: usize,
    /// Dates in range with service defined but nothing to emit. Rare, and worth
    /// surfacing rather than hiding: it usually means a feed's trips were
    /// retired while its calendar was left behind.
    pub dates_empty: usize,
    pub events: usize,
    pub trips: usize,
    pub untimed_stop_times: usize,
    pub crossing_midnight: usize,
    pub routes: usize,
    pub stops: usize,
    pub first_date: Option<NaiveDate>,
    pub last_date: Option<NaiveDate>,
}

/// Resolve `feed` and write its Parquet dataset under `data_dir`.
///
/// `range` restricts output to an inclusive date window; `None` writes
/// everything the feed's calendar covers. Dates are written one at a time —
/// each is a partition and a whole feed at once would not fit comfortably in
/// memory.
pub fn build(
    feed: &StaticFeed,
    agency: &Agency,
    data_dir: &Path,
    range: Option<(NaiveDate, NaiveDate)>,
) -> Result<Summary> {
    let layout = Layout::new(data_dir);
    let schedule = Schedule::index(feed, agency)?;

    let dates: Vec<NaiveDate> = schedule
        .service_dates()
        .into_iter()
        .filter(|date| match range {
            Some((from, to)) => *date >= from && *date <= to,
            None => true,
        })
        .collect();

    let mut summary = Summary {
        agency_id: agency.id.clone(),
        dates_written: 0,
        dates_empty: 0,
        events: 0,
        trips: 0,
        untimed_stop_times: 0,
        crossing_midnight: 0,
        routes: feed.routes.len(),
        stops: feed.stops.len(),
        first_date: dates.first().copied(),
        last_date: dates.last().copied(),
    };

    for date in &dates {
        let (events, stats) = schedule.for_date(*date)?;

        summary.trips += stats.trips;
        summary.untimed_stop_times += stats.untimed_stop_times;
        summary.crossing_midnight += stats.crossing_midnight;

        // An empty partition is a file that every reader must open to learn it
        // has nothing. Counted instead of written.
        if events.is_empty() {
            summary.dates_empty += 1;
            debug!(agency = %agency.id, %date, "no events; partition skipped");
            continue;
        }

        facts::write_parquet(&layout.fact_partition(*date, &agency.id), &events)?;
        summary.dates_written += 1;
        summary.events += events.len();
    }

    // Dimensions are per-agency and not partitioned: a few thousand rows that
    // every query joins against.
    facts::write_batch(
        &layout.dimension("routes", &agency.id),
        &dimensions::routes_batch(feed, agency)?,
    )?;
    facts::write_batch(
        &layout.dimension("stops", &agency.id),
        &dimensions::stops_batch(feed, agency)?,
    )?;

    info!(
        agency = %agency.id,
        dates = summary.dates_written,
        events = summary.events,
        routes = summary.routes,
        stops = summary.stops,
        "wrote parquet dataset"
    );

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::path::Path as StdPath;

    use super::*;
    use crate::config::Config;
    use crate::gtfs::records::{
        AgencyRow, CalendarDateRow, RouteRow, StopRow, StopTimeRow, TripRow,
    };

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn agency() -> Agency {
        Config::load(StdPath::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/agencies.toml"
        )))
        .unwrap()
        .agency("MTA_NYCT")
        .unwrap()
        .clone()
    }

    /// One trip calling at two stops, running on three consecutive dates.
    fn feed() -> StaticFeed {
        StaticFeed {
            agency_id: "MTA_NYCT".to_string(),
            agencies: vec![AgencyRow {
                agency_id: Some("A".to_string()),
                agency_name: "Test".to_string(),
                agency_timezone: "America/New_York".to_string(),
            }],
            routes: vec![RouteRow {
                route_id: "1".to_string(),
                agency_id: Some("A".to_string()),
                route_short_name: Some("1".to_string()),
                route_long_name: None,
                route_type: 1,
                route_color: None,
            }],
            stops: vec![
                StopRow {
                    stop_id: "101N".to_string(),
                    stop_name: Some("First".to_string()),
                    stop_lat: Some(40.9),
                    stop_lon: Some(-73.9),
                    location_type: Some(0),
                    parent_station: Some("101".to_string()),
                },
                StopRow {
                    stop_id: "101".to_string(),
                    stop_name: Some("First".to_string()),
                    stop_lat: Some(40.9),
                    stop_lon: Some(-73.9),
                    location_type: Some(1),
                    parent_station: None,
                },
            ],
            trips: vec![TripRow {
                trip_id: "T1".to_string(),
                route_id: "1".to_string(),
                service_id: "SVC".to_string(),
                trip_headsign: None,
                direction_id: Some(0),
            }],
            stop_times: vec![StopTimeRow {
                trip_id: "T1".to_string(),
                stop_id: "101N".to_string(),
                arrival_time: "06:00:00".to_string(),
                departure_time: "06:00:00".to_string(),
                stop_sequence: 1,
            }],
            calendar: Vec::new(),
            calendar_dates: ["20260401", "20260402", "20260403"]
                .iter()
                .map(|d| CalendarDateRow {
                    service_id: "SVC".to_string(),
                    date: d.to_string(),
                    exception_type: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn writes_one_partition_per_service_date() {
        let dir = tempfile::tempdir().unwrap();
        let summary = build(&feed(), &agency(), dir.path(), None).unwrap();

        assert_eq!(summary.dates_written, 3);
        assert_eq!(summary.events, 3, "one call on each of three dates");
        assert_eq!(summary.first_date, Some(date(2026, 4, 1)));
        assert_eq!(summary.last_date, Some(date(2026, 4, 3)));
    }

    #[test]
    fn partition_directories_are_hive_style() {
        // Arrow, pandas, Spark and Atoti all read `service_date=` as a
        // partition column and prune on it. A plain directory name would turn
        // every single-day query into a full scan.
        let dir = tempfile::tempdir().unwrap();
        build(&feed(), &agency(), dir.path(), None).unwrap();

        let partition = dir
            .path()
            .join("parquet")
            .join("scheduled_events")
            .join("service_date=2026-04-01")
            .join("MTA_NYCT.parquet");
        assert!(partition.exists(), "expected {}", partition.display());
    }

    #[test]
    fn each_agency_gets_its_own_file_within_a_partition() {
        // The three feeds are downloaded and re-ingested independently.
        // Re-running the Subway must not rewrite a file holding LIRR rows.
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());

        assert_ne!(
            layout.fact_partition(date(2026, 4, 1), "MTA_NYCT"),
            layout.fact_partition(date(2026, 4, 1), "MTA_LIRR")
        );
        assert_eq!(
            layout
                .fact_partition(date(2026, 4, 1), "MTA_NYCT")
                .parent()
                .unwrap(),
            layout
                .fact_partition(date(2026, 4, 1), "MTA_LIRR")
                .parent()
                .unwrap(),
            "but they share the partition"
        );
    }

    #[test]
    fn a_date_range_restricts_output() {
        // Phase 1 is done when one command produces a service week; a feed
        // covering months should not force writing all of it.
        let dir = tempfile::tempdir().unwrap();
        let summary = build(
            &feed(),
            &agency(),
            dir.path(),
            Some((date(2026, 4, 2), date(2026, 4, 2))),
        )
        .unwrap();

        assert_eq!(summary.dates_written, 1);
        assert_eq!(summary.first_date, Some(date(2026, 4, 2)));
        assert!(!dir
            .path()
            .join("parquet/scheduled_events/service_date=2026-04-01")
            .exists());
    }

    #[test]
    fn a_range_matching_nothing_still_writes_dimensions() {
        // Dimensions do not depend on the date window, and a cube with facts
        // but no dimensions is worse than one with neither.
        let dir = tempfile::tempdir().unwrap();
        let summary = build(
            &feed(),
            &agency(),
            dir.path(),
            Some((date(2027, 1, 1), date(2027, 1, 7))),
        )
        .unwrap();

        assert_eq!(summary.dates_written, 0);
        assert_eq!(summary.first_date, None);
        assert!(dir.path().join("parquet/routes/MTA_NYCT.parquet").exists());
        assert!(dir.path().join("parquet/stops/MTA_NYCT.parquet").exists());
    }

    #[test]
    fn dimensions_are_written_once_not_per_partition() {
        let dir = tempfile::tempdir().unwrap();
        let summary = build(&feed(), &agency(), dir.path(), None).unwrap();

        assert_eq!(summary.routes, 1);
        assert_eq!(summary.stops, 2);

        let routes_dir = dir.path().join("parquet").join("routes");
        let files: Vec<_> = std::fs::read_dir(&routes_dir).unwrap().collect();
        assert_eq!(files.len(), 1, "one file per agency, not per date");
    }

    #[test]
    fn a_date_with_service_but_no_events_is_counted_not_written() {
        // A file every reader must open to learn it holds nothing is worse than
        // an absent partition -- but the count still needs surfacing, because
        // it usually means retired trips left behind a live calendar.
        let mut feed = feed();
        feed.stop_times.clear();

        let dir = tempfile::tempdir().unwrap();
        let summary = build(&feed, &agency(), dir.path(), None).unwrap();

        assert_eq!(summary.dates_written, 0);
        assert_eq!(summary.dates_empty, 3);
        assert!(!dir.path().join("parquet/scheduled_events").exists());
    }

    #[test]
    fn stats_are_summed_across_dates() {
        let mut feed = feed();
        feed.stop_times.push(StopTimeRow {
            trip_id: "T1".to_string(),
            stop_id: "101N".to_string(),
            arrival_time: "25:30:00".to_string(),
            departure_time: "25:30:00".to_string(),
            stop_sequence: 2,
        });

        let dir = tempfile::tempdir().unwrap();
        let summary = build(&feed, &agency(), dir.path(), None).unwrap();

        assert_eq!(summary.events, 6, "two calls across three dates");
        assert_eq!(summary.trips, 3, "one trip resolved on each date");
        assert_eq!(summary.crossing_midnight, 3);
    }

    #[test]
    fn rebuilding_over_an_existing_dataset_replaces_it() {
        // Re-ingest is the normal operation, not an error. A second run must
        // overwrite rather than append or fail.
        let dir = tempfile::tempdir().unwrap();
        let first = build(&feed(), &agency(), dir.path(), None).unwrap();
        let second = build(&feed(), &agency(), dir.path(), None).unwrap();

        assert_eq!(first, second);
    }
}
