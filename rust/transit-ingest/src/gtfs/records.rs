//! Row types for the GTFS static files we consume.
//!
//! Every field that is not load-bearing is `Option`, because the three MTA
//! feeds do not agree on which columns exist. LIRR's `routes.txt` has neither
//! `agency_id` nor `route_short_name`; only the Subway ships `parent_station`;
//! Metro-North adds `track` and `note_id` to `stop_times.txt`. Deserializing by
//! header name and ignoring unknown columns means a fourth agency with a fourth
//! column set is a config entry rather than a code change.
//!
//! The one thing that is *not* optional is the identifier a row is keyed by. A
//! `stop_times.txt` row with no `trip_id` is not a row with a missing field, it
//! is a corrupt archive, and it should fail loudly at parse time.

use serde::Deserialize;

/// `agency.txt`. GTFS lets `agency_id` be omitted when a feed has exactly one
/// agency; we fill it from config in that case rather than inventing one.
#[derive(Debug, Clone, Deserialize)]
pub struct AgencyRow {
    #[serde(default)]
    pub agency_id: Option<String>,
    pub agency_name: String,
    pub agency_timezone: String,
}

/// `routes.txt`.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteRow {
    pub route_id: String,
    #[serde(default)]
    pub agency_id: Option<String>,
    /// Absent for LIRR, where the long name is the only name.
    #[serde(default)]
    pub route_short_name: Option<String>,
    #[serde(default)]
    pub route_long_name: Option<String>,
    /// GTFS route type: 0 tram, 1 subway, 2 rail, 3 bus, ...
    pub route_type: i32,
    #[serde(default)]
    pub route_color: Option<String>,
}

impl RouteRow {
    /// What a rider would call this route. Subway riders say "the 7"; LIRR
    /// riders say "Port Washington Branch". Falling back to `route_id` keeps
    /// the label non-null for the cube, where a null breaks a hierarchy level.
    pub fn display_name(&self) -> &str {
        self.route_short_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.route_long_name.as_deref().filter(|s| !s.is_empty()))
            .unwrap_or(&self.route_id)
    }
}

/// `stops.txt`.
#[derive(Debug, Clone, Deserialize)]
pub struct StopRow {
    pub stop_id: String,
    #[serde(default)]
    pub stop_name: Option<String>,
    #[serde(default)]
    pub stop_lat: Option<f64>,
    #[serde(default)]
    pub stop_lon: Option<f64>,
    /// 0 or absent = platform/stop, 1 = station. Only the Subway feed uses it.
    #[serde(default)]
    pub location_type: Option<i32>,
    #[serde(default)]
    pub parent_station: Option<String>,
}

impl StopRow {
    pub fn is_station(&self) -> bool {
        self.location_type == Some(1)
    }
}

/// `trips.txt`.
#[derive(Debug, Clone, Deserialize)]
pub struct TripRow {
    pub trip_id: String,
    pub route_id: String,
    pub service_id: String,
    #[serde(default)]
    pub trip_headsign: Option<String>,
    #[serde(default)]
    pub direction_id: Option<i32>,
}

/// `stop_times.txt`. Times stay as strings here and are parsed in a later pass
/// so a malformed time can be reported with its file and row rather than as an
/// opaque serde failure.
#[derive(Debug, Clone, Deserialize)]
pub struct StopTimeRow {
    pub trip_id: String,
    pub stop_id: String,
    #[serde(default)]
    pub arrival_time: String,
    #[serde(default)]
    pub departure_time: String,
    pub stop_sequence: u32,
}

/// `calendar.txt`. Absent entirely from the LIRR and Metro-North feeds, which
/// express every service day through `calendar_dates.txt`.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarRow {
    pub service_id: String,
    pub monday: u8,
    pub tuesday: u8,
    pub wednesday: u8,
    pub thursday: u8,
    pub friday: u8,
    pub saturday: u8,
    pub sunday: u8,
    pub start_date: String,
    pub end_date: String,
}

impl CalendarRow {
    /// Weekday flags indexed the way [`chrono::Weekday::num_days_from_monday`]
    /// counts, so a lookup is an index rather than a match arm per day.
    pub fn runs_on_weekday(&self, days_from_monday: u32) -> bool {
        let flags = [
            self.monday,
            self.tuesday,
            self.wednesday,
            self.thursday,
            self.friday,
            self.saturday,
            self.sunday,
        ];
        flags.get(days_from_monday as usize) == Some(&1)
    }
}

/// `calendar_dates.txt`. `exception_type` 1 adds service, 2 removes it.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarDateRow {
    pub service_id: String,
    pub date: String,
    pub exception_type: u8,
}

