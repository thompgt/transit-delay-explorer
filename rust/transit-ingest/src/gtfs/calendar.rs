//! Expanding GTFS service definitions into concrete service dates.
//!
//! A trip does not carry the days it runs. It carries a `service_id`, and the
//! days live in two files that a feed may use in either combination:
//!
//! - `calendar.txt` — weekday flags plus an inclusive date range
//! - `calendar_dates.txt` — per-date exceptions, adding (type 1) or removing
//!   (type 2) a single date
//!
//! All three combinations are legal and all three are in use here. The Subway
//! ships both files, using `calendar_dates.txt` to cut holiday service out of a
//! weekly pattern. LIRR and Metro-North ship no `calendar.txt` at all and
//! enumerate every operating day as an addition. Code that reads only
//! `calendar.txt` finds no service for two of our three agencies; code that
//! reads only `calendar_dates.txt` runs Subway trains on Thanksgiving.
//!
//! Order matters within a date: additions apply first, removals last. A service
//! that is both added and removed on one date does not run, because a removal
//! is the more specific statement and that is how producers use it.

use std::collections::{BTreeSet, HashMap};

use chrono::{Datelike, NaiveDate};
use tracing::warn;

use crate::error::{FeedError, Result};
use crate::gtfs::archive::StaticFeed;
use crate::gtfs::time::parse_date;

/// An implausible `end_date` is a garbled field, not a century of service.
/// Without a bound, one bad row expands into tens of millions of dates and the
/// failure looks like a hang rather than a parse error.
const MAX_SERVICE_SPAN_DAYS: i64 = 5 * 366;

/// Every service id in a feed, expanded to the dates it actually runs.
#[derive(Debug, Default)]
pub struct ServiceCalendar {
    dates: HashMap<String, BTreeSet<NaiveDate>>,
}

impl ServiceCalendar {
    /// Expand `feed`'s calendar files.
    ///
    /// A service that resolves to no dates at all is kept as an empty entry
    /// rather than dropped: "this service exists and runs nowhere" is a real
    /// state of a feed near the end of its validity window, and distinguishing
    /// it from "no such service" is what makes the warning below possible.
    pub fn build(feed: &StaticFeed) -> Result<Self> {
        let mut dates: HashMap<String, BTreeSet<NaiveDate>> = HashMap::new();

        for row in &feed.calendar {
            let start = parse_date(&row.start_date, "calendar.txt")?;
            let end = parse_date(&row.end_date, "calendar.txt")?;

            let entry = dates.entry(row.service_id.clone()).or_default();

            // An inverted range is not an error in the format, and producers do
            // emit one when a service is being retired. It means no days.
            if end < start {
                warn!(
                    service = %row.service_id,
                    start = %start,
                    end = %end,
                    "calendar.txt range ends before it starts; service has no days"
                );
                continue;
            }

            if (end - start).num_days() > MAX_SERVICE_SPAN_DAYS {
                return Err(FeedError::ImplausibleServiceRange {
                    service_id: row.service_id.clone(),
                    start: start.to_string(),
                    end: end.to_string(),
                }
                .into());
            }

            for date in start.iter_days().take_while(|d| *d <= end) {
                if row.runs_on_weekday(date.weekday().num_days_from_monday()) {
                    entry.insert(date);
                }
            }
        }

        // Two passes so an addition and a removal on the same date resolve to
        // "does not run" regardless of the order the rows appear in the file.
        for row in &feed.calendar_dates {
            if row.adds_service() {
                let date = parse_date(&row.date, "calendar_dates.txt")?;
                dates
                    .entry(row.service_id.clone())
                    .or_default()
                    .insert(date);
            }
        }

        for row in &feed.calendar_dates {
            match row.exception_type {
                1 => {}
                2 => {
                    let date = parse_date(&row.date, "calendar_dates.txt")?;
                    // An entry may not exist: removing a date from a service
                    // defined nowhere else is meaningless but harmless, and
                    // rejecting the feed over it would be disproportionate.
                    if let Some(entry) = dates.get_mut(&row.service_id) {
                        entry.remove(&date);
                    }
                }
                other => {
                    return Err(FeedError::BadExceptionType {
                        service_id: row.service_id.clone(),
                        value: other,
                    }
                    .into())
                }
            }
        }

        let calendar = Self { dates };

        let empty = calendar.dates.values().filter(|d| d.is_empty()).count();
        if empty > 0 {
            warn!(
                count = empty,
                total = calendar.dates.len(),
                "service ids with no operating days"
            );
        }

        Ok(calendar)
    }

