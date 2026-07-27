//! Referential integrity checks over a parsed static feed.
//!
//! GTFS is a relational model shipped as loose CSVs with nothing enforcing the
//! foreign keys. A `stop_times.txt` row can name a `trip_id` that appears in no
//! `trips.txt` row, and nothing in the format objects. Found late, that becomes
//! a cube with a silently missing slice; found here, it is one line of output
//! naming the file, the field, and the dangling value.
//!
//! Violations are aggregated by value rather than by row. The three MTA feeds
//! are clean today, but when a feed does break it usually breaks in bulk — one
//! retired stop referenced by forty thousand stop_times is one problem, and
//! printing it forty thousand times buries the second problem underneath it.

use std::collections::{HashMap, HashSet};

use crate::error::Violation;
use crate::gtfs::archive::StaticFeed;

/// Check every foreign key in `feed`. An empty vector means the feed is
/// internally consistent.
pub fn check(feed: &StaticFeed) -> Vec<Violation> {
    let mut dangling = Dangling::default();

    let route_ids: HashSet<&str> = feed.routes.iter().map(|r| r.route_id.as_str()).collect();
    let trip_ids: HashSet<&str> = feed.trips.iter().map(|t| t.trip_id.as_str()).collect();
    let stop_ids: HashSet<&str> = feed.stops.iter().map(|s| s.stop_id.as_str()).collect();
    let agency_ids: HashSet<&str> = feed
        .agencies
        .iter()
        .filter_map(|a| a.agency_id.as_deref())
        .collect();

    // Service ids come from either file. A feed may define a service in
    // calendar.txt, in calendar_dates.txt, or in both, and any of the three is
    // legal -- so the check is against the union, not against calendar.txt.
    let service_ids: HashSet<&str> = feed
        .calendar
        .iter()
        .map(|c| c.service_id.as_str())
        .chain(feed.calendar_dates.iter().map(|c| c.service_id.as_str()))
        .collect();

    for trip in &feed.trips {
        dangling.check(
            "trips.txt",
            "route_id",
            &trip.route_id,
            "routes.txt",
            &route_ids,
        );
        dangling.check(
            "trips.txt",
            "service_id",
            &trip.service_id,
            "calendar.txt/calendar_dates.txt",
            &service_ids,
        );
    }

    for stop_time in &feed.stop_times {
        dangling.check(
            "stop_times.txt",
            "trip_id",
            &stop_time.trip_id,
            "trips.txt",
            &trip_ids,
        );
        dangling.check(
            "stop_times.txt",
            "stop_id",
            &stop_time.stop_id,
            "stops.txt",
            &stop_ids,
        );
    }

    for stop in &feed.stops {
        if let Some(parent) = stop.parent_station.as_deref().filter(|p| !p.is_empty()) {
            dangling.check(
                "stops.txt",
                "parent_station",
                parent,
                "stops.txt",
                &stop_ids,
            );
        }
    }

    // Only checked when the feed names an agency at all. GTFS permits omitting
    // agency_id in a single-agency feed, and LIRR's routes.txt omits the column
    // entirely -- neither is a violation.
    if !agency_ids.is_empty() {
        for route in &feed.routes {
            if let Some(agency) = route.agency_id.as_deref().filter(|a| !a.is_empty()) {
                dangling.check("routes.txt", "agency_id", agency, "agency.txt", &agency_ids);
            }
        }
    }

    let mut violations = dangling.into_violations();
    violations.extend(duplicate_ids(feed));

    // Deterministic order so a diff of two validation runs is readable.
    violations.sort_by(|a, b| (&a.file, &a.field, &a.value).cmp(&(&b.file, &b.field, &b.value)));
    violations
}

