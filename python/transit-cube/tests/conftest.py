"""The shared Parquet fixture.

Writes the same layout the Rust ingest does rather than depending on a built
dataset, so the tests run on a clean checkout — the real output is tens of
megabytes and is not committed.

It lives here rather than in `test_dataset.py` because the cube tests load the
same dataset, and a second copy of it would drift: the loader tests and the
cube tests disagreeing about what the ingest writes is exactly the bug this
package exists to catch.
"""

from __future__ import annotations

import datetime as dt
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

from transit_cube.config import Agency

# 2026-05-26 is a Tuesday; 2026-05-25 is Memorial Day; 2026-05-30 a Saturday.
TUESDAY = dt.date(2026, 5, 26)
WEDNESDAY = dt.date(2026, 5, 27)

AGENCIES = {
    "MTA_NYCT": Agency(id="MTA_NYCT", name="Subway", mode="subway", timezone="America/New_York"),
}


def fact_batch(rows: list[dict]) -> pa.Table:
    """One partition's worth of facts, in the ingest's schema.

    `service_date` is deliberately absent — it lives in the partition path.
    """
    return pa.table(
        {
            "event_id": pa.array([r["event_id"] for r in rows], pa.string()),
            "agency_id": pa.array([r["agency_id"] for r in rows], pa.string()),
            "route_id": pa.array(["1"] * len(rows), pa.string()),
            "route_key": pa.array([f"{r['agency_id']}:1" for r in rows], pa.string()),
            "trip_id": pa.array([r.get("trip_id", "T1") for r in rows], pa.string()),
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
            "dwell_seconds": pa.array([r.get("dwell", 0) for r in rows], pa.int32()),
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


def write_dataset(root: Path) -> Path:
    """Write a two-date, two-partition dataset plus its dimensions under `root`."""
    partitions = {
        TUESDAY: [
            # 10:00 UTC = 06:00 EDT, an AM peak departure.
            {
                "event_id": "a",
                "agency_id": "MTA_NYCT",
                "arrival": dt.datetime(2026, 5, 26, 10, 0, tzinfo=dt.UTC),
                "dwell": 30,
            },
            # 05:30 UTC on the 27th = 01:30 EDT on the 27th, but this is the
            # 26th's 25:30 service. Overnight, and still a Tuesday.
            {
                "event_id": "b",
                "agency_id": "MTA_NYCT",
                "arrival": dt.datetime(2026, 5, 27, 5, 30, tzinfo=dt.UTC),
                "crosses_midnight": True,
                "dwell": 30,
            },
        ],
        WEDNESDAY: [
            # 21:00 UTC = 17:00 EDT, PM peak. A second trip, so the distinct
            # trip count is not the same number as the row count.
            {
                "event_id": "c",
                "agency_id": "MTA_NYCT",
                "arrival": dt.datetime(2026, 5, 27, 21, 0, tzinfo=dt.UTC),
                "trip_id": "T2",
                "dwell": 60,
            },
        ],
    }

    for service_date, rows in partitions.items():
        directory = root / "scheduled_events" / f"service_date={service_date}"
        directory.mkdir(parents=True)
        pq.write_table(fact_batch(rows), directory / "MTA_NYCT.parquet")

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
                # The station and one of its platforms, which is the shape the
                # subway feed has: the platform's station_key points at another
                # row of this same table.
                "stop_key": pa.array(["MTA_NYCT:101", "MTA_NYCT:101N"], pa.string()),
                "stop_id": pa.array(["101", "101N"], pa.string()),
                "agency_id": pa.array(["MTA_NYCT"] * 2, pa.string()),
                "stop_name": pa.array(["Van Cortlandt Park-242 St"] * 2, pa.string()),
                "is_station": pa.array([True, False], pa.bool_()),
                "station_key": pa.array(["MTA_NYCT:101"] * 2, pa.string()),
                "borough": pa.array(["Bronx"] * 2, pa.string()),
                "municipality": pa.array(["New York"] * 2, pa.string()),
            }
        ),
        stops / "MTA_NYCT.parquet",
    )

    return root


@pytest.fixture
def dataset(tmp_path: Path) -> Path:
    return write_dataset(tmp_path / "data" / "parquet")


@pytest.fixture(scope="module")
def module_dataset(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """The same dataset, written once for a module.

    The cube tests share one Atoti session because starting one costs seconds
    of JVM boot, and a module-scoped session cannot depend on a function-scoped
    directory.
    """
    return write_dataset(tmp_path_factory.mktemp("dataset") / "data" / "parquet")
