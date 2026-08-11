"""Reading the Parquet dataset the Rust ingest writes.

This is the seam between the two halves of the project, and the two things it
has to get right are both about `service_date`.

It is the **partition key, stored in the path and not in the file** — see
`docs/DATA_MODEL.md`. Hive discovery hands partition values back as strings
unless told otherwise, so the partitioning schema is declared explicitly and
dates arrive as dates.

And it is **not the same thing as the calendar date of the timestamp**. A train
scheduled at 25:30 on 2026-05-26 arrives at 01:30 on the 27th; it belongs to
the 26th's service, and the day type that matters is the 26th's. Deriving day
type from the timestamp instead of the partition would file a Friday-night
train under Saturday and quietly move a slice of overnight service between two
answers.

Nothing here derives a fact column any more. `local_hour`, `service_period` and
`overnight` are written by the ingest, which holds the agency-local instant
they come from; this module reads them. The calendar dimension is still built
in Python, because it is derived from the *set of dates present* rather than
from any one row.
"""

from __future__ import annotations

import datetime as dt
from pathlib import Path

import pandas as pd
import pyarrow as pa
import pyarrow.dataset as pads

from transit_cube.calendar import (
    DayType,
    ServicePeriod,
    classify_day_type,
)
from transit_cube.config import Agency, ConfigError, load_agencies

FACT_TABLE = "scheduled_events"

#: The partition column, declared so it reads back as a date rather than the
#: string Hive discovery would otherwise infer.
PARTITIONING = pads.partitioning(pa.schema([("service_date", pa.date32())]), flavor="hive")


class DatasetError(Exception):
    """The dataset is missing, empty, or inconsistent with the registry."""


def parquet_root(start: Path | None = None) -> Path:
    """Locate `data/parquet` by walking up from `start`."""
    current = (start or Path(__file__)).resolve()
    for parent in current.parents:
        candidate = parent / "data" / "parquet"
        if candidate.is_dir():
            return candidate
    raise DatasetError(
        f"no data/parquet found in any parent of {current}; run `transit-ingest build` first"
    )


#: Columns the ingest derives at write time. Checked on load rather than
#: assumed: a dataset built by an older ingest would otherwise reach the cube
#: missing the columns two of its hierarchies are built from, and fail somewhere
#: much less informative.
DERIVED_COLUMNS = ("local_hour", "service_period", "overnight")


def load_scheduled_events(
    root: Path | None = None,
    dates: list[dt.date] | None = None,
    agencies: dict[str, Agency] | None = None,
) -> pd.DataFrame:
    """Load the fact table.

    `dates` restricts the load to those service dates, pruning at the partition
    level so unrequested days are never opened.

    The agency-local columns — `local_hour`, `service_period`, `overnight` —
    are read straight from the Parquet. They used to be computed here, per
    load, with a groupby-and-convert over the whole fact table: a 7-day
    tri-agency window is millions of rows, and deriving three columns onto them
    meant the table existed twice in pandas before Atoti copied it a third time.
    They are properties of a row, so the ingest stamps them on once at write
    time from the local instant it has already resolved. See
    `rust/transit-ingest/src/schedule.rs`.

    `agencies` is still validated against, because a fact row from an agency the
    registry does not define is a real inconsistency worth failing on — it just
    no longer drives a timezone conversion.
    """
    root = root or parquet_root()
    agencies = agencies if agencies is not None else load_agencies()

    path = root / FACT_TABLE
    if not path.is_dir():
        raise DatasetError(f"no {FACT_TABLE} table at {path}")

    dataset = pads.dataset(path, format="parquet", partitioning=PARTITIONING)

    filter_expr = None
    if dates is not None:
        if not dates:
            raise DatasetError("dates was empty; omit it to load every date")
        filter_expr = pads.field("service_date").isin(dates)

    events = dataset.to_table(filter=filter_expr).to_pandas()
    if events.empty:
        raise DatasetError(f"{path} matched no rows")

    missing = [column for column in DERIVED_COLUMNS if column not in events.columns]
    if missing:
        raise DatasetError(
            f"{path} has no {', '.join(missing)} column; it was written by an ingest older "
            f"than the one that derives them. Rebuild with `transit-ingest build`."
        )

    _check_agencies(events, agencies)
    return events


