//! Resolving a parsed static feed into scheduled stop events.
//!
//! This is the join GTFS leaves to the reader. A `stop_times.txt` row says
//! "trip T calls at stop S at 25:14:00", which is neither a date nor a time
//! anyone can subtract. Turning it into a fact row means walking
//! `stop_times → trips → service_id → service dates`, then resolving each time
//! against each date in the agency's timezone.
//!
//! Two decisions shape the API:
//!
//! **Resolution is per service date.** A week of Subway service is millions of
//! events; materialising a whole feed at once is gigabytes for no benefit. The
//! fact table is partitioned by `service_date` anyway, so [`Schedule::for_date`]
//! produces exactly one partition and the caller streams them out one at a time.
//!
//! **The indexes are built once.** Grouping `stop_times` by trip is the
//! expensive part and it does not vary by date, so it happens in
//! [`Schedule::index`] and every date after the first is a lookup.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use tracing::{debug, warn};

use crate::config::Agency;
use crate::error::Result;
use crate::gtfs::archive::StaticFeed;
use crate::gtfs::calendar::ServiceCalendar;
use crate::gtfs::records::{StopTimeRow, TripRow};
use crate::gtfs::time::GtfsTime;

/// One scheduled call at one stop, on one service date.
///
/// Realtime columns from the fact table (`actual_arrival`, `delay_seconds`,
/// `is_cancelled`, `vehicle_id`) are absent rather than present-and-null: they
/// are not knowable from a static feed, and a struct field that is always
/// `None` invites code that pretends otherwise. The Parquet writer emits them
/// as typed nulls so the cube schema does not change in Phase 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledEvent {
    /// Stable across runs — see [`event_id`].
    pub event_id: String,
    pub service_date: NaiveDate,
    pub agency_id: String,
    pub route_id: String,
    pub route_key: String,
    pub trip_id: String,
    pub stop_id: String,
    pub stop_key: String,
    pub stop_sequence: u32,
    pub direction_id: Option<i32>,
    pub scheduled_arrival: DateTime<Utc>,
    pub scheduled_departure: DateTime<Utc>,
    /// Scheduled departure minus arrival. Zero for the many feeds that publish
    /// one time for both.
    pub dwell_seconds: i32,
    /// True when the GTFS arrival time was 24:00:00 or later, so this call
    /// happens on the calendar day after its service date. Kept as a column
    /// because it is the single most useful thing to filter on when a
    /// downstream count looks wrong.
    pub crosses_midnight: bool,
}

/// What resolving a date did, for logging and for the `inspect` output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub events: usize,
    pub trips: usize,
    /// Stop times carrying neither an arrival nor a departure. GTFS permits
    /// this at non-timepoint stops, expecting the consumer to interpolate. We
    /// do not: an interpolated time is a guess, and a delay measured against a
    /// guess is not a measurement. They are counted and dropped.
    pub untimed_stop_times: usize,
    /// Stop times whose arrival or departure could not be parsed at all — an
    /// `8:5:00`, a stray `HH:MM`, a blank that is not blank. Counted and
    /// dropped like an untimed row rather than aborting the build: the
    /// validator's whole philosophy is to report everything in one pass, and a
    /// single bad cell taking down a seven-day tri-agency run means the operator
    /// learns about one broken row per attempt.
    pub malformed_times: usize,
    pub crossing_midnight: usize,
}

/// A feed indexed for date-by-date resolution.
pub struct Schedule<'a> {
    agency: &'a Agency,
    tz: Tz,
    calendar: ServiceCalendar,
    /// Trips grouped by service, so a date resolves to trips without scanning.
    trips_by_service: HashMap<&'a str, Vec<&'a TripRow>>,
    /// Stop times grouped by trip, in `stop_sequence` order.
    stop_times_by_trip: HashMap<&'a str, Vec<&'a StopTimeRow>>,
}

