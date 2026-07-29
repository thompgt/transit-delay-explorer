"""The Atoti cube: tables, joins, hierarchies, measures.

Phase 2 of the workplan builds this on **static data only**, deliberately and
before any streaming exists. The point is to find out whether the schema is
right while it is still cheap to change — an awkward hierarchy discovered here
costs an edit, and the same one discovered after Phase 4 is built on top of it
costs a rewrite.

So every measure here is a *schedule* measure. `delay_seconds` and the other
realtime columns are present in the fact table and typed, but they are entirely
null until Phase 3 fills them, and a p90 of nothing is not a useful thing to
put in a dashboard. They arrive in Phase 5 against the same schema.

Two structural facts about this model are worth knowing before reading the
hierarchy definitions, because both are forced rather than chosen:

**Every join key is namespaced.** `route_id` `1` names three unrelated
railroads across the MTA's feeds and 104 `stop_id` values are shared between
agencies, so the joins are on `{agency_id}:{id}` and never on the bare id. See
`docs/FEED_NOTES.md`.

**A hierarchy cannot span tables.** Atoti builds each hierarchy from columns of
one table, which splits the "calendar time" hierarchy the data model sketches
(year -> month -> date -> hour) in two: the date levels are properties of the
calendar dimension and the hour is a property of the fact row. They are exposed
as `Calendar` and `Hour of Day` rather than faked into one hierarchy by copying
the date onto the facts.
"""

from __future__ import annotations

import datetime as dt
from dataclasses import dataclass
from pathlib import Path

import atoti as tt
import pandas as pd

from transit_cube.dataset import (
    build_calendar,
    load_routes,
    load_scheduled_events,
    load_stops,
)

CUBE_NAME = "Transit"

FACT_TABLE = "scheduled_events"
ROUTES_TABLE = "routes"
STOPS_TABLE = "stops"
CALENDAR_TABLE = "calendar"

#: Shown wherever a fact references a dimension row that does not exist. Atoti
#: would otherwise drop such facts out of every sliced total while leaving them
#: in the grand total, which looks like an arithmetic bug rather than like the
#: referential gap it is. The ingest validates these keys, so seeing this member
#: in the UI means something upstream regressed.
UNKNOWN = "Unknown"


@dataclass(frozen=True)
class CubeTables:
    """The four tables the cube is built from, after joining."""

    events: tt.Table
    routes: tt.Table
    stops: tt.Table
    calendar: tt.Table


def start_session(
    port: int = 0,
    content_storage: Path | None = None,
) -> tt.Session:
    """Start an Atoti session.

    `port=0` asks for an ephemeral one, which is what tests want; the server
    picks a free port and reports it back as `session.port`.

    `content_storage` is where the server keeps dashboards. Without it Atoti
    keeps them in memory and every dashboard is lost on restart — acceptable for
    a test, not for a project whose deliverable is the dashboard.
    """
    return tt.Session.start(tt.SessionConfig(port=port, user_content_storage=content_storage))


def load_tables(
    session: tt.Session,
    root: Path | None = None,
    dates: list[dt.date] | None = None,
) -> CubeTables:
    """Read the Parquet dataset into session tables and join them.

    Loaded through pandas rather than `session.read_parquet` because the fact
    table needs the two derived agency-local columns — `local_hour` and
    `service_period` — which do not exist in the Parquet and cannot be computed
    from a UTC timestamp without knowing each row's agency timezone. See
    `transit_cube.dataset`.
    """
    events_frame = load_scheduled_events(root, dates=dates)
    routes_frame = load_routes(root)
    stops_frame = load_stops(root)
    calendar_frame = build_calendar(events_frame["service_date"])

    events = session.read_pandas(
        _facts_for_atoti(events_frame),
        table_name=FACT_TABLE,
        # No key. Facts are append-only and `event_id` is a hash of the natural
        # key, so declaring it a key would buy uniqueness enforcement at the
        # cost of an index over a million-row table that nothing looks up by id.
        #
        # Atoti refuses to build a hierarchy on a column whose default value is
        # null, so every column that becomes a level needs one. `direction_id`
        # is the only genuinely nullable one — GTFS makes the field optional and
        # the MTA's feeds omit it on some routes — and it gets -1 rather than 0,
        # because 0 is a real direction and folding "unspecified" into it would
        # silently inflate one side of every directional comparison.
        default_values={"direction_id": -1, "local_hour": -1, "service_period": UNKNOWN},
    )
    routes = session.read_pandas(
        routes_frame,
        table_name=ROUTES_TABLE,
        keys=["route_key"],
        default_values={"mode": UNKNOWN, "display_name": UNKNOWN},
    )
    stops = session.read_pandas(
        stops_frame,
        table_name=STOPS_TABLE,
        keys=["stop_key"],
        default_values={
            "borough": UNKNOWN,
            "municipality": UNKNOWN,
            "station_name": UNKNOWN,
            "stop_id": UNKNOWN,
        },
    )
    calendar = session.read_pandas(
        calendar_frame,
        table_name=CALENDAR_TABLE,
        keys=["service_date"],
        default_values={"year": -1, "month": -1, "day_type": UNKNOWN, "day_of_week": -1},
    )

    events.join(routes, events["route_key"] == routes["route_key"])
    events.join(stops, events["stop_key"] == stops["stop_key"])
    events.join(calendar, events["service_date"] == calendar["service_date"])

    return CubeTables(events=events, routes=routes, stops=stops, calendar=calendar)


