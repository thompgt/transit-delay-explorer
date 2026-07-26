#!/usr/bin/env bash
# Phase 0 acceptance: the broker is up and a message survives a round trip.
# Run from the repo root:  bash infra/scripts/smoke-test.sh
set -euo pipefail

TOPIC="${TOPIC:-transit.smoke}"
PAYLOAD="transit-delay-explorer smoke $(date -u +%Y-%m-%dT%H:%M:%SZ)"

run() { docker exec tde-redpanda rpk "$@"; }

echo "==> cluster health"
run cluster health

echo "==> creating throwaway topic $TOPIC"
run topic create "$TOPIC" --partitions 1 --replicas 1 >/dev/null 2>&1 || true

echo "==> producing"
docker exec -i tde-redpanda rpk topic produce "$TOPIC" <<<"$PAYLOAD"

echo "==> consuming"
GOT=$(run topic consume "$TOPIC" --num 1 --offset start --format '%v')

echo "==> cleaning up"
run topic delete "$TOPIC" >/dev/null

if [[ "$GOT" == "$PAYLOAD" ]]; then
  echo "PASS: round trip returned the produced payload"
else
  echo "FAIL: expected [$PAYLOAD], got [$GOT]" >&2
  exit 1
fi
