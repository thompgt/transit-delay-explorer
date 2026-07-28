//! Error types.
//!
//! Referential integrity violations get one variant each rather than a single
//! stringly-typed "validation failed", because the whole point of validating a
//! GTFS archive is being able to say precisely which file broke and how.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Feed(#[from] FeedError),

    #[error("referential integrity check failed with {} violation(s)", .0.len())]
    Integrity(Vec<Violation>),

    #[error("i/o error at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse config at {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("config defines no agencies")]
    NoAgencies,

    #[error("duplicate agency id `{agency}`")]
    DuplicateAgencyId { agency: String },

    #[error("agency `{agency}` has unknown timezone `{timezone}`")]
    UnknownTimezone { agency: String, timezone: String },

    #[error("no agency configured with id `{agency}`")]
    UnknownAgency { agency: String },
}

#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    #[error("failed to download {url}")]
    Download {
        url: String,
        #[source]
        source: Box<reqwest::Error>,
    },

    #[error("{url} returned HTTP {status}")]
    HttpStatus { url: String, status: u16 },

    #[error("archive {path} is not a readable zip")]
    BadArchive {
        path: PathBuf,
        #[source]
        source: Box<zip::result::ZipError>,
    },

    /// GTFS marks some files optional, but a feed without `stop_times.txt` is
    /// not a feed. Required-file absence is distinct from a parse failure.
    #[error("archive is missing required file `{file}`")]
    MissingFile { file: String },

    #[error("failed to parse `{file}` at record {record}")]
    Csv {
        file: String,
        record: u64,
        #[source]
        source: Box<csv::Error>,
    },

    /// GTFS times are `HH:MM:SS` where HH may exceed 24 for trips that cross
    /// midnight. Anything else is malformed.
    #[error("`{value}` in {file} is not a valid GTFS time")]
    BadTime { file: String, value: String },

    #[error("`{value}` in {file} is not a valid GTFS date (expected YYYYMMDD)")]
    BadDate { file: String, value: String },

    /// GTFS defines exactly two exception types. A third means the producer
    /// intended something we cannot infer, and inferring would misstate service.
    #[error("unknown exception_type `{value}` for service `{service_id}` in calendar_dates.txt")]
    BadExceptionType { service_id: String, value: u8 },

    /// A service range long enough to be a garbled `end_date` rather than real
    /// service. Expanding one would produce millions of dates and look like a
    /// hang instead of a parse error.
    #[error("service `{service_id}` spans an implausible range {start}..={end}")]
    ImplausibleServiceRange {
        service_id: String,
        start: String,
        end: String,
    },

    /// A local time that does not exist, or exists twice, because of a DST
    /// transition. Real and unavoidable for overnight service.
    #[error("local time {local} is ambiguous or nonexistent in {timezone}")]
    AmbiguousLocalTime { local: String, timezone: String },
}

/// One referential integrity violation. Carries enough context to find the
/// offending row by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub file: String,
    pub field: String,
    pub value: String,
    pub references: String,
    /// How many rows carried this same dangling value. Feeds break in bulk;
    /// reporting 40,000 identical violations individually helps nobody.
    pub occurrences: usize,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{} = `{}` not found in {} ({} row(s))",
            self.file, self.field, self.value, self.references, self.occurrences
        )
    }
}
