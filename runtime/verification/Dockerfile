FROM node:22-bookworm-slim AS protocol
WORKDIR /verification/protocol
COPY tools/protocol/package.json tools/protocol/package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY tools/protocol/verify.mjs tools/protocol/verify_java_263.py ./

FROM melon-hoster:local AS hoster
FROM docker:28-dind AS docker

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends python3 libstdc++6 libatomic1 ca-certificates iptables e2fsprogs util-linux xz-utils && rm -rf /var/lib/apt/lists/*
COPY --from=docker /usr/local/bin/docker /usr/local/bin/dockerd /usr/local/bin/containerd /usr/local/bin/containerd-shim-runc-v2 /usr/local/bin/ctr /usr/local/bin/runc /usr/local/bin/docker-init /usr/local/bin/docker-proxy /usr/local/bin/
COPY --from=protocol /usr/local/bin/node /usr/local/bin/node
COPY --from=protocol /verification/protocol /verification/protocol
COPY --from=hoster /usr/local/bin/control-plane /usr/local/bin/gateway /usr/local/bin/worker /usr/local/bin/
COPY tools/verify_worker_runtime.py tools/verify_host_control.py /verification/
COPY runtime/verification/entrypoint.sh /verification/entrypoint.sh
ENV DOCKER_HOST=unix:///var/run/docker.sock
ENTRYPOINT ["/bin/sh", "/verification/entrypoint.sh"]
