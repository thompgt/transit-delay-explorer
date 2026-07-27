//! Reading a GTFS static archive.
//!
//! A GTFS feed is a zip of CSVs. Two things make reading one less trivial than
//! that sounds. Producers sometimes nest the files inside a directory rather
//! than at the archive root, so lookup is by basename. And GTFS marks several
//! files optional in a way that is not academic: LIRR and Metro-North ship no
//! `calendar.txt` at all, so treating an absent optional file as an error would
//! reject two of our three agencies.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, info};

use crate::config::Agency;
use crate::error::{Error, FeedError, Result};
use crate::gtfs::records::{
    AgencyRow, CalendarDateRow, CalendarRow, RouteRow, StopRow, StopTimeRow, TripRow,
};

/// One agency's static feed, parsed but not yet validated or resolved.
#[derive(Debug)]
pub struct StaticFeed {
    /// The configured agency id. Always the id from `config/agencies.toml`,
    /// never the one inside the archive — a feed's own `agency_id` is not
    /// unique across agencies and cannot be trusted as a key.
    pub agency_id: String,
    pub agencies: Vec<AgencyRow>,
    pub routes: Vec<RouteRow>,
    pub stops: Vec<StopRow>,
    pub trips: Vec<TripRow>,
    pub stop_times: Vec<StopTimeRow>,
    /// Empty for feeds that express all service through `calendar_dates.txt`.
    pub calendar: Vec<CalendarRow>,
    pub calendar_dates: Vec<CalendarDateRow>,
}

impl StaticFeed {
    /// Read and parse the archive at `path` for `agency`.
    pub fn read(path: &Path, agency: &Agency) -> Result<Self> {
        let mut archive = Archive::open(path)?;

        let feed = Self {
            agency_id: agency.id.clone(),
            agencies: archive.read_required("agency.txt")?,
            routes: archive.read_required("routes.txt")?,
            stops: archive.read_required("stops.txt")?,
            trips: archive.read_required("trips.txt")?,
            stop_times: archive.read_required("stop_times.txt")?,
            calendar: archive.read_optional("calendar.txt")?,
            calendar_dates: archive.read_optional("calendar_dates.txt")?,
        };

        info!(
            agency = %feed.agency_id,
            routes = feed.routes.len(),
            stops = feed.stops.len(),
            trips = feed.trips.len(),
            stop_times = feed.stop_times.len(),
            calendar = feed.calendar.len(),
            calendar_dates = feed.calendar_dates.len(),
            "parsed static feed"
        );

        // Not a hard error: a feed can legally define service either way. But a
        // feed with neither has no service days at all, and every downstream
        // join would silently produce nothing.
        if feed.calendar.is_empty() && feed.calendar_dates.is_empty() {
            return Err(Error::Feed(FeedError::MissingFile {
                file: "calendar.txt or calendar_dates.txt".to_string(),
            }));
        }

        Ok(feed)
    }
}

/// A zip archive of CSVs, indexed by basename.
pub struct Archive {
    zip: zip::ZipArchive<BufReader<File>>,
    path: PathBuf,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let zip = zip::ZipArchive::new(BufReader::new(file)).map_err(|source| {
            Error::Feed(FeedError::BadArchive {
                path: path.to_path_buf(),
                source: Box::new(source),
            })
        })?;

        Ok(Self {
            zip,
            path: path.to_path_buf(),
        })
    }

    /// Index of `name` within the archive, matched on basename so a feed that
    /// nests its files in a directory still reads.
    fn index_of(&self, name: &str) -> Option<usize> {
        (0..self.zip.len()).find(|&i| {
            self.zip
                .name_for_index(i)
                .and_then(|n| n.rsplit(['/', '\\']).next())
                .is_some_and(|base| base.eq_ignore_ascii_case(name))
        })
    }

    /// Read a file GTFS requires. Absence is a malformed archive.
    pub fn read_required<T: for<'de> Deserialize<'de>>(&mut self, name: &str) -> Result<Vec<T>> {
        match self.index_of(name) {
            Some(index) => self.read_index(index, name),
            None => Err(Error::Feed(FeedError::MissingFile {
                file: name.to_string(),
            })),
        }
    }

    /// Read a file GTFS marks optional. Absence yields an empty vector.
    pub fn read_optional<T: for<'de> Deserialize<'de>>(&mut self, name: &str) -> Result<Vec<T>> {
        match self.index_of(name) {
            Some(index) => self.read_index(index, name),
            None => {
                debug!(archive = %self.path.display(), file = name, "optional file absent");
                Ok(Vec::new())
            }
        }
    }

    fn read_index<T: for<'de> Deserialize<'de>>(
        &mut self,
        index: usize,
        name: &str,
    ) -> Result<Vec<T>> {
        let mut entry = self.zip.by_index(index).map_err(|source| {
            Error::Feed(FeedError::BadArchive {
                path: self.path.clone(),
                source: Box::new(source),
            })
        })?;

        // Reading to memory rather than streaming: the largest MTA file is
        // Metro-North's stop_times.txt at ~466k rows, which is tens of MB. Not
        // worth the complexity of a streaming parser until a feed forces it.
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes).map_err(|source| Error::Io {
            path: self.path.join(name),
            source,
        })?;

        parse_csv(&bytes, name)
    }
}

