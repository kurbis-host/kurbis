FROM node:22-bookworm-slim AS protocol
WORKDIR /verification/protocol
COPY tools/protocol/package.json tools/protocol/package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY tools/protocol/verify.mjs tools/protocol/verify_java_263.py ./

FROM rust:1.98-bookworm AS hoster
ENV CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_CODEGEN_UNITS=1 CARGO_PROFILE_DEV_DEBUG=0
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git --mount=type=cache,target=/source/target \
    cargo build --jobs 1 --locked --bins && mkdir /artifacts && \
    cp target/debug/control-plane target/debug/gateway target/debug/worker target/debug/worker-bundle target/debug/api-openapi target/debug/runtime-api-proxy /artifacts/
FROM docker:28-dind AS docker

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends python3 libstdc++6 libatomic1 ca-certificates iptables e2fsprogs util-linux xz-utils && rm -rf /var/lib/apt/lists/*
COPY --from=docker /usr/local/bin/docker /usr/local/bin/dockerd /usr/local/bin/containerd /usr/local/bin/containerd-shim-runc-v2 /usr/local/bin/ctr /usr/local/bin/runc /usr/local/bin/docker-init /usr/local/bin/docker-proxy /usr/local/bin/
COPY --from=protocol /usr/local/bin/node /usr/local/bin/node
COPY --from=protocol /verification/protocol /verification/protocol
COPY --from=hoster /artifacts/control-plane /artifacts/gateway /artifacts/worker /artifacts/worker-bundle /artifacts/api-openapi /artifacts/runtime-api-proxy /usr/local/bin/
COPY tools/verify_worker_runtime.py tools/verify_host_control.py \
    tools/runtime_verification_contract.py tools/runtime_verification_protocol.py \
    tools/runtime_verification_auth.py tools/runtime_verification_access.py /verification/
COPY runtime/verification/entrypoint.sh /verification/entrypoint.sh
ENV DOCKER_HOST=unix:///var/run/docker.sock
ENTRYPOINT ["/bin/sh", "/verification/entrypoint.sh"]
