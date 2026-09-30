FROM rust:1.98-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends clang cmake pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY runtime/pumpkin/REVISION /tmp/REVISION
RUN git init . && git remote add origin https://github.com/Pumpkin-MC/Pumpkin.git && git fetch --depth=1 origin "$(cat /tmp/REVISION)" && git checkout --detach FETCH_HEAD && git submodule update --init --recursive --depth=1
COPY runtime/pumpkin/host-control.patch /tmp/host-control.patch
RUN git apply --check /tmp/host-control.patch && git apply /tmp/host-control.patch
COPY runtime/pumpkin/host_control.rs crates/pumpkin/src/net/host_control.rs
ENV CARGO_BUILD_JOBS=1 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 RUSTUP_TOOLCHAIN=1.98.1
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git --mount=type=cache,target=/source/target cargo build --locked --release --package pumpkin --bin pumpkin && mkdir /artifacts && cp target/release/pumpkin /artifacts/pumpkin && strip /artifacts/pumpkin

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 libgcc-s1 && rm -rf /var/lib/apt/lists/* && groupadd --gid 2613 pumpkin && useradd --uid 2613 --gid 2613 --home-dir /pumpkin --create-home pumpkin
COPY --from=builder /artifacts/pumpkin /usr/local/bin/pumpkin
COPY --from=builder /source/LICENSE /usr/share/doc/pumpkin/LICENSE
COPY runtime/pumpkin/REVISION /usr/share/doc/pumpkin/REVISION
WORKDIR /pumpkin
USER 2613:2613
EXPOSE 25565/tcp 19132/udp
ENTRYPOINT ["/usr/local/bin/pumpkin"]
