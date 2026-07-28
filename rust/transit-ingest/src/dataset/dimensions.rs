//! The `routes` and `stops` dimension tables.
//!
//! Dimensions are small — thousands of rows against millions of facts — and
//! they are where a cube's hierarchies actually live, so the work here is
//! mostly about not leaving holes a hierarchy level can fall through.
//!
//! Two of those are worth stating outright.
//!
//! **The geography hierarchy is ragged and must not be.** GTFS models a station
//! as a stop whose children are platforms, but only the Subway uses it: 496
//! stations parenting 992 platforms, against LIRR and Metro-North where every
//! stop is flat and `parent_station` is empty. A `station_key` that is null for
//! two agencies breaks the level for all three. So a stop with no parent is its
//! own station, and `borough → station → platform` stays walkable everywhere.
//!
//! **`borough` and `municipality` exist in no GTFS feed.** They are derived
//! from coordinates against boundary geometry, which lands with the cube. Until
//! then they are `Unknown` rather than null — same reason. A null in a
//! hierarchy level is a hole; a literal `Unknown` is a slice you can select.

use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Float64Array, Int32Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};

use crate::config::Agency;
use crate::error::{Error, Result};
use crate::gtfs::archive::StaticFeed;

/// Placeholder for a hierarchy level not yet derivable. Never null — see the
/// module docs.
pub const UNKNOWN: &str = "Unknown";

pub fn routes_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("route_key", DataType::Utf8, false),
        Field::new("route_id", DataType::Utf8, false),
        Field::new("agency_id", DataType::Utf8, false),
        // The agency's mode from config, not from the feed: `route_type` is a
        // per-route integer and the network hierarchy wants one label per
        // agency above it.
        Field::new("mode", DataType::Utf8, false),
        Field::new("route_type", DataType::Int32, false),
        Field::new("route_short_name", DataType::Utf8, true),
        Field::new("route_long_name", DataType::Utf8, true),
        // Never null: whatever a rider would call this route, falling back
        // through long name to the id. A hierarchy level cannot be null.
        Field::new("display_name", DataType::Utf8, false),
        Field::new("route_color", DataType::Utf8, true),
    ]))
}

pub fn stops_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("stop_key", DataType::Utf8, false),
        Field::new("stop_id", DataType::Utf8, false),
        Field::new("agency_id", DataType::Utf8, false),
        Field::new("stop_name", DataType::Utf8, true),
        // Nullable, and emphatically not defaulted to zero: (0, 0) is in the
        // Atlantic and would drag any geographic rollup toward it.
        Field::new("stop_lat", DataType::Float64, true),
        Field::new("stop_lon", DataType::Float64, true),
        Field::new("is_station", DataType::Boolean, false),
        // The raw GTFS link, null for a stop with no parent.
        Field::new("parent_station_key", DataType::Utf8, true),
        // The hierarchy level: parent if there is one, self if not.
        Field::new("station_key", DataType::Utf8, false),
        Field::new("borough", DataType::Utf8, false),
        Field::new("municipality", DataType::Utf8, false),
    ]))
}

pub fn routes_batch(feed: &StaticFeed, agency: &Agency) -> Result<RecordBatch> {
    let routes = &feed.routes;

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            routes.iter().map(|r| agency.route_key(&r.route_id)),
        )),
        Arc::new(StringArray::from_iter_values(
            routes.iter().map(|r| r.route_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            routes.iter().map(|_| agency.id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            routes.iter().map(|_| agency.mode.as_str()),
        )),
        Arc::new(Int32Array::from_iter_values(
            routes.iter().map(|r| r.route_type),
        )),
        Arc::new(StringArray::from_iter(
            routes
                .iter()
                .map(|r| non_empty(r.route_short_name.as_deref())),
        )),
        Arc::new(StringArray::from_iter(
            routes
                .iter()
                .map(|r| non_empty(r.route_long_name.as_deref())),
        )),
        Arc::new(StringArray::from_iter_values(
            routes.iter().map(|r| r.display_name()),
        )),
        Arc::new(StringArray::from_iter(
            routes.iter().map(|r| non_empty(r.route_color.as_deref())),
        )),
    ];

    RecordBatch::try_new(routes_schema(), columns).map_err(Error::from)
}

