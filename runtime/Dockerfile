FROM rust:1.98-bookworm AS builder
ENV CARGO_BUILD_JOBS=2
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git cargo build --locked --release --bins && mkdir /artifacts && cp target/release/control-plane target/release/gateway target/release/worker target/release/worker-bundle /artifacts/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /var/lib/kurbis/object-store && chown 65532:65532 /var/lib/kurbis/object-store
COPY --from=builder /artifacts/control-plane /usr/local/bin/control-plane
COPY --from=builder /artifacts/gateway /usr/local/bin/gateway
COPY --from=builder /artifacts/worker /usr/local/bin/worker
COPY --from=builder /artifacts/worker-bundle /usr/local/bin/worker-bundle
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/control-plane"]
