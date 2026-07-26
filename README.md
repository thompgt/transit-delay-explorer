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
docker compose -f infra/docker-compose.yml up -d
```

Full setup, agency configuration, and per-component docs land as the phases
below complete.

## Status

Built in phases; see [`docs/WORKPLAN.md`](docs/WORKPLAN.md).

- [x] Phase 0 — Foundations: monorepo, broker, shared volume
- [ ] Phase 1 — Rust static ingest
- [ ] Phase 2 — Atoti cube v1 (static data)
- [ ] Phase 3 — Rust realtime ingest
- [ ] Phase 4 — Java streaming service
- [ ] Phase 5 — Cube v2, the real measures
- [ ] Phase 6 — Polish, synthetic generator, load test

## Data source

Targets the free MTA feeds — Metro-North, LIRR, and NYC Subway — across several
transit modes. The system is agency-agnostic: adding an agency is a config
entry, not a code change.

## Licensing note

No paid licenses anywhere in the stack. Atoti Community Edition's free license
expires periodically and requires updating to a current release, and it reports
telemetry — this project is not intended to run untouched for months.

## License

MIT — see [LICENSE](LICENSE).
