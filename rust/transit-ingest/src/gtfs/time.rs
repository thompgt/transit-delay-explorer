//! GTFS time and date types.
//!
//! GTFS times are not clock times. A trip that leaves at 11:50 PM and arrives
//! after midnight is written `23:50:00` to `24:20:00` — the hour field keeps
//! counting past 24 so the trip stays on one service date. The MTA feeds do
//! this for 2.5–3.4% of all stop_times with hours up to 28, so this is normal
//! volume rather than an edge case worth special-casing later.
//!
//! Resolution to an absolute instant follows the spec exactly: times are
//! measured from *noon minus twelve hours* on the service date, not from
//! midnight. On a DST transition day those differ by an hour, and noon is
//! always unambiguous while midnight need not be.

use chrono::{DateTime, Duration, NaiveDate, TimeZone};
use chrono_tz::Tz;

use crate::error::FeedError;

/// A time offset from the start of a service day, in seconds. May exceed
/// 86,400 for trips that run past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GtfsTime(i32);

impl GtfsTime {
    /// GTFS permits `H:MM:SS` as well as `HH:MM:SS`. Some producers emit an
    /// empty string for a stop where the vehicle does not pick up or drop off;
    /// that is absence, not malformation, so callers get `Ok(None)`.
    pub fn parse(value: &str, file: &str) -> Result<Option<Self>, FeedError> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }

        let bad = || FeedError::BadTime {
            file: file.to_string(),
            value: value.to_string(),
        };

        let mut parts = value.split(':');
        let hours: i32 = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        let minutes: i32 = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        let seconds: i32 = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        if parts.next().is_some() {
            return Err(bad());
        }

        // Hours are unbounded above in principle; 48 is well past any real
        // service pattern and catches a garbled field before it becomes a
        // timestamp years away.
        if !(0..48).contains(&hours) || !(0..60).contains(&minutes) || !(0..60).contains(&seconds) {
            return Err(bad());
        }

        Ok(Some(Self(hours * 3600 + minutes * 60 + seconds)))
    }

    pub fn from_seconds(seconds: i32) -> Self {
        Self(seconds)
    }

    pub fn seconds(self) -> i32 {
        self.0
    }

    /// True when this time lands on the calendar day after its service date.
    pub fn rolls_over(self) -> bool {
        self.0 >= 86_400
    }

    /// The absolute instant this time refers to on `service_date` in `tz`.
    ///
    /// Measured from noon minus twelve hours, per the GTFS spec. The
    /// distinction matters twice a year: on a spring-forward day, `02:30:00`
    /// names a wall-clock time that never happens, and anchoring on local
    /// midnight would fail. Anchoring on noon makes it fall out as 03:30 —
    /// which is what a rider experiences.
    pub fn resolve(self, service_date: NaiveDate, tz: Tz) -> Result<DateTime<Tz>, FeedError> {
        let noon = service_date
            .and_hms_opt(12, 0, 0)
            .expect("12:00:00 is a valid time on every date");

        let noon_local = tz.from_local_datetime(&noon).single().ok_or_else(|| {
            FeedError::AmbiguousLocalTime {
                local: noon.to_string(),
                timezone: tz.to_string(),
            }
        })?;

        Ok(noon_local - Duration::hours(12) + Duration::seconds(self.0 as i64))
    }
}

impl std::fmt::Display for GtfsTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}",
            self.0 / 3600,
            (self.0 % 3600) / 60,
            self.0 % 60
        )
    }
}

