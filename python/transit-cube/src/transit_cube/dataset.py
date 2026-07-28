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
"""

from __future__ import annotations

import datetime as dt
from pathlib import Path

import pandas as pd
import pyarrow as pa
import pyarrow.dataset as pads

from transit_cube.calendar import (
    US_HOLIDAYS_2026,
    DayType,
    ServicePeriod,
    classify_day_type,
    classify_service_period,
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


def load_scheduled_events(
    root: Path | None = None,
    dates: list[dt.date] | None = None,
    agencies: dict[str, Agency] | None = None,
) -> pd.DataFrame:
    """Load the fact table, with the agency-local columns the cube slices on.

    `dates` restricts the load to those service dates, pruning at the partition
    level so unrequested days are never opened.
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

    events["local_hour"] = _local_hour(events, agencies)
    events["service_period"] = _service_period(events["local_hour"])
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
    holidays: frozenset[dt.date] = US_HOLIDAYS_2026,
) -> pd.DataFrame:
    """Build the calendar dimension from the dates present in the facts.

    Derived from what the data actually contains rather than from a date range,
    so the dimension can never disagree with the fact table about which days
    exist — a mismatch there shows up as a hierarchy level with members that
    select nothing.
    """
    unique = sorted({_as_date(value) for value in service_dates})
    if not unique:
        raise DatasetError("no service dates to build a calendar from")

    frame = pd.DataFrame({"service_date": unique})
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


def _local_hour(events: pd.DataFrame, agencies: dict[str, Agency]) -> pd.Series:
    """The hour of `scheduled_arrival` in each row's own agency timezone.

    Grouped by agency rather than converted wholesale: the three MTA feeds
    happen to share a timezone, and hard-coding that would make the first
    agency in another one silently wrong.
    """
    pieces: list[pd.Series] = []

    for agency_id, group in events.groupby("agency_id", sort=False):
        agency = agencies.get(agency_id)
        if agency is None:
            raise ConfigError(
                f"the dataset contains agency {agency_id!r}, which the registry does not define"
            )
        pieces.append(group["scheduled_arrival"].dt.tz_convert(agency.tz).dt.hour)

    # Concatenated and reindexed rather than assigned into a preallocated
    # series: assigning by label upcasts to float on the first write, and a
    # float hour then fails every integer lookup downstream.
    return pd.concat(pieces).reindex(events.index).astype("int16")


def _service_period(local_hour: pd.Series) -> pd.Series:
    """Bucket local hours into service periods.

    Through a 24-entry lookup rather than a per-row call: the fact table runs to
    millions of rows and there are only ever twenty-four answers.
    """
    lookup = [str(classify_service_period(hour)) for hour in range(24)]
    return local_hour.map(lambda hour: lookup[hour]).astype("category")


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
