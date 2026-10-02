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
python3 - "$waiting_root/pumpkin.toml" <<'PY'
from pathlib import Path
import sys

config = Path(sys.argv[1])
lines = config.read_text(encoding="utf-8").splitlines()
section = "[networking.bedrock.nethernet]"
if section not in lines:
    lines.extend(["", section, 'signal_address = "127.0.0.1:19134"'])
    config.write_text("\n".join(lines) + "\n", encoding="utf-8")
else:
    start = lines.index(section) + 1
    end = next((index for index in range(start, len(lines)) if lines[index].startswith("[")), len(lines))
    signal_line = next(
        (index for index in range(start, end) if lines[index].lstrip().startswith("signal_address =")),
        None,
    )
    if signal_line is None:
        lines.insert(start, 'signal_address = "127.0.0.1:19134"')
        config.write_text("\n".join(lines) + "\n", encoding="utf-8")
    elif lines[signal_line].strip() == 'signal_address = "0.0.0.0:19132"':
        lines[signal_line] = 'signal_address = "127.0.0.1:19134"'
        config.write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
export PUMPKIN_HOST_CONTROL_SOCKET=${WAITING_CONTROL_SOCKET:-"/tmp/kurbis-dev-waiting-$(id -u).sock"}
export PUMPKIN_HOST_CONTROL_TOKEN="$PUMPKIN_GATEWAY_CONTROL_TOKEN"
cd "$waiting_root"
exec "$binary"