/// Duplicated primary keys.
///
/// Not a dangling reference, but the same class of problem: a repeated
/// `stop_id` makes every join against it ambiguous, and the row that wins is
/// whichever the hash map happened to insert last.
fn duplicate_ids(feed: &StaticFeed) -> Vec<Violation> {
    let mut violations = Vec::new();

    let mut check = |file: &str, field: &str, ids: Vec<&str>| {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for id in ids {
            *counts.entry(id).or_default() += 1;
        }
        for (value, count) in counts {
            if count > 1 {
                violations.push(Violation {
                    file: file.to_string(),
                    field: field.to_string(),
                    value: value.to_string(),
                    references: "a unique key".to_string(),
                    occurrences: count,
                });
            }
        }
    };

    check(
        "routes.txt",
        "route_id",
        feed.routes.iter().map(|r| r.route_id.as_str()).collect(),
    );
    check(
        "stops.txt",
        "stop_id",
        feed.stops.iter().map(|s| s.stop_id.as_str()).collect(),
    );
    check(
        "trips.txt",
        "trip_id",
        feed.trips.iter().map(|t| t.trip_id.as_str()).collect(),
    );

    violations
}

/// Accumulates dangling references, counting rows per distinct bad value.
#[derive(Default)]
struct Dangling {
    counts: HashMap<(String, String, String, String), usize>,
}

impl Dangling {
    fn check(
        &mut self,
        file: &str,
        field: &str,
        value: &str,
        references: &str,
        valid: &HashSet<&str>,
    ) {
        if valid.contains(value) {
            return;
        }
        *self
            .counts
            .entry((
                file.to_string(),
                field.to_string(),
                value.to_string(),
                references.to_string(),
            ))
            .or_default() += 1;
    }

