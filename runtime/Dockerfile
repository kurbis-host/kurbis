FROM docker:28-cli AS docker-cli

FROM rust:1.98-bookworm AS builder
ARG BUILD_REVISION=development
ENV KURBIS_BUILD_REVISION=${BUILD_REVISION}
ENV CARGO_BUILD_JOBS=2
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY runtime/pumpkin ./runtime/pumpkin
COPY plugins/kurbis-control ./plugins/kurbis-control
COPY plugins/kurbis-control /opt/kurbis/plugins/kurbis-control
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git cargo build --jobs 1 --locked --release --bins && mkdir /artifacts && cp target/release/control-plane target/release/gateway target/release/worker target/release/worker-bundle target/release/runtime-api-proxy /artifacts/

FROM debian:bookworm-slim
ARG BUILD_REVISION=development
LABEL org.opencontainers.image.revision=${BUILD_REVISION}
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl iptables openssh-client && rm -rf /var/lib/apt/lists/*
# Keep the fixed runtime identity resolvable. OpenSSH calls getpwuid(3) before
# it starts a connection, so a numeric-only USER 65532:65532 fails before the
# remote CI builder can receive the memory probe. The explicit entries also
# make HOME and file ownership deterministic across control-plane and worker
# containers without granting the runtime any root capability.
RUN groupadd --gid 65532 kurbis \
    && useradd --uid 65532 --gid 65532 --home-dir /home/kurbis --create-home --shell /bin/sh kurbis \
    && test "$(id -u kurbis)" = 65532 \
    && test "$(id -g kurbis)" = 65532 \
    && test "$(getent passwd 65532 | cut -d: -f1)" = kurbis \
    && test "$(getent group 65532 | cut -d: -f1)" = kurbis \
    && test "$(getent passwd 65532 | cut -d: -f6)" = /home/kurbis
RUN mkdir -p /var/lib/kurbis/object-store /home/kurbis/.ssh /home/kurbis/.cache/kurbis/build-cache /opt/kurbis/plugins /opt/kurbis/runtime && chmod 700 /home/kurbis/.cache/kurbis/build-cache && chown -R 65532:65532 /var/lib/kurbis/object-store /home/kurbis
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
