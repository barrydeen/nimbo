# Nimbo single-image build.
#
# Base image defaults are pinned by multi-arch index digest. The ARG names
# remain overridable for local experiments; production images are built by the
# release workflow, which records the source revision and version. Do not
# replace a digest with an unverified value.

ARG NODE_IMAGE=node:22-alpine@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a
ARG RUST_IMAGE=rust:1-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc

FROM ${NODE_IMAGE} AS frontend
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY frontend/ ./
RUN npm run build

FROM ${RUST_IMAGE} AS backend
WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/vendor ./vendor
COPY backend/migrations ./migrations
COPY backend/src ./src
RUN cargo build --release --locked

# Optional target used by CI to prove the backend test suite runs inside the
# same build graph as the shipped binary:
#   docker build --target backend-test .
FROM backend AS backend-test
RUN cargo test --release --locked

FROM ${RUNTIME_IMAGE} AS runtime

ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED
LABEL org.opencontainers.image.title="Nimbo" \
      org.opencontainers.image.description="Self-hosted markdown knowledge base and file drive" \
      org.opencontainers.image.source="https://github.com/barrydeen/nimbo" \
      org.opencontainers.image.url="https://github.com/barrydeen/nimbo" \
      org.opencontainers.image.documentation="https://github.com/barrydeen/nimbo/blob/main/docs/operations.md" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl passwd \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 nimbo \
    && useradd --system --uid 10001 --gid 10001 --create-home \
        --home-dir /home/nimbo --shell /usr/sbin/nologin nimbo

WORKDIR /app
COPY --from=backend /app/target/release/nimbo /usr/local/bin/nimbo
COPY --from=frontend /app/frontend/build /app/static
RUN install -d -m 0700 -o 10001 -g 10001 /data \
    && chmod -R a+rX /app/static

# The application writes only below /data. The container filesystem can stay
# read-only when /tmp is mounted as tmpfs (see docker-compose.yml).
USER 10001:10001

ENV NIMBO_BIND=0.0.0.0:8080 \
    NIMBO_DATA_DIR=/data \
    NIMBO_STATIC_DIR=/app/static \
    RUST_LOG=nimbo=info,tower_http=info

EXPOSE 8080
VOLUME ["/data"]

STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

CMD ["nimbo"]
