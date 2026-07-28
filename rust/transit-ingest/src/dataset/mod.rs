//! Parquet output — the handoff from Rust to the cube and the Java service.
//!
//! - [`facts`] — the `scheduled_events` fact table and its Arrow schema
//! - [`dimensions`] — the `routes` and `stops` tables the cube joins against
//! - [`write`] — the partitioned directory layout and the whole-dataset build

pub mod dimensions;
pub mod facts;
pub mod write;

pub use write::{build, Layout, Summary};
