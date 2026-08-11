# One entry point across three toolchains.
#
# Running this project meant a cargo invocation for the ingest, a compose
# invocation for the broker and the cube, and separate pytest and maven ones for
# the tests -- four different working directories and no single place that said
# what the sequence was. Every recipe here is the same command the README
# documents, so neither can quietly stop matching the other.
#
#   just            list the recipes
#   just quickstart broker, feeds, dataset, cube -- from nothing
#
# Requires `just` (https://just.systems). Everything it calls is either cargo,
# docker compose, or a container, so nothing here needs a local Maven or a
# second Python.

compose := "docker compose -f infra/docker-compose.yml"
cube_image := "tde-cube:latest"

# Default recipe: show what is available.
default:
    @just --list

# --------------------------------------------------------------------------
# Data
# --------------------------------------------------------------------------

# Download the static GTFS archives (one agency, or all of them).
fetch agency="":
    cd rust && cargo run --release --bin transit-ingest -- fetch {{ agency }}

# Re-check the archives against the server. A 304 keeps what is on disk.
refetch agency="":
    cd rust && cargo run --release --bin transit-ingest -- fetch {{ agency }} --force

# Write the partitioned Parquet dataset. Defaults to the window every agency
# covers; pass days to narrow it, e.g. `just build "" 7`.
build agency="" days="":
    cd rust && cargo run --release --bin transit-ingest -- build {{ agency }} \
        {{ if days == "" { "" } else { "--days " + days } }}

# Contents and referential integrity of one agency's archive. Exits non-zero on
# a violation, so it works as a gate.
inspect agency:
    cd rust && cargo run --bin transit-ingest -- inspect {{ agency }}

# The configured agency registry.
agencies:
    cd rust && cargo run --bin transit-ingest -- agencies

# --------------------------------------------------------------------------
# Services
# --------------------------------------------------------------------------

# Redpanda, its console, and the topics. Not the cube -- that is behind a profile.
broker:
    {{ compose }} up -d --wait redpanda
    {{ compose }} up topic-init

# Produce and consume one message, end to end.
smoke:
    bash infra/scripts/smoke-test.sh

# The Atoti cube and its dashboard, on http://localhost:9090 (loopback only).
cube:
    {{ compose }} --profile cube up --build cube

# Stop everything and drop the volumes.
down:
    {{ compose }} --profile cube down -v

# --------------------------------------------------------------------------
# Tests
# --------------------------------------------------------------------------

# Everything CI runs, in CI's order.
test: test-rust test-python test-java

test-rust:
    cd rust && cargo fmt --all -- --check
    cd rust && cargo clippy --all-targets --all-features -- -D warnings
    cd rust && cargo test --all-features

# On a bare interpreter the Atoti cube tests skip themselves; `test-cube` runs
# them for real.
test-python:
    cd python/transit-cube && ruff check src tests
    cd python/transit-cube && ruff format --check src tests
    cd python/transit-cube && python -m pytest -q

# The cube tests, in the image the cube actually ships as -- the only place
# Atoti is installed. This is what CI's cube-image job runs.
test-cube:
    docker build -t {{ cube_image }} python/transit-cube
    docker run --rm -v "$PWD:/app:ro" -w /app/python/transit-cube \
        {{ cube_image }} python -m pytest -q -p no:cacheprovider

# Through a container, because this machine has no local Maven.
test-java:
    docker run --rm -v "$PWD:/app" -w /app/java/transit-stream \
        maven:3.9-eclipse-temurin-21 mvn -B verify

# --------------------------------------------------------------------------
# Quickstart
# --------------------------------------------------------------------------

# Nothing to a dashboard: broker, feeds, a week of data, then the cube.
quickstart:
    just broker
    just fetch
    just build "" 7
    just cube