def _facts_for_atoti(frame: pd.DataFrame) -> pd.DataFrame:
    """The fact frame, adapted to what the server can hold and aggregate.

    Beyond the dtype widening, this adds an integer `overnight` alongside the
    boolean `crosses_midnight`. Summing the boolean directly would be the
    obvious way to count cross-midnight calls, but Atoti compiles a sum to Java
    and its ternary rejects a boolean constant outright, so the flag is carried
    as the 0/1 the aggregation actually wants. `crosses_midnight` stays, since
    it is the column anyone reading the Parquet expects to find.
    """
    facts = _widen_for_atoti(frame)
    facts["overnight"] = frame["crosses_midnight"].astype("int32")
    return facts


def _widen_for_atoti(frame: pd.DataFrame) -> pd.DataFrame:
    """Widen dtypes Atoti's type system has no equivalent for.

    Atoti's scalar types stop at 32-bit integers and it has no categorical
    type, so the two compact dtypes `transit_cube.dataset` chooses — `int16`
    for the local hour and `category` for the service period — are rejected
    outright with `Field ... has unsupported DataType`, which does not name the
    dtype that caused it. Those choices are right for pandas and the widening
    costs a few megabytes on a million rows, so it happens here at the seam
    rather than by making the loader worse for its other callers.
    """
    widened = frame.copy(deep=False)
    for column, dtype in frame.dtypes.items():
        if isinstance(dtype, pd.CategoricalDtype):
            widened[column] = frame[column].astype("string")
        elif pd.api.types.is_integer_dtype(dtype) and dtype.itemsize < 4:
            widened[column] = frame[column].astype("int32")
    return widened


def build_cube(session: tt.Session, tables: CubeTables) -> tt.Cube:
    """Define the cube over already-joined tables.

    Built in `manual` mode. The default would turn every column of every table
    into a hierarchy, which here means a level per `trip_id` and per `event_id`
    — hundreds of thousands of members apiece, unusable in the UI and expensive
    to feed. Levels are opted into instead.
    """
    cube = session.create_cube(tables.events, CUBE_NAME, mode="manual")
    _define_hierarchies(cube, tables)
    _define_measures(cube, tables)
    return cube


