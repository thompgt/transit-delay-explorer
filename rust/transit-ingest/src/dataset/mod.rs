//! Parquet output — the handoff from Rust to the cube and the Java service.
//!
//! - [`facts`] — the `scheduled_events` fact table and its Arrow schema
//!
//! Dimension tables and the partitioned directory layout land alongside this.

pub mod facts;
