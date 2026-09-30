#!/bin/sh
# Authenticated waiting runtime for the local gateway; no Docker or test fixture.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
: "${PUMPKIN_GATEWAY_CONTROL_TOKEN:?Set a random token of at least 32 bytes}"
binary=${PUMPKIN_BINARY:-"$root/output/native-pumpkin/target/debug/pumpkin"}
case "$binary" in /*) ;; *) binary="$PWD/$binary" ;; esac
if [ ! -x "$binary" ]; then
    echo "Build the native runtime with tools/build_pumpkin_native.sh first" >&2
    exit 1
fi
waiting_root=${NATIVE_WAITING_ROOT:-"$root/data/native-waiting"}
mkdir -p "$waiting_root"
if [ ! -f "$waiting_root/pumpkin.toml" ]; then
    cp "$root/examples/config/native-waiting.toml" "$waiting_root/pumpkin.toml"
fi
export PUMPKIN_HOST_CONTROL_SOCKET=${WAITING_CONTROL_SOCKET:-"/tmp/kurbis-dev-waiting-$(id -u).sock"}
export PUMPKIN_HOST_CONTROL_TOKEN="$PUMPKIN_GATEWAY_CONTROL_TOKEN"
cd "$waiting_root"
exec "$binary"
