"""Reading the shared agency registry.

`config/agencies.toml` at the repo root is the single registry, read by both the
Rust ingest and this package. Duplicating the agency list in Python would mean
adding an agency in two places and eventually forgetting one — and the failure
mode is silent, because a missing entry here does not break a load, it just
mislabels every row that agency contributed.

Only the fields the cube needs are modelled. Feed URLs and realtime quirks are
the ingest's business.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass
from pathlib import Path
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError


class ConfigError(Exception):
    """The registry is missing, malformed, or names something unusable."""


@dataclass(frozen=True)
class Agency:
    """One configured agency, as the cube sees it."""

    id: str
    name: str
    mode: str
    timezone: str

    @property
    def tz(self) -> ZoneInfo:
        """The agency-local timezone.

        Service dates and service periods are local calendar concepts. Facts are
        stored in UTC, so every derivation of a local hour goes through here.
        """
        try:
            return ZoneInfo(self.timezone)
        except ZoneInfoNotFoundError as error:
            raise ConfigError(f"agency {self.id} has unknown timezone {self.timezone!r}") from error


def default_config_path(start: Path | None = None) -> Path:
    """Locate `config/agencies.toml` by walking up from `start`.

    Found by walking rather than by a fixed relative path, so the same call
    works from the package, from a test, and from a notebook opened somewhere
    else in the tree.
    """
    current = (start or Path(__file__)).resolve()
    for parent in current.parents:
        candidate = parent / "config" / "agencies.toml"
        if candidate.is_file():
            return candidate
    raise ConfigError(f"no config/agencies.toml found in any parent of {current}")


def load_agencies(path: Path | None = None) -> dict[str, Agency]:
    """Load the registry, keyed by agency id."""
    path = path or default_config_path()

    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ConfigError(f"could not read {path}") from error
    except tomllib.TOMLDecodeError as error:
        raise ConfigError(f"could not parse {path}: {error}") from error

    entries = raw.get("agency", [])
    if not entries:
        raise ConfigError(f"{path} defines no agencies")

    agencies: dict[str, Agency] = {}
    for entry in entries:
        try:
            agency = Agency(
                id=entry["id"],
                name=entry["name"],
                mode=entry["mode"],
                timezone=entry["timezone"],
            )
        except KeyError as error:
            raise ConfigError(f"{path}: agency entry missing {error}") from error

        if agency.id in agencies:
            raise ConfigError(f"{path}: duplicate agency id {agency.id!r}")

        # Resolved eagerly so a typo fails here rather than midway through a
        # cube build, where the traceback points somewhere unhelpful.
        _ = agency.tz
        agencies[agency.id] = agency

    return agencies
