#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT/.." && pwd)"
RUNTIME="$ROOT/runtime"
PUMPKIN="$RUNTIME/pumpkin"
SERVER="$RUNTIME/server"
REVISION="$(tr -d '[:space:]' < "$ROOT/pumpkin.revision")"
RELEASE="$(tr -d '[:space:]' < "$ROOT/pumpkin.release")"

rustup target add wasm32-wasip2

mkdir -p "$RUNTIME" "$SERVER/plugins"
python3 - "$SERVER/plugins" <<'PY_CLEANUP'
import pathlib
import shutil
import sys

plugins = pathlib.Path(sys.argv[1])
runtime = plugins.parents[1]
# Remove only obsolete Vibe Bridge state. Generated Kurbis AI plugin workspaces
# and archived artifacts are intentionally preserved across bootstrap runs.
(plugins / "vibe-bridge.wasm").unlink(missing_ok=True)
shutil.rmtree(plugins / "data/vibe-bridge", ignore_errors=True)
for legacy_log in (runtime / "workspace/plugins").glob("**/.vibe-codex.log"):
    legacy_log.unlink(missing_ok=True)
PY_CLEANUP
# This checkout supplies the plugin SDK only. The server runs the official binary.
if [[ ! -d "$PUMPKIN/.git" ]]; then
  git clone --depth=1 --branch "$RELEASE" https://github.com/Pumpkin-MC/Pumpkin.git "$PUMPKIN"
fi
if ! git -C "$PUMPKIN" cat-file -e "$REVISION^{commit}" 2>/dev/null; then
  git -C "$PUMPKIN" fetch origin "$REVISION" --depth=1
fi
if ! git -C "$PUMPKIN" diff --quiet || ! git -C "$PUMPKIN" diff --cached --quiet; then
  echo "Plugin SDK checkout has local changes: $PUMPKIN" >&2
  exit 1
fi
git -C "$PUMPKIN" checkout --detach "$REVISION"
"$ROOT/scripts/download-pumpkin.sh"

cat > "$SERVER/pumpkin.toml" <<'TOML'
allow_chat_reports = false

[networking.java]
enabled = true
address = "127.0.0.1:25565"
encryption = false
online_mode = false

[networking.bedrock]
enabled = false

[plugins]
enabled = true
hot_reload = false
ask_permission_confirmation = false
allow_unsigned = true
verify_signatures = false
inherit_env = false
loopback_only = true
max_memory_mb = 256

[plugins.overrides.kurbis-ai]
enabled = true
allow_unsigned = true
max_memory_mb = 128
allowed_permissions = ["fs.read.data", "fs.write.data"]
blocked_permissions = []
loopback_only = true
TOML

python3 "$ROOT/../tools/run_bounded_build.py" --memory-bytes "${KURBIS_NATIVE_BUILD_MEMORY_BYTES:-10737418240}" \
  -- cargo build --jobs 1 --manifest-path "$REPO_ROOT/plugins/kurbis-ai/Cargo.toml" \
     --target-dir "$REPO_ROOT/target/kurbis-ai-plugin" --release --target wasm32-wasip2
cp "$REPO_ROOT/target/kurbis-ai-plugin/wasm32-wasip2/release/kurbis_ai.wasm" \
  "$SERVER/plugins/kurbis-ai.wasm"
python3 "$ROOT/../tools/run_bounded_build.py" --memory-bytes "${KURBIS_NATIVE_BUILD_MEMORY_BYTES:-10737418240}" \
  -- cargo build --jobs 1 --manifest-path "$ROOT/Cargo.toml" -p kurbis-ai-controller

echo "Bootstrap complete."
echo "Run: $ROOT/scripts/run.sh"