    /// Dates `service_id` runs on, ascending. Empty for an unknown service.
    pub fn dates_for(&self, service_id: &str) -> &BTreeSet<NaiveDate> {
        static NONE: std::sync::OnceLock<BTreeSet<NaiveDate>> = std::sync::OnceLock::new();
        self.dates
            .get(service_id)
            .unwrap_or_else(|| NONE.get_or_init(BTreeSet::new))
    }

    pub fn runs_on(&self, service_id: &str, date: NaiveDate) -> bool {
        self.dates
            .get(service_id)
            .is_some_and(|dates| dates.contains(&date))
    }

    /// Every service running on `date`, sorted for deterministic output.
    pub fn services_on(&self, date: NaiveDate) -> Vec<&str> {
        let mut services: Vec<&str> = self
            .dates
            .iter()
            .filter(|(_, dates)| dates.contains(&date))
            .map(|(id, _)| id.as_str())
            .collect();
        services.sort_unstable();
        services
    }

    /// First and last date any service runs. `None` when nothing runs at all.
    ///
    /// This is the feed's real coverage, which is not the same as the range
    /// printed in `feed_info.txt` — that field is optional, frequently stale,
    /// and not what the schedule actually contains.
    pub fn coverage(&self) -> Option<(NaiveDate, NaiveDate)> {
        let first = self.dates.values().filter_map(|d| d.first()).min()?;
        let last = self.dates.values().filter_map(|d| d.last()).max()?;
        Some((*first, *last))
    }

    pub fn service_count(&self) -> usize {
        self.dates.len()
    }

