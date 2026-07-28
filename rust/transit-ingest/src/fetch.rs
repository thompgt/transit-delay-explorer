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

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{info, warn};

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

/// Download `agency`'s static archive into `<data_dir>/raw/<agency>.zip`.
///
/// An existing archive is kept unless `force` is set: the static feeds change
/// a few times a year, and re-downloading tens of megabytes on every run to
/// get the same bytes is rude to a free service.
pub fn static_archive(
    agency: &Agency,
    data_dir: &Path,
    timeout: Duration,
    force: bool,
) -> Result<Fetched> {
    let dest = agency.archive_path(data_dir);

    if dest.exists() && !force {
        let bytes = std::fs::metadata(&dest)
            .map_err(|source| Error::Io {
                path: dest.clone(),
                source,
            })?
            .len();

        info!(agency = %agency.id, path = %dest.display(), "archive present; skipping download");
        return Ok(Fetched {
            agency_id: agency.id.clone(),
            path: dest,
            bytes,
            reused: true,
        });
    }

    let body = download(&agency.static_url, timeout)?;
    let bytes = install(&body, &dest)?;

    info!(
        agency = %agency.id,
        url = %agency.static_url,
        bytes,
        path = %dest.display(),
        "downloaded static archive"
    );

    Ok(Fetched {
        agency_id: agency.id.clone(),
        path: dest,
        bytes,
        reused: false,
    })
}

fn download(url: &str, timeout: Duration) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|source| FeedError::Download {
            url: url.to_string(),
            source: Box::new(source),
        })?;

    let response = client
        .get(url)
        .send()
        .map_err(|source| FeedError::Download {
            url: url.to_string(),
            source: Box::new(source),
        })?;

    // Checked explicitly rather than via error_for_status, so the message names
    // the code and the URL instead of reading as a generic request failure.
    if !response.status().is_success() {
        return Err(FeedError::HttpStatus {
            url: url.to_string(),
            status: response.status().as_u16(),
        }
        .into());
    }

    response.bytes().map(|b| b.to_vec()).map_err(|source| {
        FeedError::Download {
            url: url.to_string(),
            source: Box::new(source),
        }
        .into()
    })
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
