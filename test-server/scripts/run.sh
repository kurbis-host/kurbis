#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME="$ROOT/runtime"
SERVER="$RUNTIME/server"
CONTROLLER="$ROOT/target/debug/vibe-controller"
PUMPKIN_BIN="$RUNTIME/bin/pumpkin"

if [[ ! -x "$CONTROLLER" || ! -x "$PUMPKIN_BIN" || ! -f "$SERVER/plugins/vibe-bridge.wasm" ]]; then
  "$ROOT/scripts/bootstrap.sh"
fi

"$ROOT/scripts/download-pumpkin.sh"

"$CONTROLLER" --runtime "$RUNTIME" &
CONTROLLER_PID=$!
cleanup() {
  kill "$CONTROLLER_PID" 2>/dev/null || true
  wait "$CONTROLLER_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

python3 "$ROOT/scripts/server_runner.py" --runtime "$RUNTIME" --binary "$PUMPKIN_BIN"
