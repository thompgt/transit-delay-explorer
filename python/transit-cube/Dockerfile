# Atoti Community Edition runtime.
#
# Containerised rather than run from a local interpreter for one hard reason:
# Atoti CE does not support Python 3.13, and 3.13 is what a current machine
# has. Pinning 3.11 here means the cube runs the same way everywhere without
# anyone managing a second interpreter by hand.
#
# Dependencies come from pyproject.toml but the project itself is not installed,
# so the layer caches independently of the source. Atoti pulls a bundled JVM and
# is several hundred megabytes; rebuilding it on every source edit would make the
# edit-run loop unusable. Source is mounted at run time instead.
#
# The versions used to be re-listed here by hand, with a comment admitting they
# mirrored pyproject.toml. They drifted from CI's own hand-copied list, and
# nothing would have said so. Now there is one list, read out of pyproject.toml
# at build time.

FROM python:3.11-slim

# Atoti's server writes temp files and needs a writable home; the default
# distroless-ish slim image has one, but being explicit avoids a surprise if the
# container is later run as a non-root user.
ENV HOME=/tmp \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONPATH=/app/python/transit-cube/src

WORKDIR /app

# Only the manifest is copied, so editing the cube does not invalidate the
# Atoti layer. `dev` comes along because this image is also what runs the cube
# tests -- it is the only environment in which the Atoti ones are not skipped.
COPY pyproject.toml /tmp/pyproject.toml

RUN python -c "\
import tomllib; \
p = tomllib.load(open('/tmp/pyproject.toml','rb'))['project']; \
deps = p['dependencies'] + p.get('optional-dependencies', {}).get('dev', []); \
open('/tmp/requirements.txt','w').write('\n'.join(deps) + '\n')" \
    && cat /tmp/requirements.txt \
    && pip install --no-cache-dir -r /tmp/requirements.txt

# The dashboard. Atoti serves the whole UI from this one port, and binds every
# interface, so publishing it is all that is needed to reach it from the host.
EXPOSE 9090

# Overridden by compose, which runs the cube. This default only answers "is the
# image built and is Atoti importable": `atoti` exposes no `__version__`, so the
# version comes from the installed distribution's metadata instead.
CMD ["python", "-c", "import atoti; from importlib.metadata import version; print('atoti', version('atoti'))"]
