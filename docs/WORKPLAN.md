# Workplan

Estimates assume steady part-time work. The sequencing matters more than the
numbers.

## Phase 0 — Foundations (week 1)

- Monorepo layout: `rust/`, `java/`, `python/`, `infra/`, `data/`.
- docker compose with Redpanda (lighter than Kafka) and a shared volume for Parquet.
- Pick one agency and download one static GTFS archive by hand. Look at the CSVs.
  Do not skip this; the format has surprises and reading it first saves days.

**Done when:** `docker compose up` starts a broker and you can produce/consume a
test message.

## Phase 1 — Rust static ingest (weeks 2–3)

- `transit-ingest` crate: GTFS zip download, CSV parsing with `csv` + `serde`.
- Referential integrity validation with a clear error type per violation.
- Resolve `stop_times` into absolute timestamps, handling times past `24:00:00`
  and the agency timezone correctly.
- Write dimension tables and a scheduled-events table to Parquet via `arrow`/`parquet`.

**Done when:** one command produces a valid Parquet dataset for a full service
week, with tests covering the midnight-rollover case.

## Phase 2 — Atoti cube v1 (week 4)

Build the cube early, on static data only. It will tell you whether the schema is
right before streaming gets built on top of a bad one.

- Load Parquet into Atoti, define the three hierarchies.
- Simple measures first: trip counts, scheduled service frequency.
- Explore in the dashboard. Fix the schema based on what is awkward to slice.

**Done when:** "how many scheduled trips per route per hour by day type" is
answerable in the UI without writing code.

## Phase 3 — Rust realtime ingest (weeks 5–6)

- GTFS-RT polling with `prost` for protobuf decode.
- Diff consecutive polls; emit only changed stop events.
- Join against the static schedule to compute `delay_seconds`.
- Kafka producer with a defined message schema.
- Handle stale feeds, malformed payloads, and cancelled/added trips.

**Done when:** the ingester runs 24 hours unattended without crashing and
without duplicate events.

## Phase 4 — Java streaming service (weeks 7–9)

The biggest phase. Split it:

- **4a.** Kafka consumer + domain model + Parquet writer for completed windows.
- **4b.** Windowed aggregation. Ring buffers per route/stop; percentiles via a
  t-digest or similar sketch rather than storing every value.
- **4c.** REST API on virtual threads + SSE endpoint for live push.
- **4d.** Anomaly detection against historical baselines.

**Done when:** `/api/routes/{id}/current` returns live delay stats and a
deliberately injected delay spike triggers an alert.

## Phase 5 — Cube v2, the real measures (weeks 10–11)

- Load the fact table with actual delays.
- Percentile measures, on-time performance, headway regularity, adherence score.
- Scenario branching for what-if analysis.

**Done when:** p90 delay is comparable across boroughs for PM peak weekdays and a
scenario branch changes it.

## Phase 6 — Polish (week 12)

- Synthetic data generator so the project runs offline.
- README with architecture diagram and a 3-command quickstart.
- Screenshots or a short screen recording. For a portfolio piece this matters as
  much as the code — most people evaluating it will not run it.
- Load test: how many events/second before something falls over? Write down the
  number and the bottleneck.

## Risks

**GTFS is messier than it looks.** Agencies interpret the spec loosely. Realtime
feeds reference trips that do not exist in the static feed. Budget real time for
data-quality work; it is most of the actual effort in any transit project.

**Atoti CE licensing.** The free license expires periodically and requires
updating to a current release. Do not build anything that must run untouched for
months. Community Edition also reports telemetry.

**Percentiles in streaming are not trivial.** Exact percentiles need all the
data. Use a sketch (t-digest, HdrHistogram) and understand the accuracy
tradeoff.

**Scope creep toward a map UI.** Resist. The Atoti dashboard is the deliverable.