def _define_hierarchies(cube: tt.Cube, tables: CubeTables) -> None:
    events, routes, stops, calendar = (
        tables.events,
        tables.routes,
        tables.stops,
        tables.calendar,
    )
    hierarchies = cube.hierarchies

    # Geography is ragged: only the subway models stations as parents of
    # platforms. For the flat agencies the ingest emits station = stop, so the
    # level exists everywhere and slicing across agencies does not break.
    #
    # The platform level is `stop_id` and not `stop_key`, because the key is
    # only namespaced to survive cross-agency collisions and the namespace is
    # already implied by the station above it.
    hierarchies["Geography"] = [
        stops["borough"],
        stops["municipality"],
        stops["station_name"],
        stops["stop_id"],
    ]

    # The data model sketches agency -> mode -> line -> route. There is no line
    # level: GTFS has no concept of one, the MTA's own "lines" are an editorial
    # grouping that appears in no feed, and inventing one from route names would
    # be a guess presented as data.
    hierarchies["Network"] = [
        routes["agency_id"],
        routes["mode"],
        routes["display_name"],
    ]

    hierarchies["Calendar"] = [
        calendar["year"],
        calendar["month"],
        calendar["service_date"],
    ]

    # Analytical time, deliberately independent of the calendar: "PM peak" is a
    # property of the hour and "holiday" of the date, and neither nests inside
    # the other. Both are single-level hierarchies so they compose in the UI
    # rather than forcing an order on the user.
    hierarchies["Day Type"] = [calendar["day_type"]]
    hierarchies["Service Period"] = [events["service_period"]]
    hierarchies["Hour of Day"] = [events["local_hour"]]

    # `direction_id` is 0/1 with no agency-independent meaning — it is not
    # reliably north/south or inbound/outbound — so it stays a bare id rather
    # than getting a label this project would have to invent per agency.
    hierarchies["Direction"] = [events["direction_id"]]

    # Without this every fact-table hierarchy lands in a dimension named after
    # the table, so the UI would offer "Service Period" and "Hour of Day" under
    # `scheduled_events` rather than next to the calendar.
    for hierarchy in ("Service Period", "Hour of Day"):
        hierarchies[hierarchy].dimension_name = "Time"
    hierarchies["Direction"].dimension_name = "Network"


def _define_measures(cube: tt.Cube, tables: CubeTables) -> None:
    events, calendar = tables.events, tables.calendar
    measures = cube.measures

    # Every row is one scheduled call at one stop, so the contributor count is
    # already the stop count; it is aliased because "contributors.COUNT" means
    # nothing to anyone reading the dashboard.
    #
    # The `* 1` is not decoration. Assigning the measure directly is the
    # documented way to alias one, and on Atoti 0.9.16 it fails inside the
    # server with `No measure named Scheduled Stops` — the copy looks the new
    # name up before registering it. Multiplying builds an ordinary derived
    # measure instead, which takes the same path every other measure here does.
    measures["Scheduled Stops"] = measures["contributors.COUNT"] * 1

    # Distinct rather than a row count: a trip contributes one row per stop it
    # calls at, so counting rows would report a 38-stop local as 38 trips.
    measures["Scheduled Trips"] = tt.agg.count_distinct(events["trip_id"])
    measures["Routes Served"] = tt.agg.count_distinct(events["route_key"])
    measures["Stops Served"] = tt.agg.count_distinct(events["stop_key"])

    # Counted from the calendar dimension, so it stays the number of distinct
    # days in the loaded window at every level of every hierarchy. This is what
    # makes the per-day measures below comparable between a route that runs
    # daily and one that runs on weekends only.
    measures["Service Days"] = tt.agg.count_distinct(calendar["service_date"])

    measures["Trips per Service Day"] = measures["Scheduled Trips"] / measures["Service Days"]
    measures["Stops per Trip"] = measures["Scheduled Stops"] / measures["Scheduled Trips"]

    measures["Mean Dwell Seconds"] = tt.agg.mean(events["dwell_seconds"])

    # Cross-midnight calls are 2.5-3.4% of stop times across these feeds. The
    # share is surfaced as a measure because it is the fastest way to see, in
    # the UI, whether a schema change has quietly moved overnight service onto
    # the wrong service date.
    measures["Overnight Stops"] = tt.agg.sum(events["overnight"])
    measures["Overnight Share"] = measures["Overnight Stops"] / measures["Scheduled Stops"]

    for name in ("Trips per Service Day", "Stops per Trip", "Mean Dwell Seconds"):
        measures[name].formatter = "DOUBLE[#,##0.0]"
    measures["Overnight Share"].formatter = "DOUBLE[0.0%]"


def build(
    root: Path | None = None,
    dates: list[dt.date] | None = None,
    port: int = 0,
    content_storage: Path | None = None,
) -> tt.Session:
    """Start a session, load the dataset, and define the cube on it."""
    session = start_session(port=port, content_storage=content_storage)
    tables = load_tables(session, root=root, dates=dates)
    build_cube(session, tables)
    return session


__all__ = [
    "CUBE_NAME",
    "UNKNOWN",
    "CubeTables",
    "build",
    "build_cube",
    "load_tables",
    "start_session",
]