impl<'a> Schedule<'a> {
    /// Build the indexes. Cost is proportional to feed size and paid once.
    pub fn index(feed: &'a StaticFeed, agency: &'a Agency) -> Result<Self> {
        let tz = agency.tz()?;
        let calendar = ServiceCalendar::build(feed)?;

        let mut trips_by_service: HashMap<&str, Vec<&TripRow>> = HashMap::new();
        for trip in &feed.trips {
            trips_by_service
                .entry(trip.service_id.as_str())
                .or_default()
                .push(trip);
        }

        let mut stop_times_by_trip: HashMap<&str, Vec<&StopTimeRow>> = HashMap::new();
        for stop_time in &feed.stop_times {
            stop_times_by_trip
                .entry(stop_time.trip_id.as_str())
                .or_default()
                .push(stop_time);
        }
        // GTFS does not require stop_times.txt to be sorted, and the order of
        // calls along a trip is load-bearing for headway and dwell. Sorting once
        // here is cheaper than sorting per date.
        for stop_times in stop_times_by_trip.values_mut() {
            stop_times.sort_unstable_by_key(|st| st.stop_sequence);
        }

        debug!(
            agency = %agency.id,
            services = calendar.service_count(),
            trips = feed.trips.len(),
            "indexed feed for resolution"
        );

        Ok(Self {
            agency,
            tz,
            calendar,
            trips_by_service,
            stop_times_by_trip,
        })
    }

    pub fn calendar(&self) -> &ServiceCalendar {
        &self.calendar
    }

    /// Dates this feed has service on, ascending.
    pub fn service_dates(&self) -> Vec<NaiveDate> {
        self.calendar.active_dates().into_iter().collect()
    }

    /// Every scheduled call on `service_date`, plus what it took to get there.
    ///
    /// Ordered by trip then `stop_sequence`, with trips in feed order, so two
    /// runs over the same feed produce byte-identical Parquet.
    pub fn for_date(&self, service_date: NaiveDate) -> Result<(Vec<ScheduledEvent>, Stats)> {
        let mut events = Vec::new();
        let mut stats = Stats::default();

        for service_id in self.calendar.services_on(service_date) {
            let Some(trips) = self.trips_by_service.get(service_id.as_str()) else {
                // A service with dates but no trips is legal and inert. It
                // usually means a feed retired the trips and left the calendar.
                continue;
            };

            for trip in trips {
                let Some(stop_times) = self.stop_times_by_trip.get(trip.trip_id.as_str()) else {
                    // Validation reports a trip with no stop_times as its own
                    // problem; here it is simply nothing to emit.
                    continue;
                };
                stats.trips += 1;

                for stop_time in stop_times {
                    // A row this malformed cannot be measured against, which is
                    // the same situation as an untimed one and gets the same
                    // treatment: count it, drop it, keep going.
                    let (Ok(arrival), Ok(departure)) = (
                        GtfsTime::parse(&stop_time.arrival_time, "stop_times.txt"),
                        GtfsTime::parse(&stop_time.departure_time, "stop_times.txt"),
                    ) else {
                        stats.malformed_times += 1;
                        continue;
                    };

                    // One time standing in for the other is the common case at
                    // stops where a vehicle does not wait; a stop with neither
                    // cannot be measured against and is dropped.
                    let (arrival, departure) = match (arrival, departure) {
                        (Some(a), Some(d)) => (a, d),
                        (Some(a), None) => (a, a),
                        (None, Some(d)) => (d, d),
                        (None, None) => {
                            stats.untimed_stop_times += 1;
                            continue;
                        }
                    };

                    let crosses_midnight = arrival.rolls_over();
                    if crosses_midnight {
                        stats.crossing_midnight += 1;
                    }

                    events.push(ScheduledEvent {
                        event_id: event_id(
                            &self.agency.id,
                            service_date,
                            &trip.trip_id,
                            stop_time.stop_sequence,
                        ),
                        service_date,
                        agency_id: self.agency.id.clone(),
                        route_id: trip.route_id.clone(),
                        route_key: self.agency.route_key(&trip.route_id),
                        trip_id: trip.trip_id.clone(),
                        stop_id: stop_time.stop_id.clone(),
                        stop_key: self.agency.stop_key(&stop_time.stop_id),
                        stop_sequence: stop_time.stop_sequence,
                        // Optional in GTFS and blank on some LIRR trips. Left
                        // absent rather than defaulted to 0, which would assert
                        // a direction the feed did not state.
                        direction_id: trip.direction_id,
                        scheduled_arrival: arrival
                            .resolve(service_date, self.tz)?
                            .with_timezone(&Utc),
                        scheduled_departure: departure
                            .resolve(service_date, self.tz)?
                            .with_timezone(&Utc),
                        // Negative would mean a vehicle departs before it
                        // arrives. Clamped rather than rejected: it appears in
                        // real feeds as a data-entry slip and is not worth
                        // failing an entire day's ingest over.
                        dwell_seconds: (departure.seconds() - arrival.seconds()).max(0),
                        crosses_midnight,
                    });
                }
            }
        }

        stats.events = events.len();

        if stats.untimed_stop_times > 0 {
            warn!(
                agency = %self.agency.id,
                date = %service_date,
                dropped = stats.untimed_stop_times,
                "dropped stop_times with no arrival or departure"
            );
        }

        // Once per date rather than once per row: a feed that gets this wrong
        // usually gets it wrong for a whole export, and a per-row warning would
        // bury every other line of the run.
        if stats.malformed_times > 0 {
            warn!(
                agency = %self.agency.id,
                date = %service_date,
                dropped = stats.malformed_times,
                "dropped stop_times whose arrival or departure could not be parsed"
            );
        }

        Ok((events, stats))
    }
}

