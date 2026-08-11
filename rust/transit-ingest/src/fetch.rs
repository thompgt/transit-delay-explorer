//! Downloading static GTFS archives.
//!
//! The whole job is four lines of `reqwest` plus the care that keeps a bad
//! download from being indistinguishable from a good one.
//!
//! An archive is written to a `.part` file and renamed into place only after it
//! parses as a zip containing the files GTFS requires. Two failure modes make
//! that worth doing rather than streaming straight to the destination. A
//! download interrupted halfway leaves a truncated zip that `inspect` will
//! happily open and report as a feed missing most of its files — a confusing
//! way to learn the network dropped. And the MTA serves an HTML error page with
//! a 200 status when a feed is briefly unavailable, which lands on disk as
//! `MTA_NYCT.zip` and fails much later with a parse error naming the wrong
//! cause. Validating before the rename means the file at the destination path
//! is always a feed that at least opens.
//!
//! Content-Type is deliberately not checked. The MTA returns three different
//! ones for the same payload — see `docs/FEED_NOTES.md` — so it is evidence of
//! nothing.
//!
//! Freshness is decided by the server, not by us. Whatever validators came back
//! with the last successful download are written beside the archive and sent as
//! `If-None-Match` / `If-Modified-Since` on the next attempt, so a `--force`
//! against an unchanged feed costs one round trip and a 304 rather than tens of
//! megabytes. The absence of a validator simply means an unconditional request:
//! this is an optimisation, and it must never be the reason a real update is
//! missed.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::config::Agency;
use crate::error::{Error, FeedError, Result};

/// Files without which an archive is not a GTFS feed, whatever it parsed as.
const REQUIRED: [&str; 5] = [
    "agency.txt",
    "routes.txt",
    "stops.txt",
    "trips.txt",
    "stop_times.txt",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub agency_id: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// True when an existing archive was kept rather than re-downloaded.
    pub reused: bool,
}

/// The cache validators the server gave us with the archive currently on disk.
///
/// Stored beside it as `<agency>.zip.cache.json`. A sidecar rather than an
/// extended attribute or a database: it survives a copy of the data directory
/// on every platform, and losing it degrades to an unconditional request, which
/// is exactly the old behaviour.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheValidators {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl CacheValidators {
    fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }

    fn path_for(archive: &Path) -> PathBuf {
        let mut name = archive.as_os_str().to_os_string();
        name.push(".cache.json");
        PathBuf::from(name)
    }

    /// Read the sidecar for `archive`. A missing or unreadable one is not an
    /// error: it means "ask unconditionally", which is always safe.
    fn read(archive: &Path) -> Self {
        let path = Self::path_for(archive);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|error| {
            warn!(path = %path.display(), %error, "unreadable cache sidecar; refetching unconditionally");
            Self::default()
        })
    }

    /// Write the sidecar. A failure here is logged, not propagated: the archive
    /// is already safely in place and the only cost is a full download next
    /// time.
    fn write(&self, archive: &Path) {
        let path = Self::path_for(archive);
        if self.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(error) = std::fs::write(&path, text) {
                    warn!(path = %path.display(), %error, "could not record cache validators");
                }
            }
            Err(error) => warn!(%error, "could not serialize cache validators"),
        }
    }
}

/// What a conditional request came back with.
enum Response {
    /// The server confirmed what we already have.
    NotModified,
    /// A new body, plus whatever validators to send next time.
    Body {
        bytes: Vec<u8>,
        validators: CacheValidators,
    },
}

/// Download `agency`'s static archive into `<data_dir>/raw/<agency>.zip`.
///
/// An existing archive is kept without a request at all unless `force` is set:
/// the static feeds change a few times a year, and re-downloading tens of
/// megabytes on every run to get the same bytes is rude to a free service.
///
/// `force` no longer means "download it again regardless". It means "ask the
/// server whether it changed": the request carries the validators from the last
/// download, and a 304 is reported as reused. That is what makes `--force` a
/// reasonable thing to run when you are not sure, rather than a guaranteed
/// several hundred megabytes across three agencies.
pub fn static_archive(
    agency: &Agency,
    data_dir: &Path,
    timeout: Duration,
    force: bool,
) -> Result<Fetched> {
    let dest = agency.archive_path(data_dir);

    let on_disk = |bytes: u64| Fetched {
        agency_id: agency.id.clone(),
        path: dest.clone(),
        bytes,
        reused: true,
    };

    let existing_bytes = std::fs::metadata(&dest).ok().map(|meta| meta.len());

    if let Some(bytes) = existing_bytes {
        if !force {
            info!(agency = %agency.id, path = %dest.display(), "archive present; skipping download");
            return Ok(on_disk(bytes));
        }
    }

    // Only conditional when there is something on disk for a 304 to refer to.
    let validators = match existing_bytes {
        Some(_) => CacheValidators::read(&dest),
        None => CacheValidators::default(),
    };

    match download(&agency.static_url, timeout, &validators)? {
        Response::NotModified => {
            let bytes = existing_bytes.expect("a 304 is only possible with validators from disk");
            info!(
                agency = %agency.id,
                url = %agency.static_url,
                path = %dest.display(),
                "server reports the archive unchanged; keeping the one on disk"
            );
            Ok(on_disk(bytes))
        }
        Response::Body { bytes, validators } => {
            let written = install(&bytes, &dest)?;
            // Written only after the archive is validated and in place, so a
            // rejected HTML error page cannot leave validators claiming it is
            // what we hold.
            validators.write(&dest);

            info!(
                agency = %agency.id,
                url = %agency.static_url,
                bytes = written,
                path = %dest.display(),
                "downloaded static archive"
            );

            Ok(Fetched {
                agency_id: agency.id.clone(),
                path: dest,
                bytes: written,
                reused: false,
            })
        }
    }
}

