# syntax=docker/dockerfile:1

FROM node:26-alpine AS frontend
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY frontend/ ./
RUN npm run build

FROM rust:1-bookworm AS backend
WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/migrations ./migrations
COPY backend/src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl passwd \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --create-home --home-dir /home/nimbo nimbo
WORKDIR /app
COPY --from=backend /app/target/release/nimbo /usr/local/bin/nimbo
COPY --from=frontend /app/frontend/build /app/static
RUN mkdir -p /data && chown -R nimbo:nimbo /data /app
USER nimbo
ENV NIMBO_BIND=0.0.0.0:8080 \
    NIMBO_DATA_DIR=/data \
    NIMBO_STATIC_DIR=/app/static
EXPOSE 8080
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1
CMD ["nimbo"]