pub fn stops_batch(feed: &StaticFeed, agency: &Agency) -> Result<RecordBatch> {
    let stops = &feed.stops;

    let parent_key = |stop: &crate::gtfs::records::StopRow| {
        non_empty(stop.parent_station.as_deref()).map(|p| agency.stop_key(p))
    };

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            stops.iter().map(|s| agency.stop_key(&s.stop_id)),
        )),
        Arc::new(StringArray::from_iter_values(
            stops.iter().map(|s| s.stop_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            stops.iter().map(|_| agency.id.as_str()),
        )),
        Arc::new(StringArray::from_iter(
            stops.iter().map(|s| non_empty(s.stop_name.as_deref())),
        )),
        Arc::new(Float64Array::from_iter(stops.iter().map(|s| s.stop_lat))),
        Arc::new(Float64Array::from_iter(stops.iter().map(|s| s.stop_lon))),
        Arc::new(BooleanArray::from_iter(
            stops.iter().map(|s| Some(s.is_station())),
        )),
        Arc::new(StringArray::from_iter(stops.iter().map(parent_key))),
        Arc::new(StringArray::from_iter_values(stops.iter().map(|s| {
            // A stop with no parent is its own station. This is what keeps the
            // geography hierarchy walkable for the flat railroads.
            parent_key(s).unwrap_or_else(|| agency.stop_key(&s.stop_id))
        }))),
        Arc::new(StringArray::from_iter_values(stops.iter().map(|_| UNKNOWN))),
        Arc::new(StringArray::from_iter_values(stops.iter().map(|_| UNKNOWN))),
    ];

    RecordBatch::try_new(stops_schema(), columns).map_err(Error::from)
}

