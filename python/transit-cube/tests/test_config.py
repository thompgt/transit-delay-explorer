"""The registry is shared with the Rust ingest, so these guard the contract."""

from __future__ import annotations

import pytest

from transit_cube.config import Agency, ConfigError, default_config_path, load_agencies


def test_loads_the_checked_in_registry() -> None:
    """The real file must load. This is what catches a typo in agencies.toml."""
    agencies = load_agencies()

    assert set(agencies) == {"MTA_NYCT", "MTA_LIRR", "MTA_MNR"}
    assert agencies["MTA_NYCT"].mode == "subway"


def test_finds_the_registry_by_walking_up() -> None:
    path = default_config_path()

    assert path.is_file()
    assert path.parts[-2:] == ("config", "agencies.toml")


def test_every_agency_timezone_resolves() -> None:
    for agency in load_agencies().values():
        assert agency.tz.key == agency.timezone


def test_an_unknown_timezone_is_rejected_at_load(tmp_path) -> None:
    """A typo must fail here, not midway through a cube build."""
    config = tmp_path / "agencies.toml"
    config.write_text(
        """
        [[agency]]
        id = "A"
        name = "A"
        mode = "subway"
        timezone = "Mars/Olympus_Mons"
        static_url = "http://example.com/a.zip"
        """,
        encoding="utf-8",
    )

    with pytest.raises(ConfigError, match="timezone"):
        load_agencies(config)


def test_duplicate_ids_are_rejected(tmp_path) -> None:
    config = tmp_path / "agencies.toml"
    config.write_text(
        """
        [[agency]]
        id = "A"
        name = "First"
        mode = "subway"
        timezone = "America/New_York"

        [[agency]]
        id = "A"
        name = "Second"
        mode = "bus"
        timezone = "America/New_York"
        """,
        encoding="utf-8",
    )

    with pytest.raises(ConfigError, match="duplicate"):
        load_agencies(config)


def test_a_registry_with_no_agencies_is_rejected(tmp_path) -> None:
    config = tmp_path / "agencies.toml"
    config.write_text("[defaults]\npoll_interval_seconds = 30\n", encoding="utf-8")

    with pytest.raises(ConfigError, match="no agencies"):
        load_agencies(config)


def test_a_missing_required_field_names_it(tmp_path) -> None:
    config = tmp_path / "agencies.toml"
    config.write_text(
        """
        [[agency]]
        id = "A"
        name = "A"
        mode = "subway"
        """,
        encoding="utf-8",
    )

    with pytest.raises(ConfigError, match="timezone"):
        load_agencies(config)


def test_extra_fields_are_ignored() -> None:
    """The registry carries feed URLs and quirks the cube has no use for."""
    agency = Agency(id="A", name="A", mode="subway", timezone="America/New_York")

    assert agency.tz.key == "America/New_York"
