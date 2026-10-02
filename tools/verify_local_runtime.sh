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
tag=$(uuidgen | tr -d '-' | tr '[:upper:]' '[:lower:]')
case "$tag" in
    ''|*[!0-9a-f]*) echo 'Could not create a unique lowercase Docker tag.' >&2; exit 1 ;;
esac
if [ "${#tag}" -ne 32 ]; then
    echo 'Could not create a full UUID for this Docker verification run.' >&2
    exit 1
fi
name="melon-local-verification-$tag"
release_image="melon-hoster:release-check-$tag"
pumpkin_image="melon-pumpkin:release-check-$tag"
offline_image="melon-pumpkin:offline-check-$tag"
offline_verification_image="melon-pumpkin:offline-verification-$tag"
verification_image="melon-hoster:verification-$tag"
output="$root/output/runtime/$name"
mkdir -p "$root/output/runtime"
if ! mkdir "$output"; then
    echo "Verification output already exists: $output" >&2
    exit 1
fi
for image in "$release_image" "$pumpkin_image" "$offline_image" "$offline_verification_image" "$verification_image"; do
    if docker image inspect "$image" > /dev/null 2>&1; then
        echo "Refusing to overwrite existing Docker image: $image" >&2
        exit 1
    fi
done
if docker container inspect "$name" > /dev/null 2>&1; then
    echo "Refusing to overwrite existing Docker container: $name" >&2
    exit 1
fi
cleanup() {
    docker rm --force --volumes "$name" > /dev/null 2>&1 || true
    docker image rm "$verification_image" "$offline_verification_image" "$offline_image" "$pumpkin_image" "$release_image" > /dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM
docker build -f runtime/Dockerfile -t "$release_image" .
docker build -f runtime/pumpkin/Dockerfile -t "$pumpkin_image" .
docker build -f runtime/verification/offline-pumpkin/Dockerfile \
    --build-arg "PUMPKIN_BASE_IMAGE=$pumpkin_image" -t "$offline_image" .
docker tag "$offline_image" "$offline_verification_image"
docker build -f runtime/verification/Dockerfile -t "$verification_image" .
# An isolated inner daemon keeps worker bind paths and quota mounts in one Linux
# namespace. No host Docker socket, existing image store, or user volume is mounted.
docker create --name "$name" --privileged --cgroupns=host --memory=6g --memory-swap=6g --cpus=2 --interactive --env "PUMPKIN_IMAGE=$offline_verification_image" --mount "type=bind,src=$output,dst=/results" "$verification_image" > /dev/null
archive="$output/pumpkin-image.tar"
docker save --output "$archive" "$pumpkin_image" "$offline_image" "$offline_verification_image"
# Use a regular file rather than a pipeline so save/start failures propagate.
set +e
docker start --attach --interactive "$name" < "$archive"
status=$?
set -e
rm -f "$archive"
echo "Verification artifacts: $output"
exit "$status"
