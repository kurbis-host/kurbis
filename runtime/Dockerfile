FROM docker:28-cli AS docker-cli

FROM rust:1.98-bookworm AS builder
ENV CARGO_BUILD_JOBS=2
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY runtime/pumpkin ./runtime/pumpkin
COPY plugins/kurbis-control ./plugins/kurbis-control
COPY plugins/kurbis-control /opt/kurbis/plugins/kurbis-control
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git --mount=type=cache,target=/source/target cargo build --locked --release --bins && mkdir /artifacts && cp target/release/control-plane target/release/gateway target/release/worker target/release/worker-bundle target/release/runtime-api-proxy /artifacts/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl openssh-client && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /var/lib/kurbis/object-store /home/kurbis/.ssh /opt/kurbis/plugins /opt/kurbis/runtime && chown -R 65532:65532 /var/lib/kurbis/object-store /home/kurbis
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
ENV HOME=/home/kurbis
COPY --from=builder /artifacts/control-plane /usr/local/bin/control-plane
COPY --from=builder /artifacts/gateway /usr/local/bin/gateway
COPY --from=builder /artifacts/worker /usr/local/bin/worker
COPY --from=builder /artifacts/worker-bundle /usr/local/bin/worker-bundle
COPY --from=builder /artifacts/runtime-api-proxy /usr/local/bin/runtime-api-proxy
COPY --from=builder /source/runtime/pumpkin /opt/kurbis/runtime/pumpkin
COPY --from=builder /source/plugins/kurbis-control /opt/kurbis/plugins/kurbis-control
WORKDIR /opt/kurbis
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/control-plane"]
