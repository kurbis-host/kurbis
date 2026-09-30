#!/bin/sh
set -eu
# The disposable harness supplies config before start. Disable external identity
# providers only in this derived test image, retaining real packet/crypto flows.
awk '
/^\[/ { section=$0 }
/^\[networking.java\]/ { print; print "online_mode = false\nencryption = false"; next }
/^\[networking.bedrock\]/ { print; print "online_mode = false"; next }
(section == "[networking.java]" || section == "[networking.bedrock]") && /^(online_mode|encryption) *=/ { next }
{ print }
' pumpkin.toml > pumpkin.toml.fixture
mv pumpkin.toml.fixture pumpkin.toml
# A fixed disposable offline player may issue real gameplay commands. Existing
# operator/inventory state is retained across snapshots and worker reassignment.
mkdir -p data
if [ ! -f data/ops.json ]; then
    cat > data/ops.json <<'JSON'
[{"uuid":"63afe389-0d98-f833-4096-cc01dd62a9f0","name":"RuntimePlayer","level":4,"bypasses_player_limit":true}]
JSON
fi
exec /usr/local/bin/pumpkin
