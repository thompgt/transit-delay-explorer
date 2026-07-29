# Transit Delay Explorer

A multi-language analytics platform over public transit data. Rust ingests and
transforms GTFS, Java runs the real-time streaming layer, and Atoti (Python
Community Edition) provides the OLAP cube and dashboard.

```
GTFS Static (zip) ─┐
                   ├──> [Rust: transit-ingest] ──> Kafka topic ──> [Java: transit-stream]
GTFS-RT (protobuf) ┘            │                                          │
                                │                                          │
                                └──────> Parquet (partitioned) <───────────┘
                                              │
                                              ▼
                                    [Python: Atoti cube]
                                              │
                                              ▼
                                    Atoti web dashboard
```

Two paths, deliberately. The **live path** runs through Kafka into the Java
service for sub-minute state. The **historical path** runs through partitioned
Parquet into the cube for deep slicing. They meet in the dashboard.

## Components

| Path | Component | Language | Role |
| --- | --- | --- | --- |
| `rust/transit-ingest` | `transit-ingest` | Rust | GTFS static + realtime ingest, delay computation, Parquet + Kafka output |
| `java/transit-stream` | `transit-stream` | Java 21 | Kafka consumer, windowed aggregates, anomaly detection, REST + SSE |
| `python/transit-cube` | `transit-cube` | Python 3.11 | Atoti cube, hierarchies, measures, what-if scenarios |
| `infra/` | — | — | docker compose, Redpanda, shared Parquet volume |
| `data/` | — | — | Downloaded feeds and generated Parquet (gitignored) |

## Quickstart

```bash
docker compose -f infra/docker-compose.yml up -d   # broker + console + topics
bash infra/scripts/smoke-test.sh                   # produce/consume round trip
```

The Redpanda console is then at <http://localhost:8090>, the Kafka API on
`localhost:19092` from the host and `redpanda:9092` from inside the network.

## Building

Each component builds independently. Only Docker is strictly required — the
Java and Python toolchains can run containerized.

```bash
# Rust
cd rust && cargo test && cargo run --bin transit-ingest -- agencies

# Java (needs Maven, or use the container form below)
cd java/transit-stream && mvn verify
docker run --rm -v "$PWD:/app" -w /app maven:3.9-eclipse-temurin-21 mvn -B verify

# Python — Atoti CE does not support 3.13, so the cube pins 3.11
cd python/transit-cube && pip install -e ".[dev]" && pytest

# ...or containerized, which is the only form that runs the cube tests
docker build -t tde-cube:latest python/transit-cube
docker run --rm -v "$PWD:/app:ro" -w /app/python/transit-cube tde-cube:latest pytest
```

## Configuration

[`config/agencies.toml`](config/agencies.toml) is the whole agency registry.
Adding an agency is an entry there — feed URLs, timezone, and the per-agency
quirks — not a code change.

## Ingesting

`transit-ingest` downloads static GTFS archives to `data/raw/<agency>.zip` and
writes a partitioned Parquet dataset under `data/parquet/`. From nothing to a
service week of all three agencies is two commands:

```bash
cd rust
cargo run --release --bin transit-ingest -- fetch            # download archives
cargo run --release --bin transit-ingest -- build --days 7   # write the dataset

cargo run --bin transit-ingest -- agencies                   # list the registry
cargo run --bin transit-ingest -- inspect MTA_NYCT           # contents + integrity
```

`fetch` keeps an archive it already has — the static feeds change a few times a
year — so pass `--force` to re-download. A download is validated as a zip
holding the required GTFS files before it replaces anything on disk: the MTA
serves an HTML error page with a 200 status when a feed is briefly down, and
letting that land as `MTA_NYCT.zip` turns a network blip into a parse error
days later that names entirely the wrong cause.

`build` takes an optional agency id, `--from` / `--to` for an explicit window,
or `--days N` for N days from the start of the window.

