//! The `scheduled_events` fact table as Arrow, and its Parquet encoding.
//!
//! The schema here is the *full* fact schema from `docs/DATA_MODEL.md`,
//! including the five columns that only realtime data can populate. Phase 1
//! writes them as typed nulls.
//!
//! That is deliberate. The cube is built in Phase 2 against this table and
//! upgraded in Phase 5 to the realtime one; if the two had different schemas,
//! every hierarchy and measure defined in between would need reworking. Writing
//! a null column costs almost nothing — Parquet stores it as a definition-level
//! run — and buys a Phase 5 that swaps the data without touching the cube.
//!
//! `service_date` is deliberately *not* a column here. It is the partition key,
//! and it lives in the directory name — `service_date=2026-05-26/` — which is
//! where a Hive-partitioned dataset is supposed to keep it. Writing it in both
//! places is not harmless redundancy: pyarrow, Spark and every other
//! partition-aware reader fails outright on the dataset with
//! `Field service_date has incompatible types: date32[day] vs string`, because
//! the path supplies a string and the file supplies a date. Readers get it back
//! as a real date by declaring the partition schema, which
//! `transit_cube.dataset` does.
//!
//! Timestamps are microsecond-precision and explicitly UTC-tagged: GTFS
//! resolution already accounts for the agency timezone, and a naive timestamp
//! would invite a second, wrong conversion downstream.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Int32Array, RecordBatch, StringArray, TimestampMicrosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::error::{Error, Result};
use crate::schedule::ScheduledEvent;

/// Timestamps carry an explicit UTC tag rather than being naive.
const UTC: &str = "UTC";

/// The `scheduled_events` / `stop_events` schema.
pub fn schema() -> SchemaRef {
    let timestamp = || DataType::Timestamp(TimeUnit::Microsecond, Some(UTC.into()));

    Arc::new(Schema::new(vec![
        Field::new("event_id", DataType::Utf8, false),
        // service_date is absent by design — it is the partition key, supplied
        // by the directory name. See the module docs.
        Field::new("agency_id", DataType::Utf8, false),
        Field::new("route_id", DataType::Utf8, false),
        Field::new("route_key", DataType::Utf8, false),
        Field::new("trip_id", DataType::Utf8, false),
        Field::new("stop_id", DataType::Utf8, false),
        Field::new("stop_key", DataType::Utf8, false),
        Field::new("stop_sequence", DataType::Int32, false),
        // Nullable because GTFS makes direction_id optional and some LIRR trips
        // leave it blank. Defaulting to 0 would assert a direction the feed did
        // not state, and 0 is a real direction.
        Field::new("direction_id", DataType::Int32, true),
        Field::new("scheduled_arrival", timestamp(), false),
        Field::new("scheduled_departure", timestamp(), false),
        Field::new("dwell_seconds", DataType::Int32, false),
        Field::new("crosses_midnight", DataType::Boolean, false),
        // Realtime columns: null in Phase 1, populated in Phase 3.
        Field::new("actual_arrival", timestamp(), true),
        Field::new("delay_seconds", DataType::Int32, true),
        Field::new("headway_seconds", DataType::Int32, true),
        Field::new("is_cancelled", DataType::Boolean, true),
        Field::new("vehicle_id", DataType::Utf8, true),
    ]))
}

/// Encode one service date's events as a record batch.
pub fn to_record_batch(events: &[ScheduledEvent]) -> Result<RecordBatch> {
    let schema = schema();
    let rows = events.len();

    let timestamp = |values: Vec<i64>| -> ArrayRef {
        Arc::new(TimestampMicrosecondArray::from(values).with_timezone(UTC))
    };

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|e| e.event_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|e| e.agency_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|e| e.route_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|e| e.route_key.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|e| e.trip_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|e| e.stop_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|e| e.stop_key.as_str()),
        )),
        Arc::new(Int32Array::from_iter_values(
            events.iter().map(|e| e.stop_sequence as i32),
        )),
        Arc::new(Int32Array::from_iter(events.iter().map(|e| e.direction_id))),
        timestamp(
            events
                .iter()
                .map(|e| e.scheduled_arrival.timestamp_micros())
                .collect(),
        ),
        timestamp(
            events
                .iter()
                .map(|e| e.scheduled_departure.timestamp_micros())
                .collect(),
        ),
        Arc::new(Int32Array::from_iter_values(
            events.iter().map(|e| e.dwell_seconds),
        )),
        Arc::new(BooleanArray::from_iter(
            events.iter().map(|e| Some(e.crosses_midnight)),
        )),
        // By name rather than by position: these are the columns most likely to
        // move when the schema changes, and a positional slip here would write
        // nulls of the wrong type without any complaint from the compiler.
        nulls(&schema, "actual_arrival", rows),
        nulls(&schema, "delay_seconds", rows),
        nulls(&schema, "headway_seconds", rows),
        nulls(&schema, "is_cancelled", rows),
        nulls(&schema, "vehicle_id", rows),
    ];

    RecordBatch::try_new(schema, columns).map_err(Error::from)
}

