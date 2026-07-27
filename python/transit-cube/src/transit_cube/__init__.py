"""Atoti cube for Transit Delay Explorer.

The cube is the analytics surface over the Parquet the Rust ingest and Java
stream layer write. Modules:

- :mod:`transit_cube.calendar` — day-type and service-period classification.
  Pure functions, no Atoti dependency, so the logic that decides what counts as
  "PM peak" is unit-testable without standing up a session.
"""

__version__ = "0.1.0"