The default window is the **intersection** of what the selected agencies cover,
not the union. The three MTA feeds are published on their own schedules and
their coverage barely overlaps — taking each feed's own first week gives the
Subway late May, Metro-North mid-July and the LIRR the end of July, with not one
day in common. Every cross-agency question the cube exists to answer is
unanswerable on a dataset like that, and nothing looks wrong until you slice by
agency and one of them is empty. `inspect` prints a feed's coverage.

`build` refuses to write a feed with referential integrity violations unless
given `--allow-violations`, since a dataset with dangling keys becomes a cube
with silently missing slices.

The output layout is Hive-style, so Arrow, pandas and Atoti all prune on the
partition key rather than scanning the table:

```
data/parquet/
  scheduled_events/service_date=2026-05-26/MTA_NYCT.parquet
  routes/MTA_NYCT.parquet
  stops/MTA_NYCT.parquet
```

One file per agency per date: the three feeds are re-ingested independently, so
rebuilding the Subway must not rewrite a file that also holds LIRR rows.

## The cube

Once a dataset exists, one command serves it:

```bash
docker compose -f infra/docker-compose.yml --profile cube up --build cube
```

The dashboard is then at <http://localhost:9090>. It is behind a profile
because it is not part of the broker stack — `up -d` on its own still starts
only Redpanda. The repo is mounted read-only, so changing a measure is an edit
and a restart rather than an image rebuild; saved dashboards live in a volume
and survive one.

Set `TDE_CUBE_DATES` to a comma-separated list of `YYYY-MM-DD` service dates to
load only those. The load prunes at the partition level, so on a full window
that is the difference between seconds and minutes.

Every measure is a **schedule** measure — scheduled stops and trips, trips per
service day, stops per trip, mean dwell, and the overnight share. The realtime
columns exist in the fact table and are typed, but stay null until Phase 3
fills them; a p90 of nothing does not belong in a dashboard. They arrive in
Phase 5 against this same schema, which is the whole reason the cube is built
this early: an awkward hierarchy costs an edit now and a rewrite later.

Two structural facts about the model, both forced rather than chosen:

- **Every join key is namespaced.** `route_id` `1` names three unrelated
  railroads across the MTA's feeds, so joins are on `{agency_id}:{id}`.
- **A hierarchy cannot span tables.** Atoti builds each from one table's
  columns, which splits calendar time in two: the date levels belong to the
  calendar dimension and the hour to the fact row. They are exposed as
  `Calendar` and `Hour of Day` rather than faked into one hierarchy.

## Status

Built in phases; see [`docs/WORKPLAN.md`](docs/WORKPLAN.md).

- [x] Phase 0 — Foundations: monorepo, broker, topics, verified round trip
- [x] Phase 1 — Rust static ingest: fetch, parse, validate, resolve, and write a
  partitioned Parquet dataset for a service week
- [x] Phase 2 — Atoti cube v1: the schema, hierarchies and schedule measures,
  on static data and before any streaming is built on top of them
- [ ] Phase 3 — Rust realtime ingest
- [ ] Phase 4 — Java streaming service
- [ ] Phase 5 — Cube v2, the real measures
- [ ] Phase 6 — Polish, synthetic generator, load test

## Data source

Targets the free MTA feeds — Metro-North, LIRR, and NYC Subway — across several
transit modes. The system is agency-agnostic: adding an agency is a config
entry, not a code change.

The real archives were downloaded and read before any parsing code was written.
[`docs/FEED_NOTES.md`](docs/FEED_NOTES.md) records what they actually contain,
including the findings that changed the schema — `route_id` collides across all
three agencies, two of them ship no `calendar.txt` at all, and times past
`24:00:00` are ~3% of all service rather than a rare edge case.

## Licensing note

No paid licenses anywhere in the stack. Atoti Community Edition's free license
expires periodically and requires updating to a current release, and it reports
telemetry — this project is not intended to run untouched for months.

## License

MIT — see [LICENSE](LICENSE).
