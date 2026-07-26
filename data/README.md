# data/

Working directory for feeds and generated datasets. Everything here except this
file is gitignored — no transit data is ever committed.

```
data/
  raw/          Downloaded GTFS Static archives (.zip), one dir per agency
  parquet/      Partitioned output, the shared volume the cube reads
    dim_routes/
    dim_stops/
    dim_calendar/
    scheduled_events/service_date=YYYY-MM-DD/
    stop_events/service_date=YYYY-MM-DD/
```

`data/parquet` is bind-mounted into the ingest, stream, and cube containers so
all three see the same dataset.