def load_routes(root: Path | None = None) -> pd.DataFrame:
    """Load the routes dimension, concatenated across agencies."""
    return _load_dimension(root, "routes")


def load_stops(root: Path | None = None) -> pd.DataFrame:
    """Load the stops dimension, with the station name resolved onto each row.

    The geography hierarchy has a station level above the platform level, and
    the ingest gives every stop a `station_key` but no station *name* — for a
    subway platform that key points at another row of this same table. Resolved
    here rather than in the ingest because it is a display concern that would
    otherwise duplicate the station name across every one of its platforms in
    the Parquet.
    """
    stops = _load_dimension(root, "stops")

    names = stops.set_index("stop_key")["stop_name"]
    # Stations that no row defines fall back to their key rather than to NaN:
    # a null member would collapse every such platform into one bucket under a
    # single "no station" parent, which reads as a data-free hierarchy rather
    # than as a gap in one dimension row.
    stops["station_name"] = stops["station_key"].map(names).fillna(stops["station_key"])
    return stops


def build_calendar(
    service_dates: list[dt.date] | pd.Series,
    holidays: frozenset[dt.date] | None = None,
) -> pd.DataFrame:
    """Build the calendar dimension from the dates present in the facts.

    Derived from what the data actually contains rather than from a date range,
    so the dimension can never disagree with the fact table about which days
    exist — a mismatch there shows up as a hierarchy level with members that
    select nothing.

    `holidays` defaults to each date's own year, so a dataset spanning a New
    Year gets both years' holidays and a dataset in a year nobody has entered
    raises. It used to default to a flat 2026 set, which meant a 2027 dataset
    reported Christmas as a weekday and looked entirely healthy doing it.

    :raises UncoveredYearError: for a year with no holiday calendar.
    """
    unique = sorted({_as_date(value) for value in service_dates})
    if not unique:
        raise DatasetError("no service dates to build a calendar from")

    frame = pd.DataFrame({"service_date": unique})
    # Passing holidays=None per date is what lets a window spanning New Year
    # pick up both years rather than one of them.
    day_types = [classify_day_type(date, holidays) for date in unique]

    frame["day_type"] = [str(day_type) for day_type in day_types]
    frame["is_holiday"] = [day_type is DayType.HOLIDAY for day_type in day_types]
    frame["year"] = [date.year for date in unique]
    frame["month"] = [date.month for date in unique]
    # Monday=0, matching both datetime.date.weekday and the ingest's ordering.
    frame["day_of_week"] = [date.weekday() for date in unique]
    return frame


def _load_dimension(root: Path | None, table: str) -> pd.DataFrame:
    root = root or parquet_root()
    path = root / table
    if not path.is_dir():
        raise DatasetError(f"no {table} table at {path}")

    frame = pads.dataset(path, format="parquet").to_table().to_pandas()
    if frame.empty:
        raise DatasetError(f"{path} is empty")
    return frame


def _check_agencies(events: pd.DataFrame, agencies: dict[str, Agency]) -> None:
    """Every agency in the facts must be one the registry defines.

    Over the distinct values rather than the rows: there are three of them and
    millions of rows. Silently mislabelling every row an unknown agency
    contributed is worse than refusing to load.
    """
    unknown = sorted(set(events["agency_id"].unique()) - set(agencies))
    if unknown:
        raise ConfigError(
            f"the dataset contains agency {unknown[0]!r}, which the registry does not define"
        )


def _as_date(value: object) -> dt.date:
    """Normalise whatever pandas hands back for a date column."""
    if isinstance(value, dt.datetime):
        return value.date()
    if isinstance(value, dt.date):
        return value
    if isinstance(value, pd.Timestamp):
        return value.date()
    raise DatasetError(f"{value!r} is not a date")


__all__ = [
    "DERIVED_COLUMNS",
    "FACT_TABLE",
    "PARTITIONING",
    "DatasetError",
    "DayType",
    "ServicePeriod",
    "build_calendar",
    "load_routes",
    "load_scheduled_events",
    "load_stops",
    "parquet_root",
]
