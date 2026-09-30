#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME="$ROOT/runtime"
SERVER="$RUNTIME/server"
PUMPKIN="$RUNTIME/pumpkin"
CONTROLLER="$ROOT/target/debug/vibe-controller"
PUMPKIN_BIN="$PUMPKIN/target/debug/pumpkin"

if [[ ! -x "$CONTROLLER" || ! -x "$PUMPKIN_BIN" || ! -f "$SERVER/plugins/vibe-bridge.wasm" ]]; then
  "$ROOT/scripts/bootstrap.sh"
fi

"$CONTROLLER" --runtime "$RUNTIME" &
CONTROLLER_PID=$!
cleanup() {
  kill "$CONTROLLER_PID" 2>/dev/null || true
  wait "$CONTROLLER_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cd "$SERVER"
"$PUMPKIN_BIN"
