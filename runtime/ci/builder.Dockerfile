# syntax=docker/dockerfile:1.7
# Trusted, host-owned CI builder image. Never build or consume a Dockerfile from
# a Pumpkin commit. The resulting image must be published and referenced by its
# registry sha256 digest in CI_BUILDER_IMAGE.
ARG RUST_TOOLCHAIN=1.98.1
FROM rust:1.98-bookworm
ARG RUST_TOOLCHAIN
ARG PUMPKIN_REVISION=8ea2b7d1c81e977443de85d60a54155a43e9453b
ARG SCCACHE_VERSION=0.10.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils ca-certificates clang cmake git libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && rustup toolchain install "${RUST_TOOLCHAIN}" --profile minimal \
    && rustup default "${RUST_TOOLCHAIN}" \
    && rustup target add wasm32-wasip2

# sccache is optional at runtime but is part of the trusted image so every
# build uses the same pinned compiler-cache implementation. The runtime only
# enables its local disk backend; no remote cache credentials or network are
# ever present in a build container.
RUN --mount=type=cache,id=kurbis-ci-builder-cargo-registry-${RUST_TOOLCHAIN},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=kurbis-ci-builder-cargo-git-${RUST_TOOLCHAIN},target=/usr/local/cargo/git,sharing=locked \
    cargo install --locked --version "${SCCACHE_VERSION}" sccache --root /opt/sccache \
    && install -m 0755 /opt/sccache/bin/sccache /usr/local/bin/sccache \
    && rm -rf /opt/sccache

ENV CARGO_HOME=/usr/local/cargo \
    CARGO_NET_GIT_FETCH_WITH_CLI=true \
    RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}

# Build the source preprocessor from the same checked-in hoster/patch set. It
# executes only inside the network-disabled rootless builder container; no
# fetched source is extracted or patched on the control-plane host.
WORKDIR /hoster
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY runtime/pumpkin ./runtime/pumpkin
COPY plugins/kurbis-control ./plugins/kurbis-control
# Keep the BuildKit-mounted /hoster/target cache populated for warm image builds.
RUN --mount=type=cache,id=kurbis-ci-builder-cargo-registry-${RUST_TOOLCHAIN},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=kurbis-ci-builder-cargo-git-${RUST_TOOLCHAIN},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=kurbis-ci-builder-hoster-target-${RUST_TOOLCHAIN},target=/hoster/target,sharing=locked \
    cargo build --jobs 1 --locked --release --bin ci-source-preparer \
    && install -D -m 0755 target/release/ci-source-preparer /usr/local/bin/ci-source-preparer \
    && find /hoster -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf -- {} +

# Seed only the cargo package/git cache from the known upstream baseline and
# the checked-in trusted control plugin. Runtime compilation remains --offline.
RUN git init /tmp/pumpkin-seed \
    && git -C /tmp/pumpkin-seed remote add origin https://github.com/Pumpkin-MC/Pumpkin.git \
    && git -C /tmp/pumpkin-seed fetch --depth=1 origin "${PUMPKIN_REVISION}" \
    && git -C /tmp/pumpkin-seed checkout --detach FETCH_HEAD \
    && git -C /tmp/pumpkin-seed submodule update --init --recursive --depth=1 \
    && cd /tmp/pumpkin-seed \
    && cargo fetch --locked --target x86_64-unknown-linux-gnu \
    && cargo fetch --locked --target aarch64-unknown-linux-gnu \
    && cargo fetch --locked --target wasm32-wasip2

COPY plugins/kurbis-control/Cargo.toml plugins/kurbis-control/Cargo.lock /tmp/kurbis-control/
COPY plugins/kurbis-control/src /tmp/kurbis-control/src
RUN cd /tmp/kurbis-control \
    && cargo fetch --locked --manifest-path Cargo.toml --target wasm32-wasip2

RUN rm -rf /tmp/pumpkin-seed /tmp/kurbis-control \
    && chmod -R a+rX /usr/local/cargo/registry
WORKDIR /work
ENTRYPOINT ["/bin/sh"]
