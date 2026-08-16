# syntax=docker/dockerfile:1.7

ARG UV_VERSION=0.11.32
ARG DENO_VERSION=2.9.5

FROM ghcr.io/astral-sh/uv:${UV_VERSION} AS uv
FROM ghcr.io/denoland/deno:bin-${DENO_VERSION} AS deno

FROM python:3.14-slim-bookworm AS python-base

COPY --from=uv /uv /uvx /usr/local/bin/
COPY --from=deno /deno /usr/local/bin/deno

WORKDIR /workspace/apps/ytm-resolver

ENV UV_LINK_MODE=copy \
    UV_PROJECT_ENVIRONMENT=/opt/venv \
    UV_PYTHON_DOWNLOADS=0 \
    DENO_DIR=/var/cache/deno \
    DENO_NO_PROMPT=1 \
    DENO_NO_UPDATE_CHECK=1 \
    PYTHONUNBUFFERED=1

COPY apps/ytm-resolver/pyproject.toml apps/ytm-resolver/uv.lock apps/ytm-resolver/README.md ./

FROM python-base AS dev

RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen

COPY apps/ytm-resolver/app ./app

CMD ["uv", "run", "--frozen", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000", "--reload", "--reload-dir", "/workspace/apps/ytm-resolver/app"]

FROM python-base AS builder

RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev --no-install-project

COPY apps/ytm-resolver/app ./app

FROM python:3.14-slim-bookworm AS runtime

COPY --from=deno /deno /usr/local/bin/deno
COPY --from=builder /opt/venv /opt/venv
COPY --from=builder /workspace/apps/ytm-resolver/app /app/app

RUN useradd --system --no-create-home --shell /usr/sbin/nologin whio \
    && mkdir --parents /var/cache/deno \
    && chown --recursive whio: /var/cache/deno /app

WORKDIR /app

ENV PATH=/opt/venv/bin:$PATH \
    DENO_DIR=/var/cache/deno \
    DENO_NO_PROMPT=1 \
    DENO_NO_UPDATE_CHECK=1 \
    PYTHONUNBUFFERED=1

EXPOSE 8000

USER whio

ENTRYPOINT ["/opt/venv/bin/uvicorn"]
CMD ["app.main:app", "--host", "0.0.0.0", "--port", "8000"]