/// Deserialize a GTFS CSV.
///
/// `flexible(true)` because producers emit short rows for trailing empty
/// columns; a row missing an optional trailing field is not worth rejecting an
/// entire feed over. Missing *required* fields still fail, in serde.
pub fn parse_csv<T: for<'de> Deserialize<'de>>(bytes: &[u8], name: &str) -> Result<Vec<T>> {
    // GTFS is specified as UTF-8, and producers routinely include a BOM anyway.
    // Left in place it becomes part of the first header name, so `stop_id`
    // stops matching and every row fails with a confusing error.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);

    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::Headers)
        .from_reader(bytes);

    let mut rows = Vec::new();
    for (record, row) in reader.deserialize().enumerate() {
        rows.push(row.map_err(|source| {
            Error::Feed(FeedError::Csv {
                file: name.to_string(),
                // Header is line 1, so the first data row is record 2 — which
                // is what a text editor will show.
                record: record as u64 + 2,
                source: Box::new(source),
            })
        })?);
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::gtfs::records::StopRow;

    fn write_zip(entries: &[(&str, &str)]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut writer = zip::ZipWriter::new(file.reopen().unwrap());
        for (name, body) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        file
    }

    const MINIMAL: &[(&str, &str)] = &[
        (
            "agency.txt",
            "agency_id,agency_name,agency_timezone\nA,Test,America/New_York\n",
        ),
        ("routes.txt", "route_id,route_type\nR1,2\n"),
        ("stops.txt", "stop_id,stop_name\nS1,First\n"),
        (
            "trips.txt",
            "trip_id,route_id,service_id\nT1,R1,SVC\n",
        ),
        (
            "stop_times.txt",
            "trip_id,stop_id,arrival_time,departure_time,stop_sequence\nT1,S1,06:00:00,06:00:00,1\n",
        ),
        ("calendar_dates.txt", "service_id,date,exception_type\nSVC,20260401,1\n"),
    ];

    fn agency() -> Agency {
        let config = Config::load(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/agencies.toml"
        )))
        .unwrap();
        config.agencies[0].clone()
    }

    use crate::config::Config;

    #[test]
    fn reads_a_minimal_feed() {
        let zip = write_zip(MINIMAL);
        let feed = StaticFeed::read(zip.path(), &agency()).unwrap();

        assert_eq!(feed.routes.len(), 1);
        assert_eq!(feed.stop_times.len(), 1);
        assert_eq!(feed.agency_id, "MTA_NYCT");
    }

    #[test]
    fn a_feed_without_calendar_txt_is_fine() {
        // LIRR and Metro-North both ship calendar_dates.txt only.
        let zip = write_zip(MINIMAL);
        let feed = StaticFeed::read(zip.path(), &agency()).unwrap();

        assert!(feed.calendar.is_empty());
        assert_eq!(feed.calendar_dates.len(), 1);
    }

    #[test]
    fn a_feed_with_no_service_definition_at_all_is_rejected() {
        let entries: Vec<_> = MINIMAL
            .iter()
            .filter(|(name, _)| *name != "calendar_dates.txt")
            .copied()
            .collect();
        let zip = write_zip(&entries);

        let err = StaticFeed::read(zip.path(), &agency()).unwrap_err();
        assert!(
            err.to_string().contains("calendar"),
            "expected a calendar complaint, got: {err}"
        );
    }

    #[test]
    fn a_missing_required_file_is_an_error() {
        let entries: Vec<_> = MINIMAL
            .iter()
            .filter(|(name, _)| *name != "stop_times.txt")
            .copied()
            .collect();
        let zip = write_zip(&entries);

        let err = StaticFeed::read(zip.path(), &agency()).unwrap_err();
        assert!(err.to_string().contains("stop_times.txt"), "got: {err}");
    }

    #[test]
    fn files_nested_in_a_directory_still_read() {
        let entries: Vec<(String, &str)> = MINIMAL
            .iter()
            .map(|(name, body)| (format!("gtfs/{name}"), *body))
            .collect();
        let borrowed: Vec<(&str, &str)> = entries.iter().map(|(n, b)| (n.as_str(), *b)).collect();
        let zip = write_zip(&borrowed);

        let feed = StaticFeed::read(zip.path(), &agency()).unwrap();
        assert_eq!(feed.stops.len(), 1);
    }

    #[test]
    fn a_byte_order_mark_does_not_break_the_first_column() {
        // With the BOM left in place the first header becomes "\u{feff}stop_id"
        // and every row fails on a missing stop_id -- a confusing way to learn
        // your feed has a BOM.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"stop_id,stop_name\nS1,First\n");

        let rows: Vec<StopRow> = parse_csv(&bytes, "stops.txt").unwrap();
        assert_eq!(rows[0].stop_id, "S1");
    }

    #[test]
    fn a_short_row_is_tolerated() {
        // Producers drop trailing empty columns. Rejecting the feed over an
        // absent optional field would be worse than reading it as absent.
        let rows: Vec<StopRow> =
            parse_csv(b"stop_id,stop_name,parent_station\nS1,First\n", "stops.txt").unwrap();
        assert_eq!(rows[0].parent_station, None);
    }

    #[test]
    fn a_parse_failure_reports_the_editor_line_number() {
        let err =
            parse_csv::<StopRow>(b"stop_id,stop_lat\nS1,40.7\nS2,not-a-number\n", "stops.txt")
                .unwrap_err();

        let message = format!("{err}");
        assert!(message.contains("stops.txt"), "got: {message}");
        assert!(message.contains("record 3"), "got: {message}");
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_reported_as_such() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"this is not a zip archive").unwrap();

        let err = match Archive::open(file.path()) {
            Err(err) => err,
            Ok(_) => panic!("a text file should not open as an archive"),
        };
        assert!(err.to_string().contains("zip"), "got: {err}");
    }
}