impl CalendarDateRow {
    pub fn adds_service(&self) -> bool {
        self.exception_type == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: for<'de> Deserialize<'de>>(csv: &str) -> Vec<T> {
        csv::Reader::from_reader(csv.as_bytes())
            .deserialize()
            .collect::<Result<_, _>>()
            .expect("fixture should parse")
    }

    #[test]
    fn parses_subway_routes() {
        let rows: Vec<RouteRow> = parse(
            "route_id,agency_id,route_short_name,route_long_name,route_type,route_color\n\
             1,MTA NYCT,1,Broadway - 7 Avenue Local,1,EE352E\n",
        );
        assert_eq!(rows[0].display_name(), "1");
        assert_eq!(rows[0].route_type, 1);
    }

    #[test]
    fn parses_lirr_routes_without_agency_or_short_name() {
        // LIRR ships exactly these five columns. Requiring agency_id or
        // route_short_name here would reject the feed outright.
        let rows: Vec<RouteRow> = parse(
            "route_id,route_long_name,route_type,route_color,route_text_color\n\
             1,Babylon,2,B3DE96,000000\n",
        );
        assert_eq!(rows[0].agency_id, None);
        assert_eq!(rows[0].display_name(), "Babylon");
    }

    #[test]
    fn falls_back_to_route_id_when_a_feed_names_nothing() {
        let rows: Vec<RouteRow> = parse("route_id,route_type\nX9,2\n");
        assert_eq!(rows[0].display_name(), "X9");
    }

    #[test]
    fn ignores_columns_we_do_not_model() {
        // Metro-North's stop_times.txt carries track and note_id.
        let rows: Vec<StopTimeRow> = parse(
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type,track,note_id\n\
             T1,06:48:00,06:48:00,131,1,0,0,3,\n",
        );
        assert_eq!(rows[0].trip_id, "T1");
        assert_eq!(rows[0].stop_sequence, 1);
    }

    #[test]
    fn column_order_does_not_matter() {
        // Subway puts stop_id second, LIRR puts it fourth. Deserializing by
        // header name is what makes one struct serve both.
        let rows: Vec<StopTimeRow> = parse(
            "trip_id,stop_id,arrival_time,departure_time,stop_sequence\n\
             T1,101S,00:06:00,00:06:00,1\n",
        );
        assert_eq!(rows[0].stop_id, "101S");
    }

    #[test]
    fn quoted_fields_parse() {
        // The LIRR feed quotes every field, including numerics.
        let rows: Vec<StopTimeRow> = parse(
            "\"trip_id\",\"arrival_time\",\"departure_time\",\"stop_id\",\"stop_sequence\"\n\
             \"GO201_26_2\",\"01:05:00\",\"01:05:00\",\"102\",\"1\"\n",
        );
        assert_eq!(rows[0].trip_id, "GO201_26_2");
        assert_eq!(rows[0].stop_sequence, 1);
    }

    #[test]
    fn missing_stop_coordinates_are_absent_not_zero() {
        // (0, 0) is in the Atlantic. A stop with no coordinates must stay
        // unknown, or it lands in the ocean and skews any geographic rollup.
        let rows: Vec<StopRow> = parse("stop_id,stop_name,stop_lat,stop_lon\nS1,Nowhere,,\n");
        assert_eq!(rows[0].stop_lat, None);
        assert_eq!(rows[0].stop_lon, None);
    }

    #[test]
    fn detects_stations_versus_platforms() {
        let rows: Vec<StopRow> = parse(
            "stop_id,stop_name,location_type,parent_station\n\
             101,Van Cortlandt Park,1,\n\
             101N,Van Cortlandt Park,0,101\n",
        );
        assert!(rows[0].is_station());
        assert!(!rows[1].is_station());
        assert_eq!(rows[1].parent_station.as_deref(), Some("101"));
    }

    #[test]
    fn missing_required_identifier_is_an_error() {
        let result: Result<Vec<TripRow>, _> =
            csv::Reader::from_reader("route_id,service_id\n1,S1\n".as_bytes())
                .deserialize()
                .collect();
        assert!(result.is_err(), "trips.txt without trip_id must not parse");
    }

    #[test]
    fn calendar_weekday_flags() {
        let rows: Vec<CalendarRow> = parse(
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
             WK,1,1,1,1,1,0,0,20260101,20261231\n",
        );
        assert!(rows[0].runs_on_weekday(0)); // Monday
        assert!(rows[0].runs_on_weekday(4)); // Friday
        assert!(!rows[0].runs_on_weekday(5)); // Saturday
        assert!(!rows[0].runs_on_weekday(9)); // out of range, not a panic
    }

    #[test]
    fn calendar_date_exception_types() {
        let rows: Vec<CalendarDateRow> =
            parse("service_id,date,exception_type\nA,20260704,1\nB,20261225,2\n");
        assert!(rows[0].adds_service());
        assert!(!rows[1].adds_service());
    }
}
