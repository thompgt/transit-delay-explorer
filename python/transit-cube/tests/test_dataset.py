"""Loading the Parquet dataset the Rust ingest writes.

The `dataset` fixture in `conftest.py` writes the same layout the ingest does
rather than depending on a built dataset, so these run on a clean checkout —
the real output is tens of megabytes and is not committed.
"""

from __future__ import annotations

import datetime as dt
from pathlib import Path

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq
import pytest
from conftest import AGENCIES, TUESDAY, WEDNESDAY

from transit_cube.calendar import ServicePeriod
from transit_cube.config import ConfigError
from transit_cube.dataset import (
    DatasetError,
    build_calendar,
    load_routes,
    load_scheduled_events,
    load_stops,
    parquet_root,
)


def test_loads_every_partition(dataset: Path) -> None:
    events = load_scheduled_events(dataset, agencies=AGENCIES)

    assert len(events) == 3
    assert set(events["event_id"]) == {"a", "b", "c"}


def test_service_date_reads_back_as_a_date_not_a_string(dataset: Path) -> None:
    """Hive discovery infers a string unless the partition schema says otherwise."""
    events = load_scheduled_events(dataset, agencies=AGENCIES)

    dates = set(events["service_date"])
    assert dates == {TUESDAY, WEDNESDAY}
    assert all(isinstance(value, dt.date) for value in dates)


def test_a_date_filter_prunes_partitions(dataset: Path) -> None:
    events = load_scheduled_events(dataset, dates=[WEDNESDAY], agencies=AGENCIES)

    assert len(events) == 1
    assert events["event_id"].iloc[0] == "c"


def test_an_empty_date_list_is_rejected(dataset: Path) -> None:
    """Distinguished from None, which means every date."""
    with pytest.raises(DatasetError, match="empty"):
        load_scheduled_events(dataset, dates=[], agencies=AGENCIES)


def test_a_filter_matching_nothing_is_an_error(dataset: Path) -> None:
    with pytest.raises(DatasetError, match="no rows"):
        load_scheduled_events(dataset, dates=[dt.date(2030, 1, 1)], agencies=AGENCIES)


def test_local_hour_is_agency_local_not_utc(dataset: Path) -> None:
    """10:00 UTC is 06:00 in New York; slicing on the UTC hour would put a
    morning rush-hour train in the middle of the night.

    The ingest writes this column; the loader reads it. It used to be derived
    here on every load, which meant the whole fact table was rebuilt in pandas
    before Atoti copied it again.
    """
    events = load_scheduled_events(dataset, agencies=AGENCIES).set_index("event_id")

    assert events.loc["a", "local_hour"] == 6
    assert events.loc["c", "local_hour"] == 17


def test_service_periods_come_from_the_local_hour(dataset: Path) -> None:
    events = load_scheduled_events(dataset, agencies=AGENCIES).set_index("event_id")

    assert events.loc["a", "service_period"] == str(ServicePeriod.AM_PEAK)
    assert events.loc["c", "service_period"] == str(ServicePeriod.PM_PEAK)


def test_a_dataset_without_the_derived_columns_names_them(tmp_path: Path) -> None:
    """An older ingest's output must fail here, not four hierarchies later."""
    root = tmp_path / "data" / "parquet"
    partition = root / "scheduled_events" / f"service_date={TUESDAY}"
    partition.mkdir(parents=True)
    pq.write_table(
        pa.table(
            {
                "event_id": pa.array(["a"], pa.string()),
                "agency_id": pa.array(["MTA_NYCT"], pa.string()),
            }
        ),
        partition / "MTA_NYCT.parquet",
    )

    with pytest.raises(DatasetError, match="local_hour"):
        load_scheduled_events(root, agencies=AGENCIES)


def test_an_overnight_call_keeps_its_service_date(dataset: Path) -> None:
    """The 25:30 case. It arrives at 01:30 on the 27th and is Overnight, but it
    belongs to the 26th's service and must not migrate to the next day."""
    events = load_scheduled_events(dataset, agencies=AGENCIES).set_index("event_id")

    assert events.loc["b", "service_date"] == TUESDAY
    assert events.loc["b", "local_hour"] == 1
    assert events.loc["b", "service_period"] == str(ServicePeriod.OVERNIGHT)
    assert bool(events.loc["b", "crosses_midnight"])


