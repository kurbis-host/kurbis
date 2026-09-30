#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME="$ROOT/runtime"
PUMPKIN="$RUNTIME/pumpkin"
SERVER="$RUNTIME/server"
REVISION="$(tr -d '[:space:]' < "$ROOT/pumpkin.revision")"
LOCAL_PUMPKIN="$(cd "$ROOT/../../Pumpkin" && pwd)"
PUMPKIN_PATCH="$ROOT/patches/pumpkin-hot-reload.patch"

rustup target add wasm32-wasip2

mkdir -p "$RUNTIME" "$SERVER/plugins"
if [[ ! -d "$PUMPKIN/.git" ]]; then
  git clone --no-hardlinks "$LOCAL_PUMPKIN" "$PUMPKIN"
fi
if ! git -C "$PUMPKIN" cat-file -e "$REVISION^{commit}" 2>/dev/null; then
  git -C "$PUMPKIN" fetch origin "$REVISION" --depth=1
fi
if [[ "$(git -C "$PUMPKIN" rev-parse HEAD)" != "$REVISION" ]]; then
  git -C "$PUMPKIN" checkout --detach "$REVISION"
fi
if ! git -C "$PUMPKIN" diff --quiet || ! git -C "$PUMPKIN" diff --cached --quiet; then
  git -C "$PUMPKIN" reset --hard "$REVISION"
fi
git -C "$PUMPKIN" clean -fd
git -C "$PUMPKIN" apply --check "$PUMPKIN_PATCH"
git -C "$PUMPKIN" apply "$PUMPKIN_PATCH"

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
hot_reload = true
ask_permission_confirmation = false
allow_unsigned = true
verify_signatures = false
inherit_env = false
loopback_only = true
max_memory_mb = 256

[plugins.overrides.vibe-bridge]
enabled = true
allow_unsigned = true
max_memory_mb = 128
allowed_permissions = ["fs.write.data"]
blocked_permissions = []
loopback_only = true
TOML

cargo build --manifest-path "$PUMPKIN/Cargo.toml" -p pumpkin
cargo build --manifest-path "$ROOT/vibe-bridge/Cargo.toml" --release --target wasm32-wasip2
cp "$ROOT/vibe-bridge/target/wasm32-wasip2/release/vibe_bridge.wasm" \
  "$SERVER/plugins/vibe-bridge.wasm"
cargo build --manifest-path "$ROOT/Cargo.toml" -p vibe-controller

echo "Bootstrap complete."
echo "Run: $ROOT/scripts/run.sh"