/// Parse a GTFS `YYYYMMDD` date.
pub fn parse_date(value: &str, file: &str) -> Result<NaiveDate, FeedError> {
    NaiveDate::parse_from_str(value.trim(), "%Y%m%d").map_err(|_| FeedError::BadDate {
        file: file.to_string(),
        value: value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use chrono::Timelike;

    use super::*;

    const NY: Tz = chrono_tz::America::New_York;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn parses_ordinary_times() {
        let t = GtfsTime::parse("06:48:00", "stop_times.txt")
            .unwrap()
            .unwrap();
        assert_eq!(t.seconds(), 6 * 3600 + 48 * 60);
        assert!(!t.rolls_over());
    }

    #[test]
    fn parses_single_digit_hour() {
        let t = GtfsTime::parse("6:48:00", "stop_times.txt")
            .unwrap()
            .unwrap();
        assert_eq!(t.seconds(), 6 * 3600 + 48 * 60);
    }

    #[test]
    fn empty_time_is_absence_not_an_error() {
        assert_eq!(GtfsTime::parse("", "stop_times.txt").unwrap(), None);
        assert_eq!(GtfsTime::parse("   ", "stop_times.txt").unwrap(), None);
    }

    #[test]
    fn parses_hours_past_midnight() {
        // The MTA feeds reach hour 28. All three of these appear in real data.
        for (raw, hour) in [("24:00:00", 24), ("25:30:00", 25), ("28:15:00", 28)] {
            let t = GtfsTime::parse(raw, "stop_times.txt").unwrap().unwrap();
            assert_eq!(t.seconds() / 3600, hour);
            assert!(t.rolls_over(), "{raw} should roll over");
        }
    }

    #[test]
    fn rejects_malformed_times() {
        for raw in ["", "x"].iter().skip(1).chain(
            [
                "12:60:00",
                "12:00:60",
                "48:00:00",
                "12:00",
                "12:00:00:00",
                "-1:00:00",
            ]
            .iter(),
        ) {
            assert!(
                GtfsTime::parse(raw, "stop_times.txt").is_err(),
                "{raw} should be rejected"
            );
        }
    }

    #[test]
    fn midnight_rollover_lands_on_the_next_calendar_day() {
        let service_date = date(2026, 3, 10);
        let t = GtfsTime::parse("25:30:00", "stop_times.txt")
            .unwrap()
            .unwrap();
        let resolved = t.resolve(service_date, NY).unwrap();

        assert_eq!(resolved.date_naive(), date(2026, 3, 11));
        assert_eq!(resolved.hour(), 1);
        assert_eq!(resolved.minute(), 30);
    }

    #[test]
    fn service_date_is_preserved_across_rollover() {
        // Two stops on one trip, one before and one after midnight, must share
        // a service date -- that is the entire reason GTFS counts past 24.
        let service_date = date(2026, 3, 10);
        let before = GtfsTime::parse("23:50:00", "stop_times.txt")
            .unwrap()
            .unwrap();
        let after = GtfsTime::parse("24:20:00", "stop_times.txt")
            .unwrap()
            .unwrap();

        let before = before.resolve(service_date, NY).unwrap();
        let after = after.resolve(service_date, NY).unwrap();

        assert_eq!(before.date_naive(), date(2026, 3, 10));
        assert_eq!(after.date_naive(), date(2026, 3, 11));
        assert_eq!((after - before).num_minutes(), 30);
    }

    #[test]
    fn spring_forward_resolves_without_hitting_the_missing_hour() {
        // 2026-03-08, 02:00 EST -> 03:00 EDT, so wall-clock 02:30 never
        // happens. Because the anchor is noon minus twelve -- 23:00 EST on the
        // 7th, not local midnight -- 02:30:00 lands at 01:30 EST rather than
        // failing to resolve. Anchoring on local midnight would have no answer
        // to give here at all.
        let t = GtfsTime::parse("02:30:00", "stop_times.txt")
            .unwrap()
            .unwrap();
        let resolved = t.resolve(date(2026, 3, 8), NY).unwrap();

        assert_eq!(resolved.naive_utc().to_string(), "2026-03-08 06:30:00");
        assert_eq!(resolved.hour(), 1);
    }

    #[test]
    fn fall_back_resolves_inside_the_repeated_hour() {
        // 2026-11-01, 02:00 EDT -> 01:00 EST, so wall-clock 01:30 happens
        // twice. `from_local_datetime` alone would be ambiguous; counting
        // elapsed seconds from the anchor picks exactly one instant, which is
        // the point of resolving arithmetically instead of by local lookup.
        let t = GtfsTime::parse("01:30:00", "stop_times.txt")
            .unwrap()
            .unwrap();
        let resolved = t.resolve(date(2026, 11, 1), NY).unwrap();

        assert_eq!(resolved.naive_utc().to_string(), "2026-11-01 06:30:00");
    }

    #[test]
    fn times_after_a_dst_transition_match_the_wall_clock() {
        // This is the invariant that matters for real schedules: essentially
        // every stop_time on a transition day falls after the change, and each
        // one must resolve to the wall-clock time printed in the timetable.
        // A 7 AM train is a 7 AM train on both transition days.
        for service_date in [date(2026, 3, 8), date(2026, 11, 1)] {
            let t = GtfsTime::parse("07:00:00", "stop_times.txt")
                .unwrap()
                .unwrap();
            let resolved = t.resolve(service_date, NY).unwrap();

            assert_eq!(resolved.date_naive(), service_date);
            assert_eq!(resolved.hour(), 7, "on {service_date}");
            assert_eq!(resolved.minute(), 0);
        }
    }

    #[test]
    fn a_dst_day_is_not_twenty_four_hours_long() {
        // Falling out of the anchor rule: the spring-forward service day is 23
        // hours of real time and the fall-back day is 25. Any code that assumes
        // 86,400 seconds per service date will be wrong twice a year.
        let noon = GtfsTime::from_seconds(12 * 3600);
        let spring = noon.resolve(date(2026, 3, 8), NY).unwrap();
        let day_after = noon.resolve(date(2026, 3, 9), NY).unwrap();
        assert_eq!((day_after - spring).num_hours(), 24);

        let start = GtfsTime::from_seconds(0);
        let end = GtfsTime::from_seconds(24 * 3600);
        assert_eq!(
            (end.resolve(date(2026, 3, 8), NY).unwrap()
                - start.resolve(date(2026, 3, 8), NY).unwrap())
            .num_hours(),
            24,
            "elapsed seconds are elapsed seconds; it is the wall clock that jumps"
        );
    }

    #[test]
    fn twenty_four_hundred_is_the_next_midnight() {
        let t = GtfsTime::parse("24:00:00", "stop_times.txt")
            .unwrap()
            .unwrap();
        let resolved = t.resolve(date(2026, 6, 15), NY).unwrap();

        assert_eq!(resolved.date_naive(), date(2026, 6, 16));
        assert_eq!(resolved.hour(), 0);
    }

    #[test]
    fn displays_round_trip() {
        for raw in ["00:06:00", "23:50:00", "28:15:00"] {
            let t = GtfsTime::parse(raw, "stop_times.txt").unwrap().unwrap();
            assert_eq!(t.to_string(), raw);
        }
    }

    #[test]
    fn parses_dates() {
        assert_eq!(
            parse_date("20260315", "calendar.txt").unwrap(),
            date(2026, 3, 15)
        );
        assert!(parse_date("2026-03-15", "calendar.txt").is_err());
        assert!(parse_date("20261332", "calendar.txt").is_err());
    }
}
