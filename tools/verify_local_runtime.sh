#!/bin/sh
# Run Linux hard-quota verification entirely inside this Mac's Docker Desktop.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
if [ "$(uname -s)" != Darwin ]; then
    echo 'This wrapper requires macOS Docker Desktop; use verify_worker_runtime.py directly on Linux.' >&2
    exit 1
fi
endpoint=$(docker context inspect --format '{{.Endpoints.docker.Host}}')
case "$endpoint" in
    unix://*) ;;
    *) echo 'Refusing a remote Docker endpoint. Select the local Docker Desktop context.' >&2; exit 1 ;;
esac
if [ "$(docker info --format '{{.OperatingSystem}}')" != 'Docker Desktop' ]; then
    echo 'Select the local Docker Desktop context before running this verification.' >&2
    exit 1
fi
docker image inspect melon-hoster:local melon-pumpkin:host-control > /dev/null
docker build -f runtime/verification/offline-pumpkin/Dockerfile -t melon-pumpkin:offline-verification .
docker build -f runtime/verification/Dockerfile -t melon-hoster:local-verification .
name="melon-local-verification-$(date +%s)-$$"
output="$root/output/runtime/$name"
mkdir -p "$output"
cleanup() {
    docker rm --force --volumes "$name" > /dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM
# An isolated inner daemon keeps worker bind paths and quota mounts in one Linux
# namespace. No host Docker socket, existing image store, or user volume is mounted.
docker create --name "$name" --privileged --cgroupns=host --memory=6g --memory-swap=6g --cpus=2 --interactive --mount "type=bind,src=$output,dst=/results" melon-hoster:local-verification > /dev/null
archive="$output/pumpkin-image.tar"
docker save --output "$archive" melon-pumpkin:host-control melon-pumpkin:offline-verification
# Use a regular file rather than a pipeline so save/start failures propagate.
set +e
docker start --attach --interactive "$name" < "$archive"
status=$?
set -e
rm -f "$archive"
echo "Verification artifacts: $output"
exit "$status"