    fn into_violations(self) -> Vec<Violation> {
        self.counts
            .into_iter()
            .map(
                |((file, field, value, references), occurrences)| Violation {
                    file,
                    field,
                    value,
                    references,
                    occurrences,
                },
            )
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtfs::records::*;

    fn feed() -> StaticFeed {
        StaticFeed {
            agency_id: "TEST".to_string(),
            agencies: vec![AgencyRow {
                agency_id: Some("A".to_string()),
                agency_name: "Test".to_string(),
                agency_timezone: "America/New_York".to_string(),
            }],
            routes: vec![RouteRow {
                route_id: "R1".to_string(),
                agency_id: Some("A".to_string()),
                route_short_name: None,
                route_long_name: Some("Test Line".to_string()),
                route_type: 2,
                route_color: None,
            }],
            stops: vec![StopRow {
                stop_id: "S1".to_string(),
                stop_name: Some("First".to_string()),
                stop_lat: Some(40.7),
                stop_lon: Some(-73.9),
                location_type: Some(0),
                parent_station: None,
            }],
            trips: vec![TripRow {
                trip_id: "T1".to_string(),
                route_id: "R1".to_string(),
                service_id: "SVC".to_string(),
                trip_headsign: None,
                direction_id: Some(0),
            }],
            stop_times: vec![StopTimeRow {
                trip_id: "T1".to_string(),
                stop_id: "S1".to_string(),
                arrival_time: "06:00:00".to_string(),
                departure_time: "06:00:00".to_string(),
                stop_sequence: 1,
            }],
            calendar: Vec::new(),
            calendar_dates: vec![CalendarDateRow {
                service_id: "SVC".to_string(),
                date: "20260401".to_string(),
                exception_type: 1,
            }],
        }
    }

    #[test]
    fn a_consistent_feed_has_no_violations() {
        assert_eq!(check(&feed()), Vec::new());
    }

    #[test]
    fn catches_a_stop_time_pointing_at_no_trip() {
        let mut feed = feed();
        feed.stop_times[0].trip_id = "GHOST".to_string();

        let violations = check(&feed);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].file, "stop_times.txt");
        assert_eq!(violations[0].field, "trip_id");
        assert_eq!(violations[0].value, "GHOST");
        assert_eq!(violations[0].occurrences, 1);
    }

    #[test]
    fn aggregates_repeat_offenders_into_one_violation() {
        // The reason occurrences exists. One retired stop referenced by many
        // rows is one problem, not many.
        let mut feed = feed();
        for _ in 0..5_000 {
            let mut row = feed.stop_times[0].clone();
            row.stop_id = "RETIRED".to_string();
            feed.stop_times.push(row);
        }

        let violations = check(&feed);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].value, "RETIRED");
        assert_eq!(violations[0].occurrences, 5_000);
    }

    #[test]
    fn distinct_bad_values_stay_distinct() {
        let mut feed = feed();
        for id in ["GONE_A", "GONE_B"] {
            let mut row = feed.stop_times[0].clone();
            row.stop_id = id.to_string();
            feed.stop_times.push(row);
        }

        let violations = check(&feed);
        assert_eq!(violations.len(), 2);
        assert_eq!(violations[0].value, "GONE_A");
        assert_eq!(violations[1].value, "GONE_B");
    }

    #[test]
    fn service_id_may_come_from_either_calendar_file() {
        // LIRR and Metro-North define every service day in calendar_dates.txt.
        // Checking trips.service_id against calendar.txt alone would flag every
        // trip in both feeds.
        let feed = feed();
        assert!(feed.calendar.is_empty());
        assert_eq!(check(&feed), Vec::new());

        let mut via_calendar = feed;
        via_calendar.calendar = vec![CalendarRow {
            service_id: "SVC".to_string(),
            monday: 1,
            tuesday: 1,
            wednesday: 1,
            thursday: 1,
            friday: 1,
            saturday: 0,
            sunday: 0,
            start_date: "20260101".to_string(),
            end_date: "20261231".to_string(),
        }];
        via_calendar.calendar_dates.clear();
        assert_eq!(check(&via_calendar), Vec::new());
    }

    #[test]
    fn catches_a_trip_with_no_service_definition() {
        let mut feed = feed();
        feed.trips[0].service_id = "NOWHERE".to_string();

        let violations = check(&feed);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].field, "service_id");
    }

    #[test]
    fn catches_an_orphaned_parent_station() {
        let mut feed = feed();
        feed.stops[0].parent_station = Some("NOT_A_STATION".to_string());

        let violations = check(&feed);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].field, "parent_station");
    }

    #[test]
    fn an_empty_parent_station_is_absence_not_a_violation() {
        // Producers write an empty column rather than omitting it. A stop with
        // no parent is the normal case for a flat agency.
        let mut feed = feed();
        feed.stops[0].parent_station = Some(String::new());

        assert_eq!(check(&feed), Vec::new());
    }

    #[test]
    fn a_feed_that_omits_agency_id_on_routes_is_not_a_violation() {
        // LIRR's routes.txt has no agency_id column at all. GTFS allows this
        // for a single-agency feed, and flagging it would fail a valid feed.
        let mut feed = feed();
        feed.routes[0].agency_id = None;

        assert_eq!(check(&feed), Vec::new());
    }

    #[test]
    fn catches_a_route_naming_an_unknown_agency() {
        let mut feed = feed();
        feed.routes[0].agency_id = Some("SOMEONE_ELSE".to_string());

        let violations = check(&feed);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].references, "agency.txt");
    }

    #[test]
    fn catches_a_duplicated_primary_key() {
        let mut feed = feed();
        feed.stops.push(feed.stops[0].clone());

        let violations = check(&feed);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].value, "S1");
        assert_eq!(violations[0].occurrences, 2);
        assert!(violations[0].to_string().contains("unique key"));
    }

    #[test]
    fn violations_are_ordered_deterministically() {
        let mut feed = feed();
        feed.trips[0].route_id = "GHOST_ROUTE".to_string();
        feed.stops[0].parent_station = Some("GHOST_PARENT".to_string());

        let first = check(&feed);
        let second = check(&feed);
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        // stop_times.txt sorts after stops.txt, which sorts after trips.txt --
        // by file, so all of one file's problems read together.
        assert_eq!(first[0].file, "stops.txt");
        assert_eq!(first[1].file, "trips.txt");
    }
}