def test_an_agency_missing_from_the_registry_is_an_error(dataset: Path) -> None:
    """Silently mislabelling every row that agency contributed is worse."""
    with pytest.raises(ConfigError, match="MTA_NYCT"):
        load_scheduled_events(dataset, agencies={})


def test_dimensions_load(dataset: Path) -> None:
    routes = load_routes(dataset)
    stops = load_stops(dataset)

    assert routes["route_key"].tolist() == ["MTA_NYCT:1"]
    assert stops["station_key"].tolist() == ["MTA_NYCT:101"] * 2


def test_stops_carry_their_station_name(dataset: Path) -> None:
    """The platform level sits under a station level, and the ingest supplies
    the station key but not its name."""
    stops = load_stops(dataset).set_index("stop_key")

    assert stops.loc["MTA_NYCT:101N", "station_name"] == "Van Cortlandt Park-242 St"


def test_a_station_with_no_row_falls_back_to_its_key(tmp_path: Path) -> None:
    """A null member would collapse every orphaned platform under one parent."""
    stops = tmp_path / "data" / "parquet" / "stops"
    stops.mkdir(parents=True)
    pq.write_table(
        pa.table(
            {
                "stop_key": pa.array(["MTA_LIRR:8"], pa.string()),
                "stop_name": pa.array(["Hicksville"], pa.string()),
                "station_key": pa.array(["MTA_LIRR:missing"], pa.string()),
            }
        ),
        stops / "MTA_LIRR.parquet",
    )

    loaded = load_stops(tmp_path / "data" / "parquet")

    assert loaded["station_name"].tolist() == ["MTA_LIRR:missing"]


def test_a_missing_table_names_itself(tmp_path: Path) -> None:
    with pytest.raises(DatasetError, match="scheduled_events"):
        load_scheduled_events(tmp_path, agencies=AGENCIES)


def test_parquet_root_reports_how_to_create_it(tmp_path: Path) -> None:
    with pytest.raises(DatasetError, match="transit-ingest build"):
        parquet_root(tmp_path / "nowhere" / "deeper")


def test_calendar_is_built_from_the_dates_present(dataset: Path) -> None:
    """Derived from the facts, so the dimension cannot disagree with them about
    which days exist."""
    events = load_scheduled_events(dataset, agencies=AGENCIES)
    calendar = build_calendar(events["service_date"])

    assert calendar["service_date"].tolist() == [TUESDAY, WEDNESDAY]
    assert calendar["day_type"].tolist() == ["Weekday", "Weekday"]
    assert not calendar["is_holiday"].any()


def test_calendar_classifies_weekends_and_holidays() -> None:
    calendar = build_calendar(
        [
            dt.date(2026, 5, 25),  # Memorial Day, a Monday
            dt.date(2026, 5, 26),  # Tuesday
            dt.date(2026, 5, 30),  # Saturday
        ]
    ).set_index("service_date")

    assert calendar.loc[dt.date(2026, 5, 25), "day_type"] == "Holiday"
    assert calendar.loc[dt.date(2026, 5, 26), "day_type"] == "Weekday"
    assert calendar.loc[dt.date(2026, 5, 30), "day_type"] == "Weekend"
    assert calendar["is_holiday"].sum() == 1


def test_calendar_deduplicates_and_sorts() -> None:
    calendar = build_calendar([WEDNESDAY, TUESDAY, TUESDAY])

    assert calendar["service_date"].tolist() == [TUESDAY, WEDNESDAY]


def test_calendar_accepts_timestamps() -> None:
    """pandas hands date columns back in more than one shape."""
    calendar = build_calendar(pd.Series([pd.Timestamp("2026-05-26")]))

    assert calendar["service_date"].tolist() == [TUESDAY]


def test_an_empty_calendar_is_an_error() -> None:
    with pytest.raises(DatasetError, match="no service dates"):
        build_calendar([])