/// A present-but-empty CSV field is absence, not an empty string. Producers
/// write both and the difference is not meaningful; carrying `Some("")` into a
/// dimension would put a blank label in a hierarchy.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use arrow::array::Array;

    use super::*;
    use crate::config::Config;
    use crate::gtfs::records::{AgencyRow, CalendarDateRow, RouteRow, StopRow};

    fn config() -> Config {
        Config::load(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/agencies.toml"
        )))
        .unwrap()
    }

    fn subway() -> Agency {
        config().agency("MTA_NYCT").unwrap().clone()
    }

    fn lirr() -> Agency {
        config().agency("MTA_LIRR").unwrap().clone()
    }

    fn feed(routes: Vec<RouteRow>, stops: Vec<StopRow>) -> StaticFeed {
        StaticFeed {
            agency_id: "TEST".to_string(),
            agencies: vec![AgencyRow {
                agency_id: Some("A".to_string()),
                agency_name: "Test".to_string(),
                agency_timezone: "America/New_York".to_string(),
            }],
            routes,
            stops,
            trips: Vec::new(),
            stop_times: Vec::new(),
            calendar: Vec::new(),
            calendar_dates: vec![CalendarDateRow {
                service_id: "SVC".to_string(),
                date: "20260401".to_string(),
                exception_type: 1,
            }],
        }
    }

    fn route(id: &str, short: Option<&str>, long: Option<&str>) -> RouteRow {
        RouteRow {
            route_id: id.to_string(),
            agency_id: Some("A".to_string()),
            route_short_name: short.map(str::to_string),
            route_long_name: long.map(str::to_string),
            route_type: 1,
            route_color: Some("EE352E".to_string()),
        }
    }

    fn stop(id: &str, location_type: Option<i32>, parent: Option<&str>) -> StopRow {
        StopRow {
            stop_id: id.to_string(),
            stop_name: Some(format!("Stop {id}")),
            stop_lat: Some(40.7),
            stop_lon: Some(-73.9),
            location_type,
            parent_station: parent.map(str::to_string),
        }
    }

    fn strings<'a>(batch: &'a RecordBatch, column: &str) -> &'a StringArray {
        batch
            .column(batch.schema().index_of(column).unwrap())
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
    }

    #[test]
    fn routes_carry_the_namespaced_key_and_the_agency_mode() {
        let feed = feed(vec![route("1", Some("1"), Some("Broadway"))], Vec::new());
        let batch = routes_batch(&feed, &subway()).unwrap();

        assert_eq!(strings(&batch, "route_key").value(0), "MTA_NYCT:1");
        assert_eq!(strings(&batch, "route_id").value(0), "1");
        assert_eq!(strings(&batch, "mode").value(0), "subway");
    }

    #[test]
    fn the_same_route_id_yields_distinct_keys_per_agency() {
        // Route 1 is a Broadway local and a Babylon branch train. Nothing but
        // the key keeps them apart.
        let feed = feed(vec![route("1", Some("1"), None)], Vec::new());

        let subway_batch = routes_batch(&feed, &subway()).unwrap();
        let lirr_batch = routes_batch(&feed, &lirr()).unwrap();

        assert_ne!(
            strings(&subway_batch, "route_key").value(0),
            strings(&lirr_batch, "route_key").value(0)
        );
    }

    #[test]
    fn display_name_is_never_null() {
        // Three feeds name routes three ways, and a null breaks the level.
        let feed = feed(
            vec![
                route("1", Some("1"), Some("Broadway")),
                route("2", None, Some("Babylon")),
                route("3", None, None),
            ],
            Vec::new(),
        );
        let batch = routes_batch(&feed, &subway()).unwrap();

        let names = strings(&batch, "display_name");
        assert_eq!(names.null_count(), 0);
        assert_eq!(names.value(0), "1");
        assert_eq!(names.value(1), "Babylon");
        assert_eq!(names.value(2), "3", "falls back to the id");
    }

    #[test]
    fn an_empty_name_column_reads_as_absent_not_blank() {
        // Producers write an empty field rather than omitting it. A blank label
        // in a hierarchy is worse than an honest null.
        let feed = feed(vec![route("1", Some(""), Some("Broadway"))], Vec::new());
        let batch = routes_batch(&feed, &subway()).unwrap();

        assert_eq!(strings(&batch, "route_short_name").null_count(), 1);
        assert_eq!(strings(&batch, "display_name").value(0), "Broadway");
    }

    #[test]
    fn a_platform_points_at_its_parent_station() {
        let feed = feed(
            Vec::new(),
            vec![
                stop("101", Some(1), None),
                stop("101N", Some(0), Some("101")),
            ],
        );
        let batch = stops_batch(&feed, &subway()).unwrap();

        assert_eq!(
            strings(&batch, "parent_station_key").value(1),
            "MTA_NYCT:101"
        );
        assert_eq!(strings(&batch, "station_key").value(1), "MTA_NYCT:101");
    }

    #[test]
    fn a_flat_agencys_stop_is_its_own_station() {
        // LIRR and Metro-North have no parent_station at all. Leaving
        // station_key null there would break the geography level for the
        // Subway too, since a hierarchy is defined once across all agencies.
        let feed = feed(Vec::new(), vec![stop("102", None, None)]);
        let batch = stops_batch(&feed, &lirr()).unwrap();

        let stations = strings(&batch, "station_key");
        assert_eq!(stations.null_count(), 0);
        assert_eq!(stations.value(0), "MTA_LIRR:102");
        assert_eq!(
            strings(&batch, "parent_station_key").null_count(),
            1,
            "the raw GTFS link stays honestly absent"
        );
    }

    #[test]
    fn an_empty_parent_station_is_treated_as_no_parent() {
        let feed = feed(Vec::new(), vec![stop("102", None, Some(""))]);
        let batch = stops_batch(&feed, &lirr()).unwrap();

        assert_eq!(strings(&batch, "parent_station_key").null_count(), 1);
        assert_eq!(strings(&batch, "station_key").value(0), "MTA_LIRR:102");
    }

    #[test]
    fn stations_are_distinguished_from_platforms() {
        let feed = feed(
            Vec::new(),
            vec![
                stop("101", Some(1), None),
                stop("101N", Some(0), Some("101")),
            ],
        );
        let batch = stops_batch(&feed, &subway()).unwrap();

        let is_station = batch
            .column(batch.schema().index_of("is_station").unwrap())
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert!(is_station.value(0));
        assert!(!is_station.value(1));
    }

    #[test]
    fn missing_coordinates_stay_null_rather_than_becoming_the_atlantic() {
        let mut feed = feed(Vec::new(), vec![stop("102", None, None)]);
        feed.stops[0].stop_lat = None;
        feed.stops[0].stop_lon = None;
        let batch = stops_batch(&feed, &lirr()).unwrap();

        for column in ["stop_lat", "stop_lon"] {
            let index = batch.schema().index_of(column).unwrap();
            assert_eq!(batch.column(index).null_count(), 1, "{column}");
        }
    }

    #[test]
    fn geography_levels_are_unknown_rather_than_null() {
        // Derived from boundary geometry later. Until then a selectable
        // "Unknown" beats a hole in the hierarchy.
        let feed = feed(Vec::new(), vec![stop("102", None, None)]);
        let batch = stops_batch(&feed, &lirr()).unwrap();

        for column in ["borough", "municipality"] {
            assert_eq!(strings(&batch, column).null_count(), 0, "{column}");
            assert_eq!(strings(&batch, column).value(0), UNKNOWN);
        }
    }

    #[test]
    fn an_empty_feed_yields_empty_batches_with_the_right_shape() {
        let feed = feed(Vec::new(), Vec::new());

        let routes = routes_batch(&feed, &subway()).unwrap();
        assert_eq!(routes.num_rows(), 0);
        assert_eq!(routes.schema(), routes_schema());

        let stops = stops_batch(&feed, &subway()).unwrap();
        assert_eq!(stops.num_rows(), 0);
        assert_eq!(stops.schema(), stops_schema());
    }
}
