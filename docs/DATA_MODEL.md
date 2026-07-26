# Data model

## Fact table: `stop_events`

One row per vehicle arrival at a stop. Partitioned by `service_date`.

| Column | Type | Notes |
| --- | --- | --- |
| `event_id` | string | Hash of trip + stop + service date |
| `service_date` | date | Partition key |
| `agency_id` | string | |
| `route_id` | string | FK to `routes` |
| `trip_id` | string | |
| `stop_id` | string | FK to `stops` |
| `stop_sequence` | int | Position along the trip |
| `direction_id` | int | 0 / 1 |
| `scheduled_arrival` | timestamp | Resolved from GTFS, midnight-rollover aware |
| `actual_arrival` | timestamp | Nullable — trip may be cancelled |
| `delay_seconds` | int | Negative means early |
| `dwell_seconds` | int | Departure minus arrival |
| `headway_seconds` | int | Gap since previous vehicle on this route/stop/direction |
| `is_cancelled` | bool | |
| `vehicle_id` | string | Nullable |

`scheduled_events` is the same shape restricted to the static feed: every
column that requires realtime data is null. Phase 1 emits it; Phase 3 upgrades
it into `stop_events`.

## Dimension tables

- **`routes`** — `route_id`, `agency_id`, `mode`, short/long name, color
- **`stops`** — `stop_id`, `name`, lat/lon, `parent_station`, `borough`, `municipality`
- **`calendar`** — `service_date`, `day_type`, `is_holiday`, `service_period`

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

### Measures beyond simple sums

- Delay percentiles (p50 / p90 / p99)
- On-time performance against a configurable threshold
- Headway regularity — standard deviation of observed gaps between vehicles
- Schedule-adherence score

## GTFS edge cases the ingest must handle

- **Times past `24:00:00`.** GTFS expresses a trip crossing midnight as
  `25:14:00` on the *previous* service date. Naive parsing silently drops these.
- **Timezone-aware service dates.** The service date is an agency-local concept,
  not a UTC calendar day, and DST transitions make the two diverge.
- **Cancelled and added trips.** Realtime feeds both remove scheduled trips and
  introduce trips absent from the static feed.
- **Stale or malformed feeds.** Feeds go stale, return truncated protobuf, or
  reference `trip_id`s that do not exist in the static archive.
