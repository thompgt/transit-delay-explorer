# Data model

## Fact table: `stop_events`

One row per vehicle arrival at a stop. Partitioned by `service_date`.

| Column | Type | Notes |
| --- | --- | --- |
| `event_id` | string | Hash of agency + trip + `stop_sequence` + service date |
| `service_date` | date | **Partition key — in the path, not in the file** |
| `agency_id` | string | Part of every FK — see composite keys below |
| `route_id` | string | FK to `routes`, only unique within agency |
| `route_key` | string | `{agency_id}:{route_id}` — the actual join key |
| `trip_id` | string | |
| `stop_id` | string | FK to `stops`, only unique within agency |
| `stop_key` | string | `{agency_id}:{stop_id}` — the actual join key |
| `stop_sequence` | int | Position along the trip |
| `direction_id` | int | 0 / 1 |
| `scheduled_arrival` | timestamp | Resolved from GTFS, midnight-rollover aware |
| `actual_arrival` | timestamp | Nullable — trip may be cancelled |
| `delay_seconds` | int | Negative means early |
| `dwell_seconds` | int | Departure minus arrival |
| `headway_seconds` | int | Gap since previous vehicle on this route/stop/direction |
| `is_cancelled` | bool | |
| `schedule_relationship` | string | Nullable — the GTFS-RT spec name: `SCHEDULED`, `ADDED`, `UNSCHEDULED`, `CANCELED`, `DUPLICATED`, `DELETED`, `SKIPPED`, `NO_DATA`. Only `SCHEDULED` yields a usable delay: an `ADDED` trip has no static schedule to be late against, and `is_cancelled` alone cannot say so |
| `vehicle_id` | string | Nullable |

`scheduled_events` is the same shape restricted to the static feed: every
column that requires realtime data is null. Phase 1 emits it; Phase 3 upgrades
it into `stop_events`. The realtime columns are written as typed nulls rather
than omitted, so the cube built on `scheduled_events` in Phase 2 needs no
schema rework when Phase 5 swaps in the real facts.

### Partitioning

The layout is Hive-style, one file per agency per date:

```
data/parquet/scheduled_events/service_date=2026-05-26/MTA_NYCT.parquet
```

`service_date` appears in the **path only**. Writing it in both the path and
the file is not harmless redundancy — the path supplies a string and the file a
`date32`, and every partition-aware reader then fails on the whole dataset with
`Field service_date has incompatible types: date32[day] vs string`. Readers get
it back as a real date by declaring the partition schema, which
`transit_cube.dataset` does.

One file per agency rather than one merged file, because the three feeds are
downloaded and re-ingested independently: rebuilding the Subway must not rewrite
a file that also holds LIRR rows.

## Composite keys

`route_id` and `stop_id` are unique only *within* an agency. Route `1` exists in
all three MTA feeds as three unrelated lines, and 104 `stop_id` values are shared
across agencies — see [FEED_NOTES](FEED_NOTES.md#1-route_id-and-stop_id-are-not-globally-unique).

So every dimension carries a namespaced surrogate key, `{agency_id}:{id}`, and
that is what the cube joins on. The natural id is kept alongside it for display.

## Dimension tables

- **`routes`** — `route_key`, `route_id`, `agency_id`, `mode`, short/long name, color
- **`stops`** — `stop_key`, `stop_id`, `agency_id`, `name`, lat/lon,
  `parent_station_key`, `borough`, `municipality`
- **`calendar`** — `service_date`, `day_type`, `is_holiday`, `year`, `month`,
  `day_of_week`. Built from the service dates the facts actually contain rather
  than from a date range, so the dimension cannot disagree with the fact table
  about which days exist.

`service_period` is deliberately **not** in the calendar dimension. It is a
property of the time of day, not of the date, so it cannot be keyed by
`service_date`; it is derived onto the fact rows from the agency-local hour of
`scheduled_arrival`. `local_hour` is derived alongside it, for the same reason —
facts are stored in UTC and slicing on the UTC hour would put a New York morning
rush-hour train in the middle of the night.

`borough` and `municipality` exist in no GTFS feed. They are derived from
lat/lon by point-in-polygon against boundary geometry at load time, and default
to `Unknown` rather than null so the hierarchy shape stays stable.

## Cube design notes

Hierarchies are where cube projects usually go wrong. Three rules for this one:

1. **Never make a hierarchy out of something with unbounded cardinality.**
   `trip_id` is a fact attribute, not a level.
2. **Model `parent_station` properly.** In GTFS a "station" is a stop whose
   children are platforms. Flattening this loses the ability to compare
   platform-level performance within a station, which is one of the more
   interesting questions.
3. **Time gets two independent hierarchies** — calendar time
   (year → month → date → hour → 15-minute bucket) and analytical time (day
   type, service period). Users slice by both and they do not nest.

### Hierarchies

| Hierarchy | Levels |
| --- | --- |
| Geography | borough → municipality → station → platform |
| Network | agency → mode → line → route → direction |
| Calendar time | year → month → date → hour → 15-minute bucket |
| Day type | weekday / weekend / holiday |
| Service period | AM peak / midday / PM peak / evening / overnight |

The geography hierarchy is **ragged**. Only the Subway models stations as
parents of platforms (496 stations, 992 platforms); LIRR and MNR are flat. For
flat agencies the ingest emits platform = station rather than null, so slicing
across agencies does not break the level.

### Measures beyond simple sums

- Delay percentiles (p50 / p90 / p99)
- On-time performance against a configurable threshold
- Headway regularity — standard deviation of observed gaps between vehicles
- Schedule-adherence score

## GTFS edge cases the ingest must handle

- **Times past `24:00:00`.** GTFS expresses a trip crossing midnight as
  `25:14:00` on the *previous* service date. Measured at 2.5–3.4% of all
  stop_times across the three MTA feeds, with hours as high as 28 — routine
  volume, not a rare edge case.
- **Missing `calendar.txt`.** LIRR and MNR ship `calendar_dates.txt` only. The
  resolver supports calendar-only, dates-only, and both-with-override.
- **Per-agency column sets.** LIRR's `routes.txt` has no `agency_id` and no
  `route_short_name` column at all. Optional fields are `Option<T>` and unknown
  columns are ignored rather than fatal.
- **Timezone-aware service dates.** The service date is an agency-local concept,
  not a UTC calendar day, and DST transitions make the two diverge.
- **Cancelled and added trips.** Realtime feeds both remove scheduled trips and
  introduce trips absent from the static feed.
- **Stale or malformed feeds.** Feeds go stale, return truncated protobuf, or
  reference `trip_id`s that do not exist in the static archive.
