//! GTFS static and realtime ingest for Transit Delay Explorer.
//!
//! The library is the useful half; the binary is a thin CLI over it. Layout:
//!
//! - [`config`] — the agency registry, so adding an agency is config not code
//! - [`error`] — one error variant per failure mode, including per-file
//!   referential integrity violations
//!
//! Static parsing, timestamp resolution, Parquet output, and the realtime
//! poller land in later phases.

pub mod config;
pub mod error;

pub use config::{Agency, Config};
pub use error::{Error, Result};
