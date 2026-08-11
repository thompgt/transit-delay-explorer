"""Day-type and service-period classification.

These two are the *analytical* time hierarchy, deliberately independent of the
calendar hierarchy (year -> month -> date -> hour). Users slice by both and they
do not nest: "PM peak" is a property of the hour, "holiday" a property of the
date, and neither is a parent of the other.

Pure functions with no Atoti dependency, so the rules that decide what counts as
PM peak are testable without standing up a session.
"""

from __future__ import annotations

import datetime as dt
from enum import StrEnum


class DayType(StrEnum):
    """Which service pattern a date falls under."""

    WEEKDAY = "Weekday"
    WEEKEND = "Weekend"
    HOLIDAY = "Holiday"


class ServicePeriod(StrEnum):
    """Coarse time-of-day bucket, in agency-local time."""

    AM_PEAK = "AM Peak"
    MIDDAY = "Midday"
    PM_PEAK = "PM Peak"
    EVENING = "Evening"
    OVERNIGHT = "Overnight"


# Half-open [start_hour, end_hour) in agency-local time. Overnight is the
# wrap-around remainder and is handled separately rather than encoded here.
_PERIOD_BOUNDS: tuple[tuple[int, int, ServicePeriod], ...] = (
    (6, 10, ServicePeriod.AM_PEAK),
    (10, 16, ServicePeriod.MIDDAY),
    (16, 20, ServicePeriod.PM_PEAK),
    (20, 24, ServicePeriod.EVENING),
)


class UncoveredYearError(Exception):
    """The dataset runs in a year no holiday calendar has been entered for."""


#: Holidays on which the MTA runs a reduced schedule, **keyed by year**.
#:
#: Dates rather than a rule engine, because "the holiday schedule" is an
#: operational decision the agency publishes, not something derivable from the
#: calendar — the MTA runs a normal weekday schedule on Columbus Day and
#: Veterans Day, which a federal-holiday rule would get wrong.
#:
#: Keyed by year, and unforgiving about a year that is missing, because the
#: alternative is worse than an error. A single flat 2026 set as the default
#: argument meant a 2027 dataset simply matched nothing: Christmas came back
#: "Weekday", the Day Type hierarchy looked complete, and every holiday-versus-
#: normal comparison in the cube was quietly wrong with nothing to look at.
US_HOLIDAYS: dict[int, frozenset[dt.date]] = {
    2026: frozenset(
        {
            dt.date(2026, 1, 1),  # New Year's Day
            dt.date(2026, 1, 19),  # Martin Luther King Jr. Day
            dt.date(2026, 2, 16),  # Presidents' Day
            dt.date(2026, 5, 25),  # Memorial Day
            dt.date(2026, 6, 19),  # Juneteenth
            dt.date(2026, 7, 3),  # Independence Day (observed)
            dt.date(2026, 7, 4),  # Independence Day
            dt.date(2026, 9, 7),  # Labor Day
            dt.date(2026, 11, 26),  # Thanksgiving
            dt.date(2026, 12, 25),  # Christmas Day
        }
    ),
    2027: frozenset(
        {
            dt.date(2027, 1, 1),  # New Year's Day
            dt.date(2027, 1, 18),  # Martin Luther King Jr. Day
            dt.date(2027, 2, 15),  # Presidents' Day
            dt.date(2027, 5, 31),  # Memorial Day
            dt.date(2027, 6, 18),  # Juneteenth (observed)
            dt.date(2027, 6, 19),  # Juneteenth
            dt.date(2027, 7, 4),  # Independence Day
            dt.date(2027, 7, 5),  # Independence Day (observed)
            dt.date(2027, 9, 6),  # Labor Day
            dt.date(2027, 11, 25),  # Thanksgiving
            dt.date(2027, 12, 24),  # Christmas Day (observed)
            dt.date(2027, 12, 25),  # Christmas Day
        }
    ),
}


def holidays_for(year: int) -> frozenset[dt.date]:
    """The holiday set for `year`.

    :raises UncoveredYearError: for a year nobody has entered dates for. Adding
        one is an edit to ``US_HOLIDAYS``, and it has to be a deliberate one:
        these are published operational decisions, not a formula.
    """
    try:
        return US_HOLIDAYS[year]
    except KeyError:
        covered = ", ".join(str(y) for y in sorted(US_HOLIDAYS))
        raise UncoveredYearError(
            f"no holiday calendar for {year}; covered years are {covered}. "
            f"Add {year} to transit_cube.calendar.US_HOLIDAYS -- treating its holidays "
            f"as ordinary weekdays would silently skew every day-type comparison."
        ) from None


def classify_day_type(
    service_date: dt.date,
    holidays: frozenset[dt.date] | None = None,
) -> DayType:
    """Classify a service date.

    Holiday wins over weekend: a holiday falling on a Saturday is still reported
    as a holiday, because the interesting comparison is holiday-vs-normal
    ridership, and silently folding it into "weekend" hides that.

    `holidays` defaults to the set for the date's own year, so a dataset in an
    uncovered year raises rather than reporting Christmas as a Wednesday.

    :raises UncoveredYearError: when `holidays` is omitted and the date's year
        has no entry.
    """
    if holidays is None:
        holidays = holidays_for(service_date.year)
    if service_date in holidays:
        return DayType.HOLIDAY
    if service_date.weekday() >= 5:  # Saturday=5, Sunday=6
        return DayType.WEEKEND
    return DayType.WEEKDAY


def classify_service_period(hour: int) -> ServicePeriod:
    """Bucket an agency-local hour into a service period.

    Accepts GTFS-style hours past 24 (``25`` means 1 AM on the following
    calendar day, still attributed to the previous service date). Those are
    roughly 3% of all stop times across the MTA feeds, so this is a routine
    input, not a defensive check.

    :raises ValueError: for hours outside ``0..47``.
    """
    if not 0 <= hour <= 47:
        raise ValueError(f"hour {hour} outside the representable GTFS range 0..47")

    normalized = hour % 24
    for start, end, period in _PERIOD_BOUNDS:
        if start <= normalized < end:
            return period
    return ServicePeriod.OVERNIGHT


def service_date_for(local_time: dt.datetime, rollover_hour: int = 4) -> dt.date:
    """Return the service date an agency-local timestamp belongs to.

    A 2 AM train is operationally part of the *previous* day's service, which is
    why GTFS expresses it as ``26:00:00``. Anything before ``rollover_hour`` is
    attributed to the previous calendar date so overnight service does not get
    split across two rows in the cube.
    """
    if local_time.hour < rollover_hour:
        return local_time.date() - dt.timedelta(days=1)
    return local_time.date()
