//! GTFS static feed parsing.
//!
//! - [`records`] — one row type per file, tolerant of per-agency column sets
//! - [`time`] — GTFS times, which count past 24:00:00, and their resolution to
//!   absolute instants

pub mod records;
pub mod time;

pub use records::{
    AgencyRow, CalendarDateRow, CalendarRow, RouteRow, StopRow, StopTimeRow, TripRow,
};
pub use time::{parse_date, GtfsTime};
