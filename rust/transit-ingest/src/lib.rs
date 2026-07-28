//! GTFS static and realtime ingest for Transit Delay Explorer.
//!
//! The library is the useful half; the binary is a thin CLI over it. Layout:
//!
//! - [`config`] — the agency registry, so adding an agency is config not code
//! - [`error`] — one error variant per failure mode, including per-file
//!   referential integrity violations
//! - [`gtfs`] — static feed row types and GTFS time handling
//! - [`schedule`] — the join from feed to scheduled stop events, per service date
//! - [`dataset`] — Parquet output, the handoff to the cube and the Java service
//! - [`fetch`] — downloading static archives, validated before they land
//!
//! The realtime poller lands in a later phase.

pub mod config;
pub mod dataset;
pub mod error;
pub mod fetch;
pub mod gtfs;
pub mod schedule;

pub use config::{Agency, Config};
pub use error::{Error, Result};
pub use schedule::{Schedule, ScheduledEvent};
