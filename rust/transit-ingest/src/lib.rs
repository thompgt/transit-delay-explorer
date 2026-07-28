//! GTFS static and realtime ingest for Transit Delay Explorer.
//!
//! The library is the useful half; the binary is a thin CLI over it. Layout:
//!
//! - [`config`] — the agency registry, so adding an agency is config not code
//! - [`error`] — one error variant per failure mode, including per-file
//!   referential integrity violations
//! - [`gtfs`] — static feed row types and GTFS time handling
//! - [`schedule`] — the join from feed to scheduled stop events, per service date
//!
//! Parquet output and the realtime poller land in later phases.

pub mod config;
pub mod error;
pub mod gtfs;
pub mod schedule;

pub use config::{Agency, Config};
pub use error::{Error, Result};
pub use schedule::{Schedule, ScheduledEvent};