/// Write one service date's events to `path`, creating parent directories.
pub fn write_parquet(path: &Path, events: &[ScheduledEvent]) -> Result<()> {
    let batch = to_record_batch(events)?;
    write_batch(path, &batch)
}

/// Write a single batch, with the properties every table in this project uses.
pub fn write_batch(path: &Path, batch: &RecordBatch) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let file = File::create(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;

    // Zstd over Snappy: this data is overwhelmingly low-cardinality strings and
    // near-sorted timestamps, which zstd compresses several times better, and
    // the cube reads each partition far more often than the ingest writes it.
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build();

    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(properties))
        .map_err(|source| parquet_error(path, source))?;
    writer
        .write(batch)
        .map_err(|source| parquet_error(path, source))?;
    writer
        .close()
        .map_err(|source| parquet_error(path, source))?;

    Ok(())
}

fn parquet_error(path: &Path, source: parquet::errors::ParquetError) -> Error {
    Error::Parquet {
        path: path.to_path_buf(),
        source: Box::new(source),
    }
}

/// An all-null column of the schema's declared type for `name`.
fn nulls(schema: &SchemaRef, name: &str, rows: usize) -> ArrayRef {
    let field = schema
        .field_with_name(name)
        .expect("schema() and to_record_batch() must name the same columns");
    arrow::array::new_null_array(field.data_type(), rows)
}

