#!/usr/bin/env bash
# Build one image through BuildKit with per-RUN cgroup caps and host admission.
set -euo pipefail
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [[ $# -lt 1 ]]; then
  echo "usage: $0 [docker buildx build options and context]" >&2
  exit 2
fi
: "${KURBIS_BUILD_MEMORY_BYTES:=10737418240}" # 10 GiB
: "${KURBIS_BUILD_SYSTEM_RESERVE_BYTES:=4294967296}" # 4 GiB
: "${KURBIS_BUILD_RESOURCE_PROBE_IMAGE:=rust:1.98-bookworm}"
: "${KURBIS_BUILD_OUTPUT_MODE:=load}"
case "$KURBIS_BUILD_OUTPUT_MODE" in
  load) output_mode=(--load) ;;
  push) output_mode=(--push) ;;
  none) output_mode=() ;;
  *) echo "KURBIS_BUILD_OUTPUT_MODE must be load, push, or none" >&2; exit 2 ;;
esac
exec python3 "$root/tools/run_bounded_build.py" \
  --memory-bytes "$KURBIS_BUILD_MEMORY_BYTES" \
  --reserve-bytes "$KURBIS_BUILD_SYSTEM_RESERVE_BYTES" \
  --wait-seconds 600 \
  --probe-image "$KURBIS_BUILD_RESOURCE_PROBE_IMAGE" \
  -- env BUILDKIT_PROGRESS=plain docker buildx build \
    --resource "memory=$KURBIS_BUILD_MEMORY_BYTES" \
    --resource "memory-swap=$KURBIS_BUILD_MEMORY_BYTES" \
    --resource cpu-period=100000 \
    --resource cpu-quota=200000 \
    --load "$@"
