FROM rust:1.98-bookworm AS builder
ARG BUILD_REVISION=development
RUN apt-get update && apt-get install -y --no-install-recommends clang cmake pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY runtime/pumpkin/REVISION /tmp/REVISION
RUN git init . && git remote add origin https://github.com/Pumpkin-MC/Pumpkin.git && git fetch --depth=1 origin "$(cat /tmp/REVISION)" && git checkout --detach FETCH_HEAD && git submodule update --init --recursive --depth=1
COPY runtime/pumpkin/host-control.patch /tmp/host-control.patch
COPY runtime/pumpkin/snapshot-barrier.patch /tmp/snapshot-barrier.patch
COPY runtime/pumpkin/nethernet-gateway.patch /tmp/nethernet-gateway.patch
RUN git apply --check /tmp/host-control.patch \
    && git apply /tmp/host-control.patch \
    && git apply --check /tmp/snapshot-barrier.patch \
    && git apply /tmp/snapshot-barrier.patch \
    && git apply --unidiff-zero --check /tmp/nethernet-gateway.patch \
    && git apply --unidiff-zero /tmp/nethernet-gateway.patch
COPY runtime/pumpkin/host_control.rs crates/pumpkin-core/src/net/host_control.rs
ENV CARGO_BUILD_JOBS=1 CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 CARGO_PROFILE_RELEASE_LTO=false RUSTUP_TOOLCHAIN=1.98.1
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=kurbis-pumpkin-target-${BUILD_REVISION},target=/source/target,sharing=locked \
    cargo build --jobs 1 --locked --release --package pumpkin --bin pumpkin && mkdir /artifacts && cp target/release/pumpkin /artifacts/pumpkin && strip /artifacts/pumpkin

COPY plugins/kurbis-control/Cargo.toml plugins/kurbis-control/Cargo.lock /source/plugins/kurbis-control/
COPY plugins/kurbis-control/src /source/plugins/kurbis-control/src
RUN rustup target add wasm32-wasip2
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=kurbis-pumpkin-plugin-target-${BUILD_REVISION},target=/source/plugins/kurbis-control/target,sharing=locked \
    cargo build --jobs 1 --locked --release --target wasm32-wasip2 --manifest-path plugins/kurbis-control/Cargo.toml && mkdir -p /artifacts && cp plugins/kurbis-control/target/wasm32-wasip2/release/kurbis_control.wasm /artifacts/kurbis-control.wasm

FROM debian:bookworm-slim AS host-control
ARG BUILD_REVISION=development
LABEL org.opencontainers.image.revision=${BUILD_REVISION}
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 libgcc-s1 && rm -rf /var/lib/apt/lists/* && groupadd --gid 2613 pumpkin && useradd --uid 2613 --gid 2613 --home-dir /pumpkin --create-home pumpkin
COPY --from=builder /artifacts/pumpkin /usr/local/bin/pumpkin
COPY --from=builder /artifacts/kurbis-control.wasm /usr/local/share/kurbis-control.wasm
COPY runtime/pumpkin/hosted-entrypoint.sh /usr/local/bin/hosted-entrypoint
RUN test -x /usr/local/bin/hosted-entrypoint \
    && mkdir -p /usr/share/doc/kurbis \
    && printf '%s\n' "$BUILD_REVISION" > /usr/share/doc/kurbis/build-revision
COPY --from=builder /source/LICENSE /usr/share/doc/pumpkin/LICENSE
COPY runtime/pumpkin/REVISION /usr/share/doc/pumpkin/REVISION
WORKDIR /pumpkin
USER 2613:2613
EXPOSE 25565/tcp 19132/udp
ENTRYPOINT ["/usr/local/bin/hosted-entrypoint"]

FROM host-control AS waiting
ARG BUILD_REVISION=development
LABEL org.opencontainers.image.revision=${BUILD_REVISION}
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends netcat-openbsd \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /run/kurbis-gateway \
    && chown 2613:2613 /run/kurbis-gateway \
    && chmod 0770 /run/kurbis-gateway
COPY deployment/waiting.toml /pumpkin/pumpkin.toml
HEALTHCHECK --interval=5s --timeout=2s --start-period=30s --retries=12 \
    CMD printf '{"token":"%s","action":"status"}\n' "$PUMPKIN_HOST_CONTROL_TOKEN" | nc -U -w 1 /run/kurbis-gateway/host-control.sock | grep -q '\"ok\":true' || exit 1
USER 2613:2613
EXPOSE 19132/tcp
ENTRYPOINT ["/usr/local/bin/pumpkin"]
