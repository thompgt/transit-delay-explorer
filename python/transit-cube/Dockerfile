# Atoti Community Edition runtime.
#
# Containerised rather than run from a local interpreter for one hard reason:
# Atoti CE does not support Python 3.13, and 3.13 is what a current machine
# has. Pinning 3.11 here means the cube runs the same way everywhere without
# anyone managing a second interpreter by hand.
#
# Dependencies are installed by name rather than with `pip install -e .` so the
# layer caches independently of the source. Atoti pulls a bundled JVM and is
# several hundred megabytes; rebuilding it on every source edit would make the
# edit-run loop unusable. Source is mounted at run time instead.

FROM python:3.11-slim

# Atoti's server writes temp files and needs a writable home; the default
# distroless-ish slim image has one, but being explicit avoids a surprise if the
# container is later run as a non-root user.
ENV HOME=/tmp \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONPATH=/app/python/transit-cube/src

WORKDIR /app

# Versions mirror python/transit-cube/pyproject.toml. Kept in step by the
# smoke test, which imports the package and would fail on a mismatch.
RUN pip install --no-cache-dir \
        "atoti>=0.9,<0.10" \
        "atoti-jupyterlab>=0.9,<0.10" \
        "pandas>=2.2" \
        "pyarrow>=17" \
        "pytest>=8" \
        "ruff>=0.6"

# The dashboard. Atoti serves the whole UI from this one port, and binds every
# interface, so publishing it is all that is needed to reach it from the host.
EXPOSE 9090

# Overridden by compose, which runs the cube. This default only answers "is the
# image built and is Atoti importable": `atoti` exposes no `__version__`, so the
# version comes from the installed distribution's metadata instead.
CMD ["python", "-c", "import atoti; from importlib.metadata import version; print('atoti', version('atoti'))"]