    /// Distinct dates on which anything runs.
    pub fn active_dates(&self) -> BTreeSet<NaiveDate> {
        self.dates.values().flatten().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtfs::records::{CalendarDateRow, CalendarRow};

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// A feed carrying only calendar rows; the rest is irrelevant to expansion.
    fn feed(calendar: Vec<CalendarRow>, calendar_dates: Vec<CalendarDateRow>) -> StaticFeed {
        StaticFeed {
            agency_id: "TEST".to_string(),
            agencies: Vec::new(),
            routes: Vec::new(),
            stops: Vec::new(),
            trips: Vec::new(),
            stop_times: Vec::new(),
            calendar,
            calendar_dates,
        }
    }

    fn weekly(service_id: &str, flags: [u8; 7], start: &str, end: &str) -> CalendarRow {
        CalendarRow {
            service_id: service_id.to_string(),
            monday: flags[0],
            tuesday: flags[1],
            wednesday: flags[2],
            thursday: flags[3],
            friday: flags[4],
            saturday: flags[5],
            sunday: flags[6],
            start_date: start.to_string(),
            end_date: end.to_string(),
        }
    }

    fn exception(service_id: &str, date: &str, exception_type: u8) -> CalendarDateRow {
        CalendarDateRow {
            service_id: service_id.to_string(),
            date: date.to_string(),
            exception_type,
        }
    }

    const WEEKDAYS: [u8; 7] = [1, 1, 1, 1, 1, 0, 0];
    const WEEKENDS: [u8; 7] = [0, 0, 0, 0, 0, 1, 1];

    #[test]
    fn expands_weekday_flags_across_a_range() {
        // 2026-03-02 is a Monday; the range covers exactly two weeks.
        let calendar = ServiceCalendar::build(&feed(
            vec![weekly("WK", WEEKDAYS, "20260302", "20260315")],
            Vec::new(),
        ))
        .unwrap();

        let dates = calendar.dates_for("WK");
        assert_eq!(dates.len(), 10, "two weeks of weekdays");
        assert!(dates.contains(&date(2026, 3, 2)), "Monday runs");
        assert!(!dates.contains(&date(2026, 3, 7)), "Saturday does not");
    }

    #[test]
    fn the_range_is_inclusive_at_both_ends() {
        // Off-by-one here silently drops the first and last day of service.
        let calendar = ServiceCalendar::build(&feed(
            vec![weekly("WK", WEEKDAYS, "20260302", "20260306")],
            Vec::new(),
        ))
        .unwrap();

        let dates = calendar.dates_for("WK");
        assert_eq!(dates.first(), Some(&date(2026, 3, 2)));
        assert_eq!(dates.last(), Some(&date(2026, 3, 6)));
        assert_eq!(dates.len(), 5);
    }

    #[test]
    fn a_single_day_range_yields_one_day() {
        let calendar = ServiceCalendar::build(&feed(
            vec![weekly("WK", WEEKDAYS, "20260302", "20260302")],
            Vec::new(),
        ))
        .unwrap();

        assert_eq!(calendar.dates_for("WK").len(), 1);
    }

    #[test]
    fn calendar_dates_only_feeds_expand() {
        // LIRR and Metro-North ship no calendar.txt whatsoever. Every operating
        // day arrives as an exception_type 1 row.
        let calendar = ServiceCalendar::build(&feed(
            Vec::new(),
            vec![
                exception("GO501", "20260302", 1),
                exception("GO501", "20260303", 1),
                exception("GO502", "20260307", 1),
            ],
        ))
        .unwrap();

        assert_eq!(calendar.dates_for("GO501").len(), 2);
        assert_eq!(calendar.dates_for("GO502").len(), 1);
        assert_eq!(calendar.service_count(), 2);
    }

    #[test]
    fn an_exception_removes_a_day_from_a_weekly_pattern() {
        // How the Subway feed expresses a holiday: a weekday service with the
        // holiday cut out of it.
        let calendar = ServiceCalendar::build(&feed(
            vec![weekly("WK", WEEKDAYS, "20260302", "20260306")],
            vec![exception("WK", "20260304", 2)],
        ))
        .unwrap();

        let dates = calendar.dates_for("WK");
        assert_eq!(dates.len(), 4);
        assert!(!dates.contains(&date(2026, 3, 4)));
    }

    #[test]
    fn an_exception_adds_a_day_outside_the_range() {
        // Additions are not clipped to the calendar.txt range -- that is how a
        // feed extends service by one day without reissuing the range.
        let calendar = ServiceCalendar::build(&feed(
            vec![weekly("WK", WEEKDAYS, "20260302", "20260306")],
            vec![exception("WK", "20260307", 1)],
        ))
        .unwrap();

        assert!(calendar.runs_on("WK", date(2026, 3, 7)), "Saturday added");
        assert_eq!(calendar.dates_for("WK").len(), 6);
    }

    #[test]
    fn removal_wins_over_addition_on_the_same_date() {
        // Contradictory rows do occur. Resolving in file order would make the
        // answer depend on which row happened to come last; a removal is the
        // more specific statement, so it wins either way.
        for rows in [
            vec![exception("S", "20260704", 1), exception("S", "20260704", 2)],
            vec![exception("S", "20260704", 2), exception("S", "20260704", 1)],
        ] {
            let calendar = ServiceCalendar::build(&feed(Vec::new(), rows)).unwrap();
            assert!(!calendar.runs_on("S", date(2026, 7, 4)));
        }
    }

    #[test]
    fn removing_a_date_from_an_unknown_service_is_tolerated() {
        let calendar =
            ServiceCalendar::build(&feed(Vec::new(), vec![exception("GHOST", "20260704", 2)]))
                .unwrap();

        assert!(!calendar.runs_on("GHOST", date(2026, 7, 4)));
    }

    #[test]
    fn an_unknown_exception_type_is_rejected() {
        // Only 1 and 2 are defined. A 3 means the producer meant something we
        // cannot guess, and guessing would silently misstate service.
        let err = ServiceCalendar::build(&feed(Vec::new(), vec![exception("S", "20260704", 3)]))
            .unwrap_err();
        assert!(err.to_string().contains("exception_type"), "got: {err}");
    }

    #[test]
    fn an_inverted_range_is_empty_rather_than_an_error() {
        let calendar = ServiceCalendar::build(&feed(
            vec![weekly("WK", WEEKDAYS, "20260306", "20260302")],
            Vec::new(),
        ))
        .unwrap();

        assert!(calendar.dates_for("WK").is_empty());
        assert_eq!(calendar.service_count(), 1, "the service still exists");
    }

    #[test]
    fn an_implausible_range_is_rejected_rather_than_expanded() {
        // A garbled end_date would otherwise expand to millions of dates and
        // present as a hang instead of a parse error.
        let err = ServiceCalendar::build(&feed(
            vec![weekly("WK", WEEKDAYS, "20260302", "21260302")],
            Vec::new(),
        ))
        .unwrap_err();
        assert!(err.to_string().contains("WK"), "got: {err}");
    }

    #[test]
    fn a_malformed_date_names_its_file() {
        let err = ServiceCalendar::build(&feed(Vec::new(), vec![exception("S", "2026-07-04", 1)]))
            .unwrap_err();
        assert!(err.to_string().contains("calendar_dates.txt"), "got: {err}");
    }

    #[test]
    fn unknown_services_have_no_dates_rather_than_panicking() {
        let calendar = ServiceCalendar::default();
        assert!(calendar.dates_for("NOPE").is_empty());
        assert!(!calendar.runs_on("NOPE", date(2026, 3, 2)));
        assert_eq!(calendar.coverage(), None);
    }

    #[test]
    fn coverage_spans_every_service() {
        let calendar = ServiceCalendar::build(&feed(
            vec![
                weekly("WK", WEEKDAYS, "20260302", "20260306"),
                weekly("WE", WEEKENDS, "20260307", "20260315"),
            ],
            Vec::new(),
        ))
        .unwrap();

        assert_eq!(
            calendar.coverage(),
            Some((date(2026, 3, 2), date(2026, 3, 15)))
        );
    }

    #[test]
    fn coverage_ignores_a_service_that_runs_nowhere() {
        // An empty service must not drag coverage to None or to a bogus bound.
        let calendar = ServiceCalendar::build(&feed(
            vec![
                weekly("WK", WEEKDAYS, "20260302", "20260306"),
                weekly("DEAD", WEEKENDS, "20260306", "20260302"),
            ],
            Vec::new(),
        ))
        .unwrap();

        assert_eq!(
            calendar.coverage(),
            Some((date(2026, 3, 2), date(2026, 3, 6)))
        );
    }

    #[test]
    fn services_on_a_date_are_sorted_and_complete() {
        let calendar = ServiceCalendar::build(&feed(
            vec![
                weekly("WK", WEEKDAYS, "20260302", "20260306"),
                weekly("WE", WEEKENDS, "20260302", "20260308"),
            ],
            vec![exception("EXTRA", "20260302", 1)],
        ))
        .unwrap();

        assert_eq!(calendar.services_on(date(2026, 3, 2)), ["EXTRA", "WK"]);
        assert_eq!(calendar.services_on(date(2026, 3, 7)), ["WE"]);
        assert!(calendar.services_on(date(2026, 4, 1)).is_empty());
    }

    #[test]
    fn active_dates_are_the_union_across_services() {
        let calendar = ServiceCalendar::build(&feed(
            vec![
                weekly("WK", WEEKDAYS, "20260302", "20260306"),
                weekly("WE", WEEKENDS, "20260307", "20260308"),
            ],
            Vec::new(),
        ))
        .unwrap();

        // A full contiguous week, assembled from two disjoint services.
        let active = calendar.active_dates();
        assert_eq!(active.len(), 7);
        assert_eq!(active.first(), Some(&date(2026, 3, 2)));
        assert_eq!(active.last(), Some(&date(2026, 3, 8)));
    }

    #[test]
    fn a_service_defined_in_both_files_unions_them() {
        // Legal and used: a weekly pattern plus extra dates outside the range.
        let calendar = ServiceCalendar::build(&feed(
            vec![weekly("WK", WEEKDAYS, "20260302", "20260306")],
            vec![
                exception("WK", "20260307", 1),
                exception("WK", "20260302", 2),
            ],
        ))
        .unwrap();

        let dates = calendar.dates_for("WK");
        assert!(!dates.contains(&date(2026, 3, 2)), "removed");
        assert!(dates.contains(&date(2026, 3, 7)), "added");
        assert_eq!(dates.len(), 5);
    }
}
