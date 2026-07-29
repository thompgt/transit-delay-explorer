"""Run the cube and serve its dashboard until interrupted.

`python -m transit_cube.serve` is what the `cube` service in
`infra/docker-compose.yml` runs. Everything it needs comes from the
environment, because the container is the only caller and compose is where its
configuration already lives:

- `TDE_DATA_DIR` — the repo root's `data/`, mounted read-only. The Parquet is
  expected at `<TDE_DATA_DIR>/parquet`. Unset, the dataset is found by walking
  up from the package, which is what a local run wants.
- `TDE_CUBE_PORT` — the published port. Fixed rather than ephemeral, because a
  dashboard whose URL changes on every restart cannot be bookmarked.
- `TDE_CUBE_CONTENT` — where the server keeps saved dashboards. It must be a
  writable path that outlives the container; compose gives it a volume.
- `TDE_CUBE_DATES` — an optional comma-separated list of `YYYY-MM-DD` service
  dates. The load prunes at the partition level, so this is the difference
  between a few seconds and a few minutes when only one day is of interest.

There is no host setting. Atoti's `SessionConfig` has no such field — the
server binds every interface — so publishing the port is all that is needed to
reach it from outside the container.
"""

from __future__ import annotations

import datetime as dt
import os
import signal
import sys
from pathlib import Path

from transit_cube.cube import CUBE_NAME, build
from transit_cube.dataset import DatasetError

DEFAULT_PORT = 9090


class ServeError(Exception):
    """The environment does not describe a runnable session."""


def parquet_root_from_env(environ: dict[str, str] | None = None) -> Path | None:
    """The dataset root, or None to let `transit_cube.dataset` find it."""
    environ = os.environ if environ is None else environ

    raw = environ.get("TDE_DATA_DIR")
    if not raw:
        return None

    root = Path(raw) / "parquet"
    if not root.is_dir():
        raise ServeError(f"TDE_DATA_DIR is {raw!r}, but {root} does not exist")
    return root


def dates_from_env(environ: dict[str, str] | None = None) -> list[dt.date] | None:
    """The service dates to load, or None for every date in the dataset."""
    environ = os.environ if environ is None else environ

    raw = environ.get("TDE_CUBE_DATES", "").strip()
    if not raw:
        return None

    dates = []
    for piece in raw.split(","):
        try:
            dates.append(dt.date.fromisoformat(piece.strip()))
        except ValueError as error:
            raise ServeError(f"TDE_CUBE_DATES contains {piece.strip()!r}, not a date") from error
    return dates


def port_from_env(environ: dict[str, str] | None = None) -> int:
    environ = os.environ if environ is None else environ

    raw = environ.get("TDE_CUBE_PORT", str(DEFAULT_PORT)).strip()
    try:
        return int(raw)
    except ValueError as error:
        raise ServeError(f"TDE_CUBE_PORT is {raw!r}, not a port number") from error


def content_storage_from_env(environ: dict[str, str] | None = None) -> Path | None:
    """Where dashboards are saved, or None to keep them in memory.

    Created if missing rather than required to exist: compose mounts an empty
    volume there, and failing on the first start of a fresh checkout would be a
    poor introduction to the one command this project's deliverable needs.
    """
    environ = os.environ if environ is None else environ

    raw = environ.get("TDE_CUBE_CONTENT")
    if not raw:
        return None

    path = Path(raw)
    try:
        path.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        raise ServeError(f"TDE_CUBE_CONTENT is {raw!r}, which is not writable") from error
    return path


def main(argv: list[str] | None = None) -> int:
    del argv  # Configured by environment; compose is the only caller.

    try:
        root = parquet_root_from_env()
        dates = dates_from_env()
        port = port_from_env()
        content = content_storage_from_env()
    except ServeError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    try:
        session = build(root=root, dates=dates, port=port, content_storage=content)
    except DatasetError as error:
        # The common first-run failure by a wide margin: the cube is started
        # before `transit-ingest build` has written anything for it to read.
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(f"cube {CUBE_NAME!r} is serving on http://localhost:{session.port}", flush=True)

    # Atoti's server runs on its own non-daemon threads, so the session stays up
    # on its own; this process only has to stay alive and not spin. `signal.pause`
    # is unavailable on Windows, where a local run falls back to blocking on
    # stdin instead.
    try:
        if hasattr(signal, "pause"):
            signal.pause()
        else:  # pragma: no cover - Windows only
            sys.stdin.read()
    except KeyboardInterrupt:
        print("shutting down", flush=True)
    finally:
        session.close()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