fn download(url: &str, timeout: Duration, validators: &CacheValidators) -> Result<Response> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|source| FeedError::Download {
            url: url.to_string(),
            source: Box::new(source),
        })?;

    let mut request = client.get(url);
    if let Some(etag) = &validators.etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    if let Some(last_modified) = &validators.last_modified {
        request = request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
    }

    let response = request.send().map_err(|source| FeedError::Download {
        url: url.to_string(),
        source: Box::new(source),
    })?;

    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Response::NotModified);
    }

    // Checked explicitly rather than via error_for_status, so the message names
    // the code and the URL instead of reading as a generic request failure.
    if !response.status().is_success() {
        return Err(FeedError::HttpStatus {
            url: url.to_string(),
            status: response.status().as_u16(),
        }
        .into());
    }

    let validators = validators_from(response.headers());
    debug!(url, ?validators, "server validators for the next request");

    let bytes = response.bytes().map_err(|source| FeedError::Download {
        url: url.to_string(),
        source: Box::new(source),
    })?;

    Ok(Response::Body {
        bytes: bytes.to_vec(),
        validators,
    })
}

/// Pull `ETag` and `Last-Modified` off a response, ignoring anything that is
/// not valid ASCII — a malformed validator is worth less than no validator,
/// since echoing one back can only produce a wrong 304.
fn validators_from(headers: &reqwest::header::HeaderMap) -> CacheValidators {
    let header = |name: reqwest::header::HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };

    CacheValidators {
        etag: header(reqwest::header::ETAG),
        last_modified: header(reqwest::header::LAST_MODIFIED),
    }
}

/// Validate `body` as a GTFS archive and move it into place atomically.
///
/// Separate from [`download`] so the part that can go wrong on disk is testable
/// without a network.
pub fn install(body: &[u8], dest: &Path) -> Result<u64> {
    validate_archive(body, dest)?;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // Written beside the destination, not in a temp directory: rename is only
    // atomic within a filesystem, and a temp dir may well be on another one.
    let partial = dest.with_extension("zip.part");
    std::fs::write(&partial, body).map_err(|source| Error::Io {
        path: partial.clone(),
        source,
    })?;

    // Windows will not rename onto an existing file.
    if dest.exists() {
        std::fs::remove_file(dest).map_err(|source| Error::Io {
            path: dest.to_path_buf(),
            source,
        })?;
    }

    std::fs::rename(&partial, dest).map_err(|source| Error::Io {
        path: dest.to_path_buf(),
        source,
    })?;

    Ok(body.len() as u64)
}

