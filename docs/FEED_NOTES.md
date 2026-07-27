# Feed notes — what the real MTA archives actually contain

Measured against the live feeds on 2026-07-26. This is the output of the Phase 0
"download one archive by hand and look at the CSVs" step. Several findings
changed the data model before any Rust was written, which was the point.

| | Subway | LIRR | Metro-North |
| --- | --- | --- | --- |
| routes | 29 | 13 | 6 |
| stops | 1,488 | 127 | 114 |
| trips | 20,309 | 2,615 | 34,969 |
| stop_times | 563,533 | 26,086 | 466,176 |
| `calendar.txt` | 3 rows | **absent** | **absent** |
| `calendar_dates.txt` | 4 rows | 828 rows | 2,026 rows |
| distinct `service_id` | 3 | 57 | 517 |

Static referential integrity is **clean** across all three feeds: no dangling
`trip_id`, `stop_id`, `route_id`, or `service_id`, and no trips without
stop_times. The validator still has to exist — realtime is where this breaks —
but the static path should not expect to reject these archives.

## Findings that changed the design

### 1. `route_id` and `stop_id` are not globally unique

`route_id` values `1`–`7` exist in **all three** agencies. Subway route `1` is
the Broadway–7th Ave Local; LIRR route `1` is the Babylon Branch; MNR route `1`
is a Hudson Line route. 104 `stop_id` values are likewise shared.

Every key in the warehouse is therefore **composite: `(agency_id, route_id)`**
and `(agency_id, stop_id)`. Loading these three feeds into a cube keyed on bare
`route_id` silently merges three unrelated railroads into one line. The fact
table carries `agency_id` on every row for exactly this reason, and the ingest
emits a namespaced surrogate (`MTA_LIRR:1`) as the join key.

### 2. Two agencies have no `calendar.txt` at all

LIRR and Metro-North express service exclusively through `calendar_dates.txt`
with `exception_type=1` (service added). The spec's data model assumed
`calendar.txt`. The calendar resolver must support three shapes:

- `calendar.txt` only (weekly pattern)
- `calendar_dates.txt` only (explicit date list) — LIRR, MNR
- both, with `calendar_dates` overriding — Subway (`Saturday` added on the
  July 3rd holiday, `Weekday` removed the same day)

MNR's 517 distinct `service_id` values are a direct consequence: one service id
per distinct operating day rather than a handful of reusable patterns.

### 3. Times past `24:00:00` are routine, not an edge case

| Feed | stop_times past 24:00 | share | max hour |
| --- | --- | --- | --- |
| Subway | 18,856 | 3.35% | 28 |
| LIRR | 817 | 3.13% | 25 |
| MNR | 11,742 | 2.52% | 26 |

Hour 28 means `28:xx:xx` — 4 AM on the following calendar day, still attributed
to the previous service date. Naive `HH:MM:SS` parsing does not merely lose an
edge case, it drops ~3% of all service and skews every overnight metric to zero.
This is a first-class code path with tests, not a guard clause.

### 4. Only the Subway has a station/platform structure

| Feed | `location_type` | stops with `parent_station` |
| --- | --- | --- |
| Subway | 496 stations (`1`), 992 platforms (blank) | 992 |
| LIRR | all blank | 0 |
| MNR | all `0` | 0 |

The geography hierarchy is **ragged**: Subway has borough → municipality →
station → platform, while LIRR and MNR terminate at the station. The cube must
synthesize a platform level for the flat agencies (platform = station) rather
than leave nulls, or the hierarchy breaks when slicing across agencies.

Note also that `location_type` blank and `location_type=0` both mean "stop" —
Subway uses the former, MNR the latter. Deserialization must treat missing as 0.

### 5. Column sets differ per agency

LIRR's `routes.txt` has neither `agency_id` nor `route_short_name`:

```
"route_id","route_long_name","route_type","route_color","route_text_color"
```

Subway and MNR both carry the full set, and MNR's `route_short_name` is present
but empty for every row. LIRR also quotes every field while the other two quote
nothing. Consequences for the Rust structs: optional fields are `Option<T>` with
`#[serde(default)]`, `agency_id` is backfilled from `agency.txt` when the column
is absent, and display name falls back `short_name → long_name → route_id`.

Non-standard columns appear too — LIRR `trips.peak_offpeak`, MNR
`stop_times.track` and `stop_times.note_id`, plus an MNR `notes.txt` that is not
in the GTFS spec. The CSV reader must ignore unknown columns rather than fail.

### 6. NYCT encodes direction in the `stop_id`, in the static feed

`stops.txt` carries `101` (station), `101N`, and `101S` (platforms), and
`stop_times.txt` references `101S` directly. So the direction suffix is not just
a realtime quirk as the agency config initially assumed — it is how the static
schedule is expressed. The parent station is the id with the suffix stripped,
and it is already present as a real stop row.

### 7. No borough or municipality anywhere

Neither field exists in any feed; `stops.txt` gives only lat/lon. The
`borough` / `municipality` columns in the stops dimension are **derived**, not
ingested — a point-in-polygon join against boundary geometry, computed once at
load. Until that lands they are populated as `Unknown` rather than dropped, so
the hierarchy shape stays stable.

## Realtime endpoints

All three respond `HTTP 200` with **no API key** (the MTA dropped the key
requirement in 2023):

| Feed | bytes | `Content-Type` |
| --- | --- | --- |
| `nyct%2Fgtfs-ace` | 69,838 | `text/plain` |
| `lirr%2Fgtfs-lirr` | 47,219 | `application/octet-stream` |
| `mnr%2Fgtfs-mnr` | 95,450 | `application/x-protobuf` |

Three different content types for the same protobuf payload — so the poller must
**not** validate on `Content-Type`. Decode and let `prost` reject malformed
bodies.

## Reproducing

```bash
cargo run -p transit-ingest -- fetch --agency MTA_NYCT
```

The archives land in `data/raw/` and are gitignored.
