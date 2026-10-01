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
revision=$(cat "$root/runtime/pumpkin/REVISION")
source_dir="$root/output/native-pumpkin/source"
mkdir -p "$source_dir"
if [ ! -d "$source_dir/.git" ]; then
    git -C "$source_dir" init
    git -C "$source_dir" remote add origin https://github.com/Pumpkin-MC/Pumpkin.git
fi
fingerprint=$(cat "$root/runtime/pumpkin/REVISION" \
    "$root/runtime/pumpkin/host-control.patch" \
    "$root/runtime/pumpkin/nethernet-gateway.patch" \
    "$root/runtime/pumpkin/host_control.rs" | shasum -a 256 | cut -d ' ' -f 1)
stamp="$root/output/native-pumpkin/source-fingerprint"
if [ ! -f "$stamp" ] || [ "$(cat "$stamp")" != "$fingerprint" ]; then
    git -C "$source_dir" fetch --depth=1 origin "$revision"
    # This checkout is exclusively managed by this build script in ignored output/.
    git -C "$source_dir" reset --hard FETCH_HEAD
    git -C "$source_dir" submodule update --init --recursive --depth=1
    git -C "$source_dir" apply --check "$root/runtime/pumpkin/host-control.patch"
    git -C "$source_dir" apply "$root/runtime/pumpkin/host-control.patch"
    git -C "$source_dir" apply --unidiff-zero --check "$root/runtime/pumpkin/nethernet-gateway.patch"
    git -C "$source_dir" apply --unidiff-zero "$root/runtime/pumpkin/nethernet-gateway.patch"
    cp "$root/runtime/pumpkin/host_control.rs" "$source_dir/crates/pumpkin/src/net/host_control.rs"
    printf '%s\n' "$fingerprint" > "$stamp"
fi
export CARGO_TARGET_DIR="$root/output/native-pumpkin/target"
export CARGO_BUILD_JOBS="${PUMPKIN_BUILD_JOBS:-4}"
# Native debug builds retain fast development compilation without huge DWARF.
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_DEV_CODEGEN_UNITS=16
export CARGO_PROFILE_DEV_INCREMENTAL=false
cd "$source_dir"
set --
if [ "$profile" = release ]; then
    set -- --release
fi
cargo +1.98.1 build "$@" --locked --package pumpkin --bin pumpkin
printf 'Native Pumpkin: %s/%s/pumpkin\n' "$CARGO_TARGET_DIR" "$cargo_profile"

rustup target add --toolchain 1.98.1 wasm32-wasip2
CARGO_TARGET_DIR="$root/plugins/kurbis-control/target" \
    cargo +1.98.1 build --locked --manifest-path "$root/plugins/kurbis-control/Cargo.toml" --target wasm32-wasip2 "$@"
printf 'Kurbis control plugin: %s/wasm32-wasip2/%s/kurbis_control.wasm\n' "$root/plugins/kurbis-control/target" "$cargo_profile"
