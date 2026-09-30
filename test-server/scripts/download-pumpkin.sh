#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RELEASE="$(tr -d '[:space:]' < "$ROOT/pumpkin.release")"
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) ASSET="pumpkin-ARM64-macOS" ;;
  Linux/aarch64|Linux/arm64) ASSET="pumpkin-ARM64-Linux-musl" ;;
  Linux/x86_64) ASSET="pumpkin-X64-Linux-musl" ;;
  *) echo "No pinned official Pumpkin binary for $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

BIN_DIR="$ROOT/runtime/bin"
mkdir -p "$BIN_DIR"
verify() {
  python3 - "$ROOT/pumpkin.checksums.sha256" "$ASSET" "$1" <<'PY'
import hashlib
import pathlib
import sys

checksums, asset, binary = sys.argv[1:]
expected = dict((name, digest) for digest, name in
                (line.split() for line in pathlib.Path(checksums).read_text().splitlines()))[asset]
with open(binary, "rb") as stream:
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(chunk)
    actual = digest.hexdigest()
if actual != expected:
    sys.exit(f"SHA-256 mismatch for {asset}: expected {expected}, got {actual}")
PY
}
if [[ -f "$BIN_DIR/pumpkin" ]] && verify "$BIN_DIR/pumpkin"; then
  chmod +x "$BIN_DIR/pumpkin"
  exit 0
fi

TEMP="$(mktemp "$BIN_DIR/.pumpkin.XXXXXX")"
trap 'rm -f "$TEMP"' EXIT
curl --fail --location --retry 3 \
  "https://github.com/Pumpkin-MC/Pumpkin/releases/download/$RELEASE/$ASSET" \
  --output "$TEMP"
verify "$TEMP"
chmod +x "$TEMP"
mv "$TEMP" "$BIN_DIR/pumpkin"
