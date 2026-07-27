import datetime as dt

import pytest

from transit_cube.calendar import (
    DayType,
    ServicePeriod,
    classify_day_type,
    classify_service_period,
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
