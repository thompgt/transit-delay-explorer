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
```

## Configuration

[`config/agencies.toml`](config/agencies.toml) is the whole agency registry.
Adding an agency is an entry there — feed URLs, timezone, and the per-agency
quirks — not a code change.

## Status

Built in phases; see [`docs/WORKPLAN.md`](docs/WORKPLAN.md).

- [x] Phase 0 — Foundations: monorepo, broker, topics, verified round trip
- [ ] Phase 1 — Rust static ingest — *in progress: config + error model done*
- [ ] Phase 2 — Atoti cube v1 (static data)
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
