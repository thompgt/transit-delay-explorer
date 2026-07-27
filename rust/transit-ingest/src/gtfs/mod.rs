//! GTFS static feed parsing.
//!
//! - [`archive`] — reading the zip and its CSVs into row vectors
//! - [`records`] — one row type per file, tolerant of per-agency column sets
//! - [`time`] — GTFS times, which count past 24:00:00, and their resolution to
//!   absolute instants
//! - [`validate`] — foreign keys, which the format does not enforce

pub mod archive;
pub mod records;
pub mod time;
pub mod validate;

pub use archive::StaticFeed;
pub use records::{
    AgencyRow, CalendarDateRow, CalendarRow, RouteRow, StopRow, StopTimeRow, TripRow,
};
pub use time::{parse_date, GtfsTime};
