//! Agency registry loaded from `config/agencies.toml`.
//!
//! Adding an agency must be a config entry, not a code change, so everything
//! that varies between agencies lives here: feed URLs, the agency-local
//! timezone used to resolve service dates, and the per-agency quirks that the
//! parsers key off.

use std::path::{Path, PathBuf};

use chrono_tz::Tz;
use serde::Deserialize;

use crate::error::{ConfigError, Result};

/// The whole registry: shared defaults plus one entry per agency.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "agency")]
    pub agencies: Vec<Agency>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// MTA regenerates roughly every 30s; polling faster just re-fetches
    /// identical payloads.
    #[serde(default = "d_poll")]
    pub poll_interval_seconds: u64,
    /// A feed whose header timestamp is older than this is stale: it raises a
    /// health event rather than silently producing old data as if it were new.
    #[serde(default = "d_stale")]
    pub stale_after_seconds: u64,
    #[serde(default = "d_timeout")]
    pub request_timeout_seconds: u64,
    /// Threshold for the default on-time-performance measure.
    #[serde(default = "d_on_time")]
    pub on_time_threshold_seconds: i64,
}

fn d_poll() -> u64 {
    30
}
fn d_stale() -> u64 {
    300
}
fn d_timeout() -> u64 {
    20
}
fn d_on_time() -> i64 {
    300
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            poll_interval_seconds: d_poll(),
            stale_after_seconds: d_stale(),
            request_timeout_seconds: d_timeout(),
            on_time_threshold_seconds: d_on_time(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agency {
    /// Namespace for every key this agency contributes. `route_id` and
    /// `stop_id` are unique only within an agency, so this prefixes both.
    pub id: String,
    pub name: String,
    pub mode: String,
    /// Agency-local timezone. Service dates are a local calendar concept, not
    /// a UTC day, and the two diverge across DST transitions.
    pub timezone: String,
    pub static_url: String,
    /// A single agency may split realtime across several endpoints — NYCT
    /// publishes one payload per trunk line over one shared static schedule.
    #[serde(default)]
    pub realtime: Vec<RealtimeFeed>,
    #[serde(default)]
    pub quirks: Quirks,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeFeed {
    pub name: String,
    pub url: String,
}

/// Per-agency deviations from the spec. Every one of these is a real MTA
/// behaviour documented in `docs/FEED_NOTES.md`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quirks {
    #[serde(default)]
    pub trip_id_match: TripIdMatch,
    /// NYCT appends a direction letter to the parent stop id (`101` -> `101N`,
    /// `101S`). True for the subway, false for the railroads.
    #[serde(default)]
    pub stop_id_direction_suffix: bool,
}

/// How a realtime `trip_id` is matched back to the static schedule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripIdMatch {
    /// The realtime id equals the static id.
    #[default]
    Exact,
    /// NYCT realtime ids carry a schedule-run prefix that the static feed also
    /// carries but which differs between them; match on the suffix after the
    /// last underscore.
    NyctSuffix,
}

impl Agency {
    /// Namespaced surrogate key for a route. `route_id` collides across
    /// agencies — subway route `1` and LIRR route `1` are unrelated lines.
    pub fn route_key(&self, route_id: &str) -> String {
        format!("{}:{}", self.id, route_id)
    }

    /// Namespaced surrogate key for a stop. 104 `stop_id` values are shared
    /// across the three MTA feeds.
    pub fn stop_key(&self, stop_id: &str) -> String {
        format!("{}:{}", self.id, stop_id)
    }

    /// Where this agency's downloaded static archive lives under `data_dir`.
    /// Named after the agency id rather than the URL's filename, so two
    /// agencies that both publish `gtfs.zip` do not overwrite each other.
    pub fn archive_path(&self, data_dir: &Path) -> PathBuf {
        data_dir.join("raw").join(format!("{}.zip", self.id))
    }

    /// Parsed timezone. Validated at load so a typo fails at startup rather
    /// than midway through a run.
    pub fn tz(&self) -> Result<Tz> {
        self.timezone.parse::<Tz>().map_err(|_| {
            ConfigError::UnknownTimezone {
                agency: self.id.clone(),
                timezone: self.timezone.clone(),
            }
            .into()
        })
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let config: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Fail fast on anything that would otherwise surface as a confusing error
    /// deep in a run: duplicate ids, bad timezones, no agencies at all.
    fn validate(&self) -> Result<()> {
        if self.agencies.is_empty() {
            return Err(ConfigError::NoAgencies.into());
        }
        let mut seen = std::collections::HashSet::new();
        for agency in &self.agencies {
            if !seen.insert(&agency.id) {
                return Err(ConfigError::DuplicateAgencyId {
                    agency: agency.id.clone(),
                }
                .into());
            }
            agency.tz()?;
        }
        Ok(())
    }

    pub fn agency(&self, id: &str) -> Option<&Agency> {
        self.agencies.iter().find(|a| a.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-in registry must actually load — this is the test that
    /// catches a typo in `agencies.toml` before a run does.
    fn repo_config() -> Config {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/agencies.toml")
            .canonicalize()
            .expect("config/agencies.toml should exist");
        Config::load(&path).expect("checked-in config should load")
    }

    #[test]
    fn loads_repo_config() {
        let config = repo_config();
        assert_eq!(config.agencies.len(), 3);
        for id in ["MTA_NYCT", "MTA_LIRR", "MTA_MNR"] {
            assert!(config.agency(id).is_some(), "missing agency {id}");
        }
    }

    #[test]
    fn nyct_splits_realtime_across_trunk_lines() {
        let config = repo_config();
        let nyct = config.agency("MTA_NYCT").unwrap();
        assert_eq!(nyct.realtime.len(), 8);
        assert_eq!(nyct.quirks.trip_id_match, TripIdMatch::NyctSuffix);
        assert!(nyct.quirks.stop_id_direction_suffix);
    }

    #[test]
    fn railroads_match_trip_ids_exactly() {
        let config = repo_config();
        for id in ["MTA_LIRR", "MTA_MNR"] {
            let agency = config.agency(id).unwrap();
            assert_eq!(agency.quirks.trip_id_match, TripIdMatch::Exact);
            assert!(!agency.quirks.stop_id_direction_suffix);
        }
    }

    /// Route `1` exists in all three feeds as three unrelated lines. The
    /// surrogate key is what keeps them apart.
    #[test]
    fn keys_are_namespaced_per_agency() {
        let config = repo_config();
        let keys: Vec<_> = config.agencies.iter().map(|a| a.route_key("1")).collect();
        assert_eq!(keys, ["MTA_NYCT:1", "MTA_LIRR:1", "MTA_MNR:1"]);

        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(
            unique.len(),
            3,
            "route keys must not collide across agencies"
        );
    }

    #[test]
    fn every_agency_timezone_parses() {
        for agency in repo_config().agencies {
            assert!(agency.tz().is_ok(), "{} has a bad timezone", agency.id);
        }
    }

    #[test]
    fn rejects_duplicate_agency_ids() {
        let toml = r#"
            [[agency]]
            id = "A"
            name = "A"
            mode = "subway"
            timezone = "America/New_York"
            static_url = "http://example.com/a.zip"

            [[agency]]
            id = "A"
            name = "Also A"
            mode = "bus"
            timezone = "America/New_York"
            static_url = "http://example.com/b.zip"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unknown_timezone() {
        let toml = r#"
            [[agency]]
            id = "A"
            name = "A"
            mode = "subway"
            timezone = "Mars/Olympus_Mons"
            static_url = "http://example.com/a.zip"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn defaults_apply_when_section_omitted() {
        let toml = r#"
            [[agency]]
            id = "A"
            name = "A"
            mode = "subway"
            timezone = "America/New_York"
            static_url = "http://example.com/a.zip"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.defaults.poll_interval_seconds, 30);
        assert_eq!(config.defaults.stale_after_seconds, 300);
    }
}