/// A stable identifier for one scheduled call.
///
/// Keyed on `stop_sequence` rather than `stop_id` because a trip may call at
/// the same stop twice — loop services and terminal turnarounds both do this,
/// and keying on the stop would collapse two distinct calls into one row.
///
/// Hashed with FNV-1a rather than [`std::collections::hash_map::DefaultHasher`],
/// whose output is explicitly not stable across Rust releases. These ids end up
/// in Parquet files that outlive the toolchain that wrote them, so an id that
/// changes when the compiler is upgraded would silently break every join
/// against previously written data.
pub fn event_id(
    agency_id: &str,
    service_date: NaiveDate,
    trip_id: &str,
    stop_sequence: u32,
) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    };

    // A separator between fields, so ("AB", "C") and ("A", "BC") differ.
    feed(agency_id.as_bytes());
    feed(b"\x1f");
    feed(service_date.to_string().as_bytes());
    feed(b"\x1f");
    feed(trip_id.as_bytes());
    feed(b"\x1f");
    feed(&stop_sequence.to_le_bytes());

    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::Timelike;

    use super::*;
    use crate::config::Config;
    use crate::gtfs::records::{AgencyRow, CalendarDateRow, RouteRow, StopRow};

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn agency() -> Agency {
        Config::load(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/agencies.toml"
        )))
        .unwrap()
        .agencies[0]
            .clone()
    }

    fn stop_time(trip: &str, stop: &str, seq: u32, arrival: &str, departure: &str) -> StopTimeRow {
        StopTimeRow {
            trip_id: trip.to_string(),
            stop_id: stop.to_string(),
            arrival_time: arrival.to_string(),
            departure_time: departure.to_string(),
            stop_sequence: seq,
        }
    }

    fn trip(trip_id: &str, route: &str, service: &str) -> TripRow {
        TripRow {
            trip_id: trip_id.to_string(),
            route_id: route.to_string(),
            service_id: service.to_string(),
            trip_headsign: None,
            direction_id: Some(0),
        }
    }

    /// A one-trip, two-stop feed running on a single date.
    fn feed() -> StaticFeed {
        StaticFeed {
            agency_id: "MTA_NYCT".to_string(),
            agencies: vec![AgencyRow {
                agency_id: Some("A".to_string()),
                agency_name: "Test".to_string(),
                agency_timezone: "America/New_York".to_string(),
            }],
            routes: vec![RouteRow {
                route_id: "1".to_string(),
                agency_id: Some("A".to_string()),
                route_short_name: Some("1".to_string()),
                route_long_name: None,
                route_type: 1,
                route_color: None,
            }],
            stops: vec![
                StopRow {
                    stop_id: "101N".to_string(),
                    stop_name: Some("First".to_string()),
                    stop_lat: Some(40.9),
                    stop_lon: Some(-73.9),
                    location_type: Some(0),
                    parent_station: Some("101".to_string()),
                },
                StopRow {
                    stop_id: "102N".to_string(),
                    stop_name: Some("Second".to_string()),
                    stop_lat: Some(40.8),
                    stop_lon: Some(-73.9),
                    location_type: Some(0),
                    parent_station: Some("102".to_string()),
                },
            ],
            trips: vec![trip("T1", "1", "SVC")],
            stop_times: vec![
                stop_time("T1", "101N", 1, "06:00:00", "06:00:30"),
                stop_time("T1", "102N", 2, "06:10:00", "06:10:00"),
            ],
            calendar: Vec::new(),
            calendar_dates: vec![CalendarDateRow {
                service_id: "SVC".to_string(),
                date: "20260401".to_string(),
                exception_type: 1,
            }],
        }
    }

    #[test]
    fn resolves_a_trip_into_one_event_per_call() {
        let feed = feed();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, stats) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(stats.events, 2);
        assert_eq!(stats.trips, 1);

        assert_eq!(events[0].stop_sequence, 1);
        assert_eq!(events[1].stop_sequence, 2);
    }

    #[test]
    fn keys_are_namespaced_by_agency() {
        // Route 1 and stop 101N exist in more than one MTA feed. The bare ids
        // are kept for display; the keys are what anything joins on.
        let feed = feed();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events[0].route_id, "1");
        assert_eq!(events[0].route_key, "MTA_NYCT:1");
        assert_eq!(events[0].stop_key, "MTA_NYCT:101N");
    }

    #[test]
    fn times_resolve_in_the_agency_timezone() {
        // 06:00 America/New_York on 2026-04-01 is 10:00 UTC (EDT, UTC-4).
        let feed = feed();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events[0].scheduled_arrival.hour(), 10);
        assert_eq!(
            events[0].scheduled_arrival.to_string(),
            "2026-04-01 10:00:00 UTC"
        );
    }

    #[test]
    fn dwell_is_departure_minus_arrival() {
        let feed = feed();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events[0].dwell_seconds, 30);
        assert_eq!(events[1].dwell_seconds, 0, "one time serves for both");
    }

    #[test]
    fn a_negative_dwell_is_clamped_rather_than_emitted() {
        // A vehicle cannot depart before it arrives. Feeds do publish this.
        let mut feed = feed();
        feed.stop_times[0].departure_time = "05:59:00".to_string();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events[0].dwell_seconds, 0);
    }

    #[test]
    fn a_call_past_midnight_keeps_its_service_date() {
        // The whole reason GTFS counts past 24: both calls belong to April 1's
        // service, and the second happens on April 2.
        let mut feed = feed();
        feed.stop_times = vec![
            stop_time("T1", "101N", 1, "23:50:00", "23:50:00"),
            stop_time("T1", "102N", 2, "24:20:00", "24:20:00"),
        ];
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, stats) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events[0].service_date, date(2026, 4, 1));
        assert_eq!(events[1].service_date, date(2026, 4, 1));

        assert!(!events[0].crosses_midnight);
        assert!(events[1].crosses_midnight);
        assert_eq!(stats.crossing_midnight, 1);

        // ...and the actual instants are 30 minutes apart, across the boundary.
        assert_eq!(
            (events[1].scheduled_arrival - events[0].scheduled_arrival).num_minutes(),
            30
        );
    }

    #[test]
    fn a_stop_with_only_a_departure_uses_it_for_both() {
        let mut feed = feed();
        feed.stop_times[0].arrival_time = String::new();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, stats) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(stats.untimed_stop_times, 0);
        assert_eq!(events[0].dwell_seconds, 0);
    }

    #[test]
    fn a_stop_with_no_times_is_dropped_and_counted() {
        // GTFS expects interpolation here. An interpolated schedule is a guess,
        // and a delay measured against a guess is not a measurement.
        let mut feed = feed();
        feed.stop_times[0].arrival_time = String::new();
        feed.stop_times[0].departure_time = String::new();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, stats) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(stats.untimed_stop_times, 1);
    }

    #[test]
    fn an_unparseable_time_is_dropped_and_counted_rather_than_fatal() {
        // This used to propagate, so one bad cell anywhere in a feed took down
        // a whole multi-day multi-agency build -- and the operator learned
        // about exactly one broken row per attempt. The untimed case one line
        // away was already counted and skipped; this now matches it.
        let mut feed = feed();
        feed.stop_times[0].arrival_time = "8:5".to_string();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, stats) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events.len(), 1, "the sound row still resolves");
        assert_eq!(stats.malformed_times, 1);
        assert_eq!(stats.untimed_stop_times, 0, "malformed is not untimed");
    }

    #[test]
    fn a_clean_feed_reports_no_malformed_times() {
        let feed = feed();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (_, stats) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(stats.malformed_times, 0);
    }

    #[test]
    fn calls_are_ordered_by_stop_sequence_not_file_order() {
        // GTFS does not require stop_times.txt to be sorted, and dwell and
        // headway both depend on the order of calls along the trip.
        let mut feed = feed();
        feed.stop_times.reverse();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events[0].stop_sequence, 1);
        assert_eq!(events[1].stop_sequence, 2);
    }

    #[test]
    fn a_date_with_no_service_yields_nothing() {
        let feed = feed();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, stats) = schedule.for_date(date(2026, 4, 2)).unwrap();
        assert!(events.is_empty());
        assert_eq!(stats, Stats::default());
    }

    #[test]
    fn the_same_trip_resolves_on_every_date_it_runs() {
        let mut feed = feed();
        feed.calendar_dates.push(CalendarDateRow {
            service_id: "SVC".to_string(),
            date: "20260402".to_string(),
            exception_type: 1,
        });
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        assert_eq!(
            schedule.service_dates(),
            [date(2026, 4, 1), date(2026, 4, 2)]
        );

        let (first, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        let (second, _) = schedule.for_date(date(2026, 4, 2)).unwrap();
        assert_eq!(first.len(), second.len());
        assert_ne!(
            first[0].event_id, second[0].event_id,
            "the same call on two dates is two events"
        );
    }

    #[test]
    fn a_service_with_no_trips_is_inert() {
        let mut feed = feed();
        feed.calendar_dates.push(CalendarDateRow {
            service_id: "ORPHAN".to_string(),
            date: "20260401".to_string(),
            exception_type: 1,
        });
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn a_trip_with_no_stop_times_emits_nothing() {
        let mut feed = feed();
        feed.trips.push(trip("T_EMPTY", "1", "SVC"));
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, stats) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(stats.trips, 1, "the empty trip is not counted as resolved");
    }

    #[test]
    fn resolution_is_deterministic() {
        // Byte-identical Parquet across runs is what makes a re-ingest a no-op
        // instead of a diff. Iterating a HashMap directly would break this.
        let feed = feed();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (first, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        let (second, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_malformed_time_does_not_take_the_rest_of_the_date_with_it() {
        // The inverse of the old contract, deliberately. Failing the date meant
        // one unparseable cell aborted a seven-day tri-agency build, so the
        // operator found out about broken rows one attempt at a time -- the
        // opposite of what `validate::check` does two modules away.
        let mut feed = feed();
        feed.stop_times[0].arrival_time = "not a time".to_string();
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, stats) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(stats.malformed_times, 1);
    }

    #[test]
    fn event_ids_distinguish_every_component() {
        let base = event_id("MTA_NYCT", date(2026, 4, 1), "T1", 1);

        for other in [
            event_id("MTA_LIRR", date(2026, 4, 1), "T1", 1),
            event_id("MTA_NYCT", date(2026, 4, 2), "T1", 1),
            event_id("MTA_NYCT", date(2026, 4, 1), "T2", 1),
            event_id("MTA_NYCT", date(2026, 4, 1), "T1", 2),
        ] {
            assert_ne!(base, other);
        }
        assert_eq!(base, event_id("MTA_NYCT", date(2026, 4, 1), "T1", 1));
    }

    #[test]
    fn event_ids_do_not_collide_across_field_boundaries() {
        // Without a separator, ("AB", "C") and ("A", "BC") hash identically and
        // two unrelated calls silently become one row.
        assert_ne!(
            event_id("AB", date(2026, 4, 1), "C", 1),
            event_id("A", date(2026, 4, 1), "BC", 1)
        );
    }

    #[test]
    fn a_trip_calling_at_one_stop_twice_gets_two_ids() {
        // Loop services and terminal turnarounds do this. Keying the id on
        // stop_id rather than stop_sequence would collapse them into one row.
        let mut feed = feed();
        feed.stop_times
            .push(stop_time("T1", "101N", 3, "06:20:00", "06:20:00"));
        let agency = agency();
        let schedule = Schedule::index(&feed, &agency).unwrap();

        let (events, _) = schedule.for_date(date(2026, 4, 1)).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].stop_key, events[2].stop_key);
        assert_ne!(events[0].event_id, events[2].event_id);
    }

    #[test]
    fn event_ids_are_stable_against_the_hash_implementation_changing() {
        // Pinned deliberately. These ids go into Parquet files that outlive the
        // toolchain; if this assertion ever fails, every previously written
        // dataset needs rewriting, and that should be a decision rather than a
        // surprise.
        assert_eq!(
            event_id("MTA_NYCT", date(2026, 4, 1), "T1", 1),
            "a932425890b2ec62"
        );
    }
}
