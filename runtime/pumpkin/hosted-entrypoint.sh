#!/bin/sh
set -eu

mkdir -p /pumpkin/plugins
install -m 0644 /usr/local/share/kurbis-control.wasm /pumpkin/plugins/kurbis-control.wasm
exec /usr/local/bin/pumpkin "$@"
