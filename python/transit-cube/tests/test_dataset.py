"""Loading the Parquet dataset the Rust ingest writes.

The fixture below writes the same layout the ingest does rather than depending
on a built dataset, so these run on a clean checkout — the real output is tens
of megabytes and is not committed.
"""

from __future__ import annotations

import datetime as dt
from pathlib import Path

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq
import pytest

from transit_cube.calendar import ServicePeriod
from transit_cube.config import Agency, ConfigError
from transit_cube.dataset import (
    DatasetError,
    build_calendar,
    load_routes,
    load_scheduled_events,
    load_stops,
    parquet_root,
)

# 2026-05-26 is a Tuesday; 2026-05-25 is Memorial Day; 2026-05-30 a Saturday.
TUESDAY = dt.date(2026, 5, 26)
WEDNESDAY = dt.date(2026, 5, 27)

AGENCIES = {
    "MTA_NYCT": Agency(id="MTA_NYCT", name="Subway", mode="subway", timezone="America/New_York"),
}


def _fact_batch(rows: list[dict]) -> pa.Table:
    """One partition's worth of facts, in the ingest's schema.

    `service_date` is deliberately absent — it lives in the partition path.
    """
    return pa.table(
        {
            "event_id": pa.array([r["event_id"] for r in rows], pa.string()),
            "agency_id": pa.array([r["agency_id"] for r in rows], pa.string()),
            "route_id": pa.array(["1"] * len(rows), pa.string()),
            "route_key": pa.array([f"{r['agency_id']}:1" for r in rows], pa.string()),
            "trip_id": pa.array(["T1"] * len(rows), pa.string()),
            "stop_id": pa.array(["101N"] * len(rows), pa.string()),
            "stop_key": pa.array([f"{r['agency_id']}:101N" for r in rows], pa.string()),
            "stop_sequence": pa.array(range(1, len(rows) + 1), pa.int32()),
            "direction_id": pa.array([0] * len(rows), pa.int32()),
            "scheduled_arrival": pa.array(
                [r["arrival"] for r in rows], pa.timestamp("us", tz="UTC")
            ),
            "scheduled_departure": pa.array(
                [r["arrival"] for r in rows], pa.timestamp("us", tz="UTC")
            ),
            "dwell_seconds": pa.array([0] * len(rows), pa.int32()),
            "crosses_midnight": pa.array(
                [r.get("crosses_midnight", False) for r in rows], pa.bool_()
            ),
            "actual_arrival": pa.nulls(len(rows), pa.timestamp("us", tz="UTC")),
            "delay_seconds": pa.nulls(len(rows), pa.int32()),
            "headway_seconds": pa.nulls(len(rows), pa.int32()),
            "is_cancelled": pa.nulls(len(rows), pa.bool_()),
            "vehicle_id": pa.nulls(len(rows), pa.string()),
        }
    )


@pytest.fixture
def dataset(tmp_path: Path) -> Path:
    """A two-date, two-partition dataset plus its dimensions."""
    root = tmp_path / "data" / "parquet"

    partitions = {
        TUESDAY: [
            # 10:00 UTC = 06:00 EDT, an AM peak departure.
            {
                "event_id": "a",
                "agency_id": "MTA_NYCT",
                "arrival": dt.datetime(2026, 5, 26, 10, 0, tzinfo=dt.UTC),
            },
            # 05:30 UTC on the 27th = 01:30 EDT on the 27th, but this is the
            # 26th's 25:30 service. Overnight, and still a Tuesday.
            {
                "event_id": "b",
                "agency_id": "MTA_NYCT",
                "arrival": dt.datetime(2026, 5, 27, 5, 30, tzinfo=dt.UTC),
                "crosses_midnight": True,
            },
        ],
        WEDNESDAY: [
            # 21:00 UTC = 17:00 EDT, PM peak.
            {
                "event_id": "c",
                "agency_id": "MTA_NYCT",
                "arrival": dt.datetime(2026, 5, 27, 21, 0, tzinfo=dt.UTC),
            },
        ],
    }

    for service_date, rows in partitions.items():
        directory = root / "scheduled_events" / f"service_date={service_date}"
        directory.mkdir(parents=True)
        pq.write_table(_fact_batch(rows), directory / "MTA_NYCT.parquet")

    routes = root / "routes"
    routes.mkdir(parents=True)
    pq.write_table(
        pa.table(
            {
                "route_key": pa.array(["MTA_NYCT:1"], pa.string()),
                "route_id": pa.array(["1"], pa.string()),
                "agency_id": pa.array(["MTA_NYCT"], pa.string()),
                "mode": pa.array(["subway"], pa.string()),
                "display_name": pa.array(["1"], pa.string()),
            }
        ),
        routes / "MTA_NYCT.parquet",
    )

    stops = root / "stops"
    stops.mkdir(parents=True)
    pq.write_table(
        pa.table(
            {
                "stop_key": pa.array(["MTA_NYCT:101N"], pa.string()),
                "stop_id": pa.array(["101N"], pa.string()),
                "agency_id": pa.array(["MTA_NYCT"], pa.string()),
                "station_key": pa.array(["MTA_NYCT:101"], pa.string()),
                "borough": pa.array(["Unknown"], pa.string()),
            }
        ),
        stops / "MTA_NYCT.parquet",
    )

    return root


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
    morning rush-hour train in the middle of the night."""
    events = load_scheduled_events(dataset, agencies=AGENCIES).set_index("event_id")

    assert events.loc["a", "local_hour"] == 6
    assert events.loc["c", "local_hour"] == 17


def test_service_periods_come_from_the_local_hour(dataset: Path) -> None:
    events = load_scheduled_events(dataset, agencies=AGENCIES).set_index("event_id")

    assert events.loc["a", "service_period"] == str(ServicePeriod.AM_PEAK)
    assert events.loc["c", "service_period"] == str(ServicePeriod.PM_PEAK)


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
    assert stops["station_key"].tolist() == ["MTA_NYCT:101"]


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
