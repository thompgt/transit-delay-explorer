# Contracts

Things more than one language has to agree about, kept in one file each and
asserted against from both sides. The pattern is the point: a definition that
exists twice will eventually differ, and the failures it causes are the quiet
kind — a field that binds to null, a boundary hour that lands in two buckets.

## `stop_event.json`

The golden fixture for the `transit.stop_events` topic: one
realtime stop event exactly as the Rust producer will publish it and the Java
consumer parses it.

It exists because the two halves of the project had no shared definition of the
message at all. The Parquet fact table speaks snake_case (`direction_id`,
`is_cancelled`), the Java record's components are camelCase, and nothing pinned
which of the two went on the wire — the only test hand-wrote camelCase, so a
producer following the Parquet vocabulary would have parsed into a record of
nulls and zeroes with no error anywhere.

The wire vocabulary is **snake_case, matching the Parquet columns**, so one
name means one thing across the whole project.

Both sides are tested against this one file, from their own toolchain:

- `rust/transit-ingest/src/dataset/facts.rs` asserts every key here is either a
  column of the `scheduled_events` schema or `service_date`, the partition key
  that lives in the path rather than in the file.
- `java/.../domain/StopEventTest.java` deserializes it into `StopEvent` and
  asserts every component bound, so a renamed or missing field fails the build
  rather than arriving as a null.

Changing a name here is a breaking change to the topic. Change it in this file
first; both test suites will then tell you what else has to move.

## `service_periods.json`

The agency-local hour → service period mapping, for all 24 hours.

The rule now has two implementations. The Rust writer stamps `service_period`
onto every fact row at write time, so the cube can read it off the partition
instead of computing it over millions of rows at load; the Python classifier
stays, because it is the readable statement of the rule and the thing tests
about "what counts as PM peak" should be written against.

Two implementations of one rule is exactly how a dashboard ends up disagreeing
with itself, so neither is allowed to be the authority. Both are tested against
this table:

- `rust/transit-ingest/src/schedule.rs` — `service_period` over 0..23.
- `python/transit-cube/tests/test_calendar.py` — `classify_service_period`.

Moving a boundary means editing this file and watching two suites go red.

