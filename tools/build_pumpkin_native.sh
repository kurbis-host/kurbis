#!/bin/sh
# Build the same pinned integration as the container, entirely on this host.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
profile=${1:-debug}
case "$profile" in
    debug) cargo_profile=debug ;;
    release) cargo_profile=release ;;
    *) echo "Usage: $0 [debug|release]" >&2; exit 2 ;;
esac
source_dir="$root/output/native-pumpkin/source"
fingerprint=$(cat "$root/runtime/pumpkin/REVISION" \
    "$root/runtime/pumpkin/host-control.patch" \
    "$root/runtime/pumpkin/snapshot-barrier.patch" \
    "$root/runtime/pumpkin/nethernet-gateway.patch" \
    "$root/runtime/pumpkin/host_control.rs" | shasum -a 256 | cut -d ' ' -f 1)
stamp="$root/output/native-pumpkin/source-fingerprint"
if [ ! -f "$stamp" ] || [ "$(cat "$stamp")" != "$fingerprint" ]; then
    "$root/tools/pumpkin_source.sh" "$root/runtime/pumpkin" "$source_dir"
    printf '%s\n' "$fingerprint" > "$stamp"
fi
export CARGO_TARGET_DIR="$root/output/native-pumpkin/target"
export CARGO_BUILD_JOBS=1
# Native debug builds retain fast development compilation without huge DWARF.
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_DEV_CODEGEN_UNITS=1
export CARGO_PROFILE_DEV_INCREMENTAL=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
cd "$source_dir"
set --
if [ "$profile" = release ]; then
    set -- --release
fi
python3 "$root/tools/run_bounded_build.py" \
    --memory-bytes "${KURBIS_NATIVE_BUILD_MEMORY_BYTES:-10737418240}" \
    -- python3 -c 'import os,sys; os.execvp(sys.argv[1], sys.argv[1:])' \
    cargo +1.98.1 build --jobs 1 "$@" --locked --package pumpkin --bin pumpkin
printf 'Native Pumpkin: %s/%s/pumpkin\n' "$CARGO_TARGET_DIR" "$cargo_profile"

rustup target add --toolchain 1.98.1 wasm32-wasip2
python3 "$root/tools/run_bounded_build.py" \
    --memory-bytes "${KURBIS_NATIVE_BUILD_MEMORY_BYTES:-10737418240}" \
    -- env "CARGO_TARGET_DIR=$root/plugins/kurbis-control/target" \
    cargo +1.98.1 build --jobs 1 --locked --manifest-path "$root/plugins/kurbis-control/Cargo.toml" --target wasm32-wasip2 "$@"
printf 'Kurbis control plugin: %s/wasm32-wasip2/%s/kurbis_control.wasm\n' "$root/plugins/kurbis-control/target" "$cargo_profile"
