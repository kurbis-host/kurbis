#!/bin/sh
set -eu
mkdir -p /opt/melon-hoster-verification /results
dockerd --host=unix:///var/run/docker.sock --storage-driver=vfs > /results/docker.log 2>&1 &
daemon_pid=$!
trap 'kill "$daemon_pid" 2>/dev/null || true; wait "$daemon_pid" 2>/dev/null || true' EXIT INT TERM
attempt=0
until docker info > /dev/null 2>&1; do
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        cat /results/docker.log
        exit 1
    fi
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 60 ]; then
        cat /results/docker.log
        exit 1
    fi
    sleep 1
done
docker load
python3 /verification/verify_worker_runtime.py --bin-dir /usr/local/bin --pumpkin-image "${PUMPKIN_IMAGE:?PUMPKIN_IMAGE must name the offline Pumpkin image for this verification run}" --output /results
