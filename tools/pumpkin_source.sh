#!/bin/sh
# Check out the pinned Pumpkin revision into DESTINATION and apply the Kurbis
# integration. Every from-source Pumpkin build prepares its tree through this
# script so the pin and the patch order live in one place.
set -eu
[ "$#" -eq 2 ] || { echo "Usage: $0 INTEGRATION_DIRECTORY DESTINATION" >&2; exit 2; }
integration=$(CDPATH= cd -- "$1" && pwd)
destination=$2
mkdir -p "$destination"
if [ ! -d "$destination/.git" ]; then
    git -C "$destination" init --quiet
    git -C "$destination" remote add origin https://github.com/Pumpkin-MC/Pumpkin.git
fi
git -C "$destination" fetch --quiet --depth=1 origin "$(cat "$integration/REVISION")"
# The destination is owned by the calling build and holds nothing else.
git -C "$destination" reset --quiet --hard FETCH_HEAD
git -C "$destination" clean --quiet -fdx
git -C "$destination" submodule update --quiet --init --recursive --depth=1
for patch in host-control snapshot-barrier; do
    git -C "$destination" apply --check "$integration/$patch.patch"
    git -C "$destination" apply "$integration/$patch.patch"
done
git -C "$destination" apply --unidiff-zero --check "$integration/nethernet-gateway.patch"
git -C "$destination" apply --unidiff-zero "$integration/nethernet-gateway.patch"
cp "$integration/host_control.rs" "$destination/crates/pumpkin-core/src/net/host_control.rs"