/// Reject anything that is not a zip holding the files GTFS requires.
fn validate_archive(body: &[u8], dest: &Path) -> Result<()> {
    let archive =
        zip::ZipArchive::new(Cursor::new(body)).map_err(|source| FeedError::BadArchive {
            path: dest.to_path_buf(),
            source: Box::new(source),
        })?;

    // Matched on basename, so an archive that nests its CSVs in a directory
    // still validates — the same rule the reader uses.
    let names: Vec<String> = archive
        .file_names()
        .filter_map(|name| name.rsplit(['/', '\\']).next())
        .map(str::to_ascii_lowercase)
        .collect();

    for required in REQUIRED {
        if !names.iter().any(|name| name == required) {
            warn!(
                file = required,
                "downloaded archive is missing a required file"
            );
            return Err(FeedError::MissingFile {
                file: required.to_string(),
            }
            .into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;

    fn zip_of(names: &[&str]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut buffer));
            for name in names {
                writer
                    .start_file(*name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"header\n").unwrap();
            }
            writer.finish().unwrap();
        }
        buffer
    }

    fn valid_zip() -> Vec<u8> {
        zip_of(&REQUIRED)
    }

    #[test]
    fn cache_validators_round_trip_beside_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");
        let validators = CacheValidators {
            etag: Some("\"abc123\"".to_string()),
            last_modified: Some("Wed, 21 Oct 2026 07:28:00 GMT".to_string()),
        };

        validators.write(&dest);
        assert_eq!(CacheValidators::read(&dest), validators);
    }

    #[test]
    fn a_missing_sidecar_means_an_unconditional_request() {
        // Never an error. Losing the sidecar has to degrade to the old
        // behaviour -- a full download -- and never to a missed update.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");

        assert_eq!(CacheValidators::read(&dest), CacheValidators::default());
    }

    #[test]
    fn a_corrupt_sidecar_means_an_unconditional_request() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");
        std::fs::write(CacheValidators::path_for(&dest), "not json").unwrap();

        assert_eq!(CacheValidators::read(&dest), CacheValidators::default());
    }

    #[test]
    fn writing_no_validators_leaves_nothing_to_send_back() {
        // A server that gives neither header must not leave a stale sidecar
        // from an earlier response, or the next request conditions on a
        // validator for bytes we no longer hold.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");

        CacheValidators {
            etag: Some("\"old\"".to_string()),
            last_modified: None,
        }
        .write(&dest);
        CacheValidators::default().write(&dest);

        assert!(!CacheValidators::path_for(&dest).exists());
        assert_eq!(CacheValidators::read(&dest), CacheValidators::default());
    }

    #[test]
    fn the_sidecar_sits_beside_the_archive_not_in_place_of_it() {
        let dest = Path::new("data/raw/MTA_NYCT.zip");
        assert_eq!(
            CacheValidators::path_for(dest),
            Path::new("data/raw/MTA_NYCT.zip.cache.json"),
            "the .zip extension must be kept, or two agencies could collide"
        );
    }

    #[test]
    fn installs_a_valid_archive() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("raw").join("MTA_NYCT.zip");

        let bytes = install(&valid_zip(), &dest).unwrap();
        assert!(dest.exists());
        assert_eq!(bytes, valid_zip().len() as u64);
    }

    #[test]
    fn an_html_error_page_never_reaches_the_destination() {
        // The MTA serves one of these with a 200 when a feed is briefly down.
        // Landing it on disk as MTA_NYCT.zip turns a network problem into a
        // parse error days later, naming entirely the wrong cause.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");

        let err = install(b"<html><body>503 Service Unavailable</body></html>", &dest).unwrap_err();
        assert!(err.to_string().contains("zip"), "got: {err}");
        assert!(!dest.exists(), "nothing should be written");
    }

    #[test]
    fn a_truncated_download_never_reaches_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");
        let full = valid_zip();

        let err = install(&full[..full.len() / 2], &dest).unwrap_err();
        assert!(!dest.exists(), "got: {err}");
    }

    #[test]
    fn a_zip_missing_a_required_file_is_rejected() {
        // A zip that opens fine but is not a GTFS feed.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");

        let err = install(&zip_of(&["agency.txt", "routes.txt"]), &dest).unwrap_err();
        assert!(err.to_string().contains("stops.txt"), "got: {err}");
        assert!(!dest.exists());
    }

    #[test]
    fn a_failed_install_leaves_the_previous_archive_intact() {
        // The reason for validating before the rename. A feed that fails to
        // download must leave yesterday's working archive in place rather than
        // replacing it with rubble.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");
        install(&valid_zip(), &dest).unwrap();

        let err = install(b"not a zip", &dest).unwrap_err();
        assert!(dest.exists(), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), valid_zip());
    }

    #[test]
    fn reinstalling_over_an_existing_archive_succeeds() {
        // Windows will not rename onto an existing file, so --force would fail
        // on exactly the platform this is developed on.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");

        install(&valid_zip(), &dest).unwrap();
        install(&valid_zip(), &dest).unwrap();
        assert!(dest.exists());
    }

    #[test]
    fn no_part_file_is_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");
        install(&valid_zip(), &dest).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".part"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn files_nested_in_a_directory_still_validate() {
        // Some producers nest the CSVs one level down inside the archive.
        let nested: Vec<String> = REQUIRED.iter().map(|n| format!("gtfs/{n}")).collect();
        let names: Vec<&str> = nested.iter().map(String::as_str).collect();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("MTA_NYCT.zip");
        install(&zip_of(&names), &dest).unwrap();
        assert!(dest.exists());
    }

    #[test]
    fn an_existing_archive_is_reused_rather_than_redownloaded() {
        // No network involved: the point is that the early return happens
        // before anything reaches out. The static feeds change a few times a
        // year, and refetching tens of megabytes per run is rude to a free
        // service.
        let dir = tempfile::tempdir().unwrap();
        let agency = crate::config::Config::load(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/agencies.toml"
        )))
        .unwrap()
        .agency("MTA_NYCT")
        .unwrap()
        .clone();

        let dest = agency.archive_path(dir.path());
        install(&valid_zip(), &dest).unwrap();

        let fetched = static_archive(&agency, dir.path(), Duration::from_secs(1), false).unwrap();
        assert!(fetched.reused);
        assert_eq!(fetched.bytes, valid_zip().len() as u64);
        assert_eq!(fetched.agency_id, "MTA_NYCT");
    }
}
