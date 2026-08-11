import datetime as dt
import json
from pathlib import Path

import pytest

from transit_cube.calendar import (
    US_HOLIDAYS,
    DayType,
    ServicePeriod,
    UncoveredYearError,
    classify_day_type,
    classify_service_period,
    holidays_for,
    service_date_for,
)


class TestDayType:
    def test_midweek_is_a_weekday(self):
        assert classify_day_type(dt.date(2026, 7, 22)) is DayType.WEEKDAY  # Wednesday

    @pytest.mark.parametrize("day", [dt.date(2026, 7, 25), dt.date(2026, 7, 26)])
    def test_saturday_and_sunday_are_weekend(self, day):
        assert classify_day_type(day) is DayType.WEEKEND

    def test_holiday_on_a_weekday(self):
        assert classify_day_type(dt.date(2026, 12, 25)) is DayType.HOLIDAY  # Friday

    def test_holiday_beats_weekend(self):
        """July 4 2026 is a Saturday. Folding it into 'weekend' hides the
        holiday-vs-normal comparison, which is the point of the level."""
        july_fourth = dt.date(2026, 7, 4)
        assert july_fourth.weekday() == 5
        assert classify_day_type(july_fourth) is DayType.HOLIDAY

    def test_holiday_set_is_injectable(self):
        day = dt.date(2026, 7, 22)
        assert classify_day_type(day, holidays=frozenset({day})) is DayType.HOLIDAY

    def test_the_holiday_set_follows_the_dates_year(self):
        """Christmas is Christmas in every covered year.

        The set used to be a flat 2026 default argument, so a 2027 dataset
        matched nothing at all: Christmas came back Weekday, the Day Type
        hierarchy looked complete, and every holiday comparison was wrong with
        nothing on screen to suggest it.
        """
        assert classify_day_type(dt.date(2027, 12, 25)) is DayType.HOLIDAY
        assert classify_day_type(dt.date(2027, 11, 25)) is DayType.HOLIDAY

    def test_an_uncovered_year_raises_rather_than_reporting_a_weekday(self):
        uncovered = max(US_HOLIDAYS) + 50
        with pytest.raises(UncoveredYearError, match=str(uncovered)):
            classify_day_type(dt.date(uncovered, 12, 25))


class TestHolidayCalendars:
    def test_every_year_holds_only_its_own_dates(self):
        for year, holidays in US_HOLIDAYS.items():
            assert holidays, f"{year} is empty"
            assert all(day.year == year for day in holidays), f"{year} holds a foreign date"

    def test_holidays_for_returns_the_keyed_set(self):
        for year, holidays in US_HOLIDAYS.items():
            assert holidays_for(year) is holidays

    def test_holidays_for_names_the_covered_years(self):
        with pytest.raises(UncoveredYearError, match="1999"):
            holidays_for(1999)


#: The shared table both implementations of the rule are tested against.
SERVICE_PERIODS = Path(__file__).resolve().parents[3] / "contracts" / "service_periods.json"


class TestTheServicePeriodContract:
    """The rule has two implementations, so neither of them is the authority.

    The Rust ingest stamps `service_period` onto every fact row at write time so
    the cube does not have to compute it over millions of rows on every load;
    this classifier stays because it is the readable statement of the rule.
    Both are checked against `contracts/service_periods.json`, and the Rust
    half of this pair lives in `schedule.rs`.
    """

    def test_the_contract_is_checked_in(self):
        assert SERVICE_PERIODS.is_file(), f"{SERVICE_PERIODS} is missing"

    def test_the_classifier_matches_it_for_every_hour(self):
        expected = json.loads(SERVICE_PERIODS.read_text())

        assert len(expected) == 24, "every hour of the day must be stated"
        for hour, period in expected.items():
            assert str(classify_service_period(int(hour))) == period, f"hour {hour}"


class TestServicePeriod:
    @pytest.mark.parametrize(
        ("hour", "expected"),
        [
            (0, ServicePeriod.OVERNIGHT),
            (5, ServicePeriod.OVERNIGHT),
            (6, ServicePeriod.AM_PEAK),
            (9, ServicePeriod.AM_PEAK),
            (10, ServicePeriod.MIDDAY),
            (15, ServicePeriod.MIDDAY),
            (16, ServicePeriod.PM_PEAK),
            (19, ServicePeriod.PM_PEAK),
            (20, ServicePeriod.EVENING),
            (23, ServicePeriod.EVENING),
        ],
    )
    def test_buckets_are_half_open(self, hour, expected):
        assert classify_service_period(hour) is expected

    @pytest.mark.parametrize(
        ("hour", "expected"),
        [
            (24, ServicePeriod.OVERNIGHT),  # midnight, next calendar day
            (25, ServicePeriod.OVERNIGHT),
            (28, ServicePeriod.OVERNIGHT),  # 4 AM -- the max seen in the subway feed
            (30, ServicePeriod.AM_PEAK),  # 6 AM
        ],
    )
    def test_handles_gtfs_hours_past_midnight(self, hour, expected):
        """~3% of MTA stop_times use hours >= 24. Routine input, not an edge case."""
        assert classify_service_period(hour) is expected

    @pytest.mark.parametrize("hour", [-1, 48, 99])
    def test_rejects_out_of_range_hours(self, hour):
        with pytest.raises(ValueError):
            classify_service_period(hour)


class TestServiceDate:
    def test_afternoon_belongs_to_the_same_date(self):
        ts = dt.datetime(2026, 7, 22, 17, 30)
        assert service_date_for(ts) == dt.date(2026, 7, 22)

    def test_after_midnight_belongs_to_the_previous_service_date(self):
        """A 2 AM train is operationally the previous day's service."""
        ts = dt.datetime(2026, 7, 23, 2, 15)
        assert service_date_for(ts) == dt.date(2026, 7, 22)

    def test_rollover_hour_is_the_boundary(self):
        assert service_date_for(dt.datetime(2026, 7, 23, 3, 59)) == dt.date(2026, 7, 22)
        assert service_date_for(dt.datetime(2026, 7, 23, 4, 0)) == dt.date(2026, 7, 23)

    def test_rollover_hour_is_configurable(self):
        ts = dt.datetime(2026, 7, 23, 2, 15)
        assert service_date_for(ts, rollover_hour=0) == dt.date(2026, 7, 23)

    def test_crossing_a_month_boundary(self):
        ts = dt.datetime(2026, 8, 1, 1, 0)
        assert service_date_for(ts) == dt.date(2026, 7, 31)
