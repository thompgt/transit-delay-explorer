#!/usr/bin/env bash
# Creates the topics the pipeline needs. Idempotent — an existing topic is not
# an error, so `docker compose up` can be re-run freely.
set -uo pipefail

BROKER="${BROKER:-redpanda:9092}"

# Partitioned by route_id so the Java service's per-route windowed aggregates
# stay on one consumer instance and need no cross-partition coordination.
create() {
  local topic="$1" partitions="$2" retention_ms="$3"
  if rpk topic create "$topic" \
      --brokers "$BROKER" \
      --partitions "$partitions" \
      --replicas 1 \
      --topic-config "retention.ms=$retention_ms" \
      --topic-config "compression.type=zstd" 2>&1 | tee /tmp/out; then
    echo "created: $topic"
  elif grep -qi "already exists" /tmp/out; then
    echo "exists:  $topic"
  else
    echo "FAILED:  $topic" >&2
    return 1
  fi
}

# 7 days of stop events; alerts are low-volume and kept a day.
create transit.stop_events 6 604800000
create transit.alerts      1 86400000
create transit.ingest_health 1 86400000

echo
rpk topic list --brokers "$BROKER"
