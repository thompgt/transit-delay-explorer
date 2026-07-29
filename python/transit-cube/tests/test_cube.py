"""The cube: tables, joins, hierarchies, and the schedule measures.

These need Atoti, which pulls a bundled JVM and does not support the Python a
current machine has — see `python/transit-cube/Dockerfile`. They skip rather
than fail where it is absent, so `pytest` still runs the loader tests on a bare
interpreter, and the container run is what covers this file.

One session serves the whole module. Starting one costs seconds of JVM boot,
and nothing here mutates the cube, so a per-test session would buy isolation
that is not at risk and pay for it many times over.
"""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path

import pandas as pd
import pytest
from conftest import TUESDAY, WEDNESDAY

atoti = pytest.importorskip("atoti", reason="Atoti is not installed on this interpreter")

from transit_cube.calendar import ServicePeriod  # noqa: E402
from transit_cube.cube import CUBE_NAME, build  # noqa: E402


@pytest.fixture(scope="module")
def session(module_dataset: Path) -> Iterator[atoti.Session]:
    """A session over the shared Parquet fixture."""
    session = build(root=module_dataset, port=0)
    try:
        yield session
    finally:
        session.close()


@pytest.fixture(scope="module")
def cube(session: atoti.Session) -> atoti.Cube:
    return session.cubes[CUBE_NAME]


def test_every_table_is_loaded(session: atoti.Session) -> None:
    assert set(session.tables) == {"scheduled_events", "routes", "stops", "calendar"}
    assert session.tables["scheduled_events"].row_count == 3
    assert session.tables["calendar"].row_count == 2


def test_the_hierarchies_the_dashboard_slices_on_exist(cube: atoti.Cube) -> None:
    names = {name for _, name in cube.hierarchies}

    assert names == {
        "Geography",
        "Network",
        "Calendar",
        "Day Type",
        "Service Period",
        "Hour of Day",
        "Direction",
    }


def test_geography_is_borough_to_platform(cube: atoti.Cube) -> None:
    """Ragged: only the subway has real stations, so the ingest gives the flat
    agencies station = stop and the level exists everywhere."""
    levels = list(cube.hierarchies["Geography"])

    assert levels == ["borough", "municipality", "station_name", "stop_id"]


def test_network_stops_at_route_because_gtfs_has_no_line(cube: atoti.Cube) -> None:
    levels = list(cube.hierarchies["Network"])

    assert levels == ["agency_id", "mode", "display_name"]


def test_calendar_and_hour_are_separate_hierarchies(cube: atoti.Cube) -> None:
    """A hierarchy cannot span tables: the date levels belong to the calendar
    dimension and the hour to the fact row, so they cannot nest."""
    assert list(cube.hierarchies["Calendar"]) == ["year", "month", "service_date"]
    assert list(cube.hierarchies["Hour of Day"]) == ["local_hour"]


def test_trips_are_counted_distinct_not_by_row(cube: atoti.Cube) -> None:
    """A trip contributes one row per stop it calls at; counting rows would
    report a 38-stop local as 38 trips."""
    result = cube.query(cube.measures["Scheduled Stops"], cube.measures["Scheduled Trips"])

    assert result["Scheduled Stops"][0] == 3
    assert result["Scheduled Trips"][0] == 2


def test_service_days_come_from_the_calendar_dimension(cube: atoti.Cube) -> None:
    """So it stays the number of days in the window at every level, which is
    what makes the per-day measures comparable across routes."""
    result = cube.query(cube.measures["Service Days"], cube.measures["Trips per Service Day"])

    assert result["Service Days"][0] == 2
    assert result["Trips per Service Day"][0] == 1.0


def test_overnight_share_is_the_cross_midnight_fraction(cube: atoti.Cube) -> None:
    """Surfaced as a measure because it is the fastest way to see in the UI
    whether a schema change has moved overnight service to the wrong date."""
    result = cube.query(cube.measures["Overnight Stops"], cube.measures["Overnight Share"])

    assert result["Overnight Stops"][0] == 1
    assert result["Overnight Share"][0] == pytest.approx(1 / 3)


def test_the_phase_two_question_is_answerable(cube: atoti.Cube) -> None:
    """Scheduled trips per route per hour by day type — the workplan's
    done-when for this phase, asked here the way the UI asks it.

    Asking for a level brings its ancestors with it, which is why the route
    breakdown arrives already grouped by agency and mode.
    """
    result = cube.query(
        cube.measures["Scheduled Trips"],
        levels=[
            cube.levels["display_name"],
            cube.levels["local_hour"],
            cube.levels["day_type"],
        ],
    ).reset_index()

    assert list(result.columns) == [
        "agency_id",
        "mode",
        "display_name",
        "local_hour",
        "day_type",
        "Scheduled Trips",
    ]

    peaks = result[result["local_hour"].isin([6, 17])]
    assert peaks["display_name"].tolist() == ["1", "1"]
    assert peaks["day_type"].tolist() == ["Weekday", "Weekday"]
    assert peaks["Scheduled Trips"].tolist() == [1, 1]


def test_an_overnight_call_slices_under_its_own_service_date(cube: atoti.Cube) -> None:
    """The 25:30 case, end to end: it arrives at 01:30 on the 27th and is
    Overnight, but it must count against the 26th."""
    result = cube.query(
        cube.measures["Scheduled Stops"],
        levels=[cube.levels["service_date"], cube.levels["service_period"]],
    ).reset_index()
    dated = {
        (pd.Timestamp(date).date(), period): stops
        for date, period, stops in zip(
            result["service_date"],
            result["service_period"],
            result["Scheduled Stops"],
            strict=True,
        )
    }

    assert dated[(TUESDAY, str(ServicePeriod.OVERNIGHT))] == 1
    assert dated[(TUESDAY, str(ServicePeriod.AM_PEAK))] == 1
    assert (WEDNESDAY, str(ServicePeriod.OVERNIGHT)) not in dated


def test_dwell_is_averaged_not_summed(cube: atoti.Cube) -> None:
    result = cube.query(cube.measures["Mean Dwell Seconds"])

    assert result["Mean Dwell Seconds"][0] == pytest.approx(40.0)
