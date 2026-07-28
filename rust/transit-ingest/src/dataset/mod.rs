//! Parquet output — the handoff from Rust to the cube and the Java service.
//!
//! - [`facts`] — the `scheduled_events` fact table and its Arrow schema
//! - [`dimensions`] — the `routes` and `stops` tables the cube joins against
//!
//! The partitioned directory layout lands alongside these.

pub mod dimensions;
pub mod facts;