#[cfg(test)]
mod tests {
    use arrow::array::Array;
    use chrono::{DateTime, NaiveDate, Utc};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn instant(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn event(sequence: u32) -> ScheduledEvent {
        ScheduledEvent {
            event_id: format!("id{sequence}"),
            service_date: date(2026, 4, 1),
            agency_id: "MTA_NYCT".to_string(),
            route_id: "1".to_string(),
            route_key: "MTA_NYCT:1".to_string(),
            trip_id: "T1".to_string(),
            stop_id: "101N".to_string(),
            stop_key: "MTA_NYCT:101N".to_string(),
            stop_sequence: sequence,
            direction_id: Some(0),
            scheduled_arrival: instant("2026-04-01T10:00:00Z"),
            scheduled_departure: instant("2026-04-01T10:00:30Z"),
            dwell_seconds: 30,
            crosses_midnight: false,
        }
    }

    /// Write, read back, and concatenate. The reader yields batches of its own
    /// choosing — 1024 rows by default, and none at all for an empty file — so
    /// a test that took the first batch would be asserting about the reader's
    /// batching rather than about what was written.
    fn round_trip(events: &[ScheduledEvent]) -> RecordBatch {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("part.parquet");
        write_parquet(&path, events).unwrap();

        let file = File::open(&path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let read_schema = builder.schema().clone();
        let batches: Vec<RecordBatch> = builder
            .build()
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();

        // The schema read back must be the schema written; otherwise the cube
        // is reading something other than what this module claims to produce.
        assert_eq!(read_schema, schema());

        arrow::compute::concat_batches(&read_schema, &batches).unwrap()
    }

    #[test]
    fn the_schema_matches_the_documented_fact_table() {
        let schema = schema();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();

        assert_eq!(
            names,
            [
                "event_id",
                "agency_id",
                "route_id",
                "route_key",
                "trip_id",
                "stop_id",
                "stop_key",
                "stop_sequence",
                "direction_id",
                "scheduled_arrival",
                "scheduled_departure",
                "dwell_seconds",
                "crosses_midnight",
                "actual_arrival",
                "delay_seconds",
                "headway_seconds",
                "is_cancelled",
                "vehicle_id",
            ]
        );
    }

    #[test]
    fn realtime_columns_are_present_and_null() {
        // The whole point: Phase 5 swaps the data without the cube's schema
        // changing underneath it.
        let batch = to_record_batch(&[event(1), event(2)]).unwrap();

        for column in [
            "actual_arrival",
            "delay_seconds",
            "headway_seconds",
            "is_cancelled",
            "vehicle_id",
        ] {
            let index = batch.schema().index_of(column).unwrap();
            let array = batch.column(index);
            assert_eq!(array.len(), 2, "{column} must have a value per row");
            assert_eq!(array.null_count(), 2, "{column} must be entirely null");
        }
    }

    #[test]
    fn keys_and_measures_survive_a_parquet_round_trip() {
        let batch = round_trip(&[event(1)]);

        let keys = batch
            .column(batch.schema().index_of("stop_key").unwrap())
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(keys.value(0), "MTA_NYCT:101N");

        let dwell = batch
            .column(batch.schema().index_of("dwell_seconds").unwrap())
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert_eq!(dwell.value(0), 30);
    }

    #[test]
    fn timestamps_round_trip_as_utc_microseconds() {
        // A naive timestamp here would invite a second timezone conversion
        // downstream, on data that has already been resolved once.
        let batch = round_trip(&[event(1)]);
        let index = batch.schema().index_of("scheduled_arrival").unwrap();

        assert_eq!(
            batch.schema().field(index).data_type(),
            &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
        );

        let arrivals = batch
            .column(index)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(
            arrivals.value(0),
            instant("2026-04-01T10:00:00Z").timestamp_micros()
        );
    }

    #[test]
    fn service_date_is_not_a_column() {
        // It is the partition key and lives in the directory name. Writing it
        // in both places makes every partition-aware reader fail on the whole
        // dataset with a date32-versus-string type conflict.
        let batch = round_trip(&[event(1)]);

        assert!(
            batch.schema().index_of("service_date").is_err(),
            "service_date must come from the partition path, not the file"
        );
    }

    #[test]
    fn an_absent_direction_stays_null_rather_than_becoming_zero() {
        // 0 is a real direction. Defaulting to it would invent a fact.
        let mut event = event(1);
        event.direction_id = None;
        let batch = round_trip(&[event]);

        let index = batch.schema().index_of("direction_id").unwrap();
        assert_eq!(batch.column(index).null_count(), 1);
    }

    #[test]
    fn an_empty_date_writes_a_readable_file_with_no_rows() {
        // A service date with nothing scheduled is a real state -- a holiday on
        // a weekday-only feed. It must not produce an unreadable file.
        let batch = round_trip(&[]);
        assert_eq!(batch.num_rows(), 0);
        assert_eq!(batch.num_columns(), schema().fields().len());
    }

    #[test]
    fn writing_creates_missing_parent_directories() {
        // Partition paths are several levels deep and do not exist yet.
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("scheduled_events")
            .join("service_date=2026-04-01")
            .join("MTA_NYCT.parquet");

        write_parquet(&path, &[event(1)]).unwrap();
        assert!(path.exists());
    }

    /// The golden wire fixture must speak this schema's vocabulary.
    ///
    /// The Java consumer parses the same file. Without this, the two halves of
    /// the project had no shared definition of the message: the Parquet columns
    /// are snake_case, the Java record's components are camelCase, and a
    /// producer following either one would deserialize into a record of nulls
    /// on the other side without an error anywhere.
    #[test]
    fn the_wire_contract_speaks_the_fact_table_vocabulary() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/stop_event.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is the shared wire contract: {e}", path.display()));
        let fixture: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&text).unwrap();

        let schema = schema();
        for key in fixture.keys() {
            // service_date is the partition key: it belongs on the wire, but it
            // lives in the directory name rather than in the file.
            assert!(
                key == "service_date" || schema.field_with_name(key).is_ok(),
                "wire field {key} is not a fact-table column"
            );
        }

        // And the other direction: every schedule column has to be on the wire,
        // or the realtime producer is dropping something the cube reads.
        for field in schema.fields() {
            assert!(
                fixture.contains_key(field.name()),
                "fact-table column {} is missing from the wire contract",
                field.name()
            );
        }
    }

    #[test]
    fn a_row_count_survives_many_rows() {
        let events: Vec<_> = (1..=5_000).map(event).collect();
        let batch = round_trip(&events);
        assert_eq!(batch.num_rows(), 5_000);
    }
}
